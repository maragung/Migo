package com.migo.core.session

import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.Envelope
import com.migo.core.crypto.IdentitySecret
import com.migo.core.crypto.InitialMessage
import com.migo.core.crypto.KeyPair
import com.migo.core.crypto.Preamble
import com.migo.core.crypto.PrekeyBundle
import com.migo.core.crypto.RatchetSession
import com.migo.core.crypto.SCHEME_DOUBLE_RATCHET
import com.migo.core.crypto.SCHEME_DOUBLE_RATCHET_PREKEY
import com.migo.core.crypto.X3dh
import com.migo.core.wire.Id
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * The per-device Double Ratchet store for pairwise traffic.
 *
 * One instance per signed-in device, shared across every conversation, keyed by
 * `(conversationId, remoteDeviceId)`. A direct conversation has one peer, but that peer may be signed
 * in on a phone and a laptop, and each device is a separate ratchet with its own identity -- which is
 * what makes a compromised phone unable to read what the laptop received.
 *
 * This is the Kotlin port of `packages/sdk/src/session-crypto.ts` and mirrors it decision for
 * decision, because the envelopes it produces are opened by that code and by
 * `clients/desktop/src/crypto/session.rs`. Any behavioural difference here is a message that does not
 * decrypt on another platform, so where this file deviates it says so and why.
 *
 * # What this layer actually carries
 *
 * Not message content. Content is sealed once under a sender key -- see [GroupCrypto] -- so that a
 * message costs one ciphertext regardless of how many devices are in the conversation. What travels
 * pairwise through *this* layer is the sender-key distribution: the small block that tells one
 * specific device how to open the sender-key chain. That is the design the web client and the desktop
 * client both implement, and it is why a group of twenty devices does not mean twenty copies of every
 * photo.
 *
 * # Two deviations from the TypeScript reference, both required by the platform
 *
 * **A mutex.** The reference runs on a single-threaded event loop, so its map needs no guarding. Here
 * two coroutines can call [seal] for the same device at the same time -- two conversations opening at
 * once on a cold start is the ordinary case -- and without a lock both would find no session, both
 * would fetch a bundle, both would run X3DH, and the second would overwrite the first. The peer would
 * then hold a ratchet for one of them and be unable to open anything sent through the other. The lock
 * is held across the bundle fetch on purpose: releasing it to await the network is exactly what would
 * let the second caller through.
 *
 * **A persistence seam.** The reference keeps sessions in memory because a browser tab that goes away
 * is a user who closed it. Android kills processes in the background as a matter of routine, and a
 * ratchet that does not survive that is a conversation that stops decrypting for no reason the user
 * can see. [SessionPersistence] is how the sealed store is layered underneath without this class
 * knowing anything about files or key wrapping; the default does nothing, which keeps the in-memory
 * behaviour of the reference available for tests.
 */
class SessionCrypto(
    private val keys: LocalKeyStore,
    private val bundles: PeerBundleSource,
    private val persistence: SessionPersistence = SessionPersistence.None,
) {
    private val lock = Mutex()
    private val sessions = HashMap<String, SessionEntry>()

    /**
     * Whether a session already exists for a conversation's remote device.
     *
     * Consults the persistent store as well as memory, because "we have no session" is the condition
     * that triggers a bundle fetch and an X3DH run, and answering it from memory alone after a process
     * restart would burn one of the peer's one-time prekeys to rebuild a session already on disk.
     */
    suspend fun hasSession(conversationId: Id, deviceId: Id): Boolean = lock.withLock {
        entryOrNull(conversationId, deviceId) != null
    }

    /**
     * Seals [plaintext] for one remote device, establishing a session first if there is none.
     *
     * The first message to a device fetches that device's prekey bundle, runs X3DH as the initiator,
     * and emits a [SCHEME_DOUBLE_RATCHET_PREKEY] envelope carrying the material the peer needs to
     * answer. Messages after that stay in the prekey scheme until the peer replies, then switch to the
     * plain [SCHEME_DOUBLE_RATCHET] form -- the standard "keep sending prekey messages until
     * acknowledged" rule, because until we successfully open something from the peer we have no
     * evidence they ever received the first one.
     */
    suspend fun seal(
        conversationId: Id,
        peerUserId: Id,
        peerDeviceId: Id,
        plaintext: ByteArray,
    ): SealedEnvelope = lock.withLock {
        val key = sessionKey(conversationId, peerDeviceId)
        var entry = entryOrNull(conversationId, peerDeviceId)

        if (entry == null) {
            // Become the initiator: verify and consume a prekey bundle, then seed a ratchet from it.
            // `initiate` verifies the signed prekey's signature before it derives anything, so a
            // bundle the server substituted fails here rather than producing a session the peer
            // cannot open.
            val bundle = bundles.fetchBundle(peerUserId, peerDeviceId)
            val initiation = X3dh.initiate(keys.identity(), bundle)
            // A fresh ratchet pair, not `initiation.ephemeral`. Both references do the same -- the
            // Rust one discards it as `_ephemeral` -- and it is not an oversight: the responder
            // derives its first step from whatever ratchet key the header advertises, so reusing the
            // X3DH ephemeral buys nothing and gives one key two jobs. The field exists so a test
            // vector can pin the ephemeral, not so a session can be built from it.
            val session = RatchetSession.initiator(
                initiation.seed,
                bundle.signedPrekey.publicKey,
            )
            // `exposeSharedSecret` hands out a copy, so the seed still holds a live secret that
            // nothing else will ever zero. Rust gets this from `Drop`; here it is a call.
            initiation.seed.destroy()
            entry = SessionEntry(session, initiation.message)
            sessions[key] = entry
        }

        val message = entry.session.encryptNext(plaintext)
        val pending = entry.pendingInit
        val envelope = if (pending != null) {
            Envelope.initial(preambleOf(pending), message.header, message.ciphertext).encode()
        } else {
            Envelope.established(message.header, message.ciphertext).encode()
        }

        // The ratchet advanced, so the new state has to reach disk before the envelope reaches the
        // network. The other order loses: a process killed between the send and the save would leave
        // the peer holding a chain step this device has forgotten, and every later message would fail.
        persistence.save(conversationId, peerDeviceId, entry.session)

        SealedEnvelope(
            scheme = if (pending != null) SCHEME_DOUBLE_RATCHET_PREKEY else SCHEME_DOUBLE_RATCHET,
            senderKeyId = 0L,
            envelope = envelope,
        )
    }

    /**
     * Opens an envelope from one remote device, establishing a responder session if it is a first
     * message and none exists yet.
     *
     * A [SCHEME_DOUBLE_RATCHET_PREKEY] envelope with no existing session runs X3DH as the responder. A
     * prekey envelope for a device we already have a session with is a resend the initiator made
     * before hearing back; it decrypts in the session we already hold. A plain
     * [SCHEME_DOUBLE_RATCHET] envelope requires an existing session.
     *
     * `senderUserId` is deliberately unused for anything cryptographic: the initiator's identity comes
     * from the envelope's own X3DH material, which is bound into the associated data of every message
     * in the session. Trusting the frame's sender field instead would mean trusting the server about
     * who sent something, which is the one thing the protocol is built not to do. It stays in the
     * signature because a caller that had to look it up separately would be a caller that could get it
     * wrong.
     *
     * # Commit only on success
     *
     * Establishing a responder session mutates two things that must not be spent on a message that is
     * not ours: the session slot for this sender device, and a one-time prekey. Because a distribution
     * goes out to every device in a conversation, a first message pairwise-sealed for a *different*
     * device also arrives here, decodes as a well-formed prekey envelope, and would -- if committed
     * eagerly -- plant a bogus session in this slot so the real distribution could never open, and
     * burn a prekey doing it. So the responder session is derived locally and the decrypt is attempted
     * before anything is stored: a foreign envelope fails the AEAD tag, throws, and leaves the store
     * untouched, exactly as the ratchet's own anti-denial-of-service rule leaves an established
     * session untouched on a bad message.
     */
    suspend fun open(
        conversationId: Id,
        senderUserId: Id,
        senderDeviceId: Id,
        envelope: ByteArray,
    ): ByteArray = lock.withLock {
        // `senderUserId` is intentionally unread here; see the note above. `Envelope.decode`
        // already refuses a sender-key envelope and an unknown scheme, so there is
        // no scheme check here: adding a second one would be a second place to keep in step.
        val parsed = Envelope.decode(envelope)
        val key = sessionKey(conversationId, senderDeviceId)
        val existing = entryOrNull(conversationId, senderDeviceId)

        if (existing != null) {
            // An established session: the ratchet does not mutate itself when a decrypt fails, so a
            // resent prekey preamble or a foreign envelope landing on this slot cannot corrupt it.
            val plaintext = existing.session.decrypt(parsed.header, parsed.ciphertext)
            // We have now heard from the peer, so they hold a working session; stop re-sending X3DH.
            existing.pendingInit = null
            persistence.save(conversationId, senderDeviceId, existing.session)
            return@withLock plaintext
        }

        val preamble = parsed.preamble
            // A ratchet message with no session and no X3DH material: the first message was lost, and
            // this one cannot bootstrap a session on its own. The peer has to send a fresh prekey
            // message, which it will, because it never saw a reply.
            ?: throw CryptoError.noSession()

        val derived = deriveResponder(preamble)
        val plaintext = derived.session.decrypt(parsed.header, parsed.ciphertext)

        // Success: this message was ours. Now -- and only now -- consume the prekey and keep the
        // session.
        derived.oneTimePrekeyId?.let { keys.consumeOneTimePrekey(it) }
        val committed = SessionEntry(derived.session, null)
        sessions[key] = committed
        persistence.save(conversationId, senderDeviceId, committed.session)
        plaintext
    }

    /**
     * Forgets sessions, so the next message re-runs X3DH.
     *
     * With [deviceId], forgets that one device's session in the conversation; without it, every
     * device's. Use it when leaving a conversation, and when a peer's identity key changes -- brief
     * section 155 requires that be surfaced as a visible warning rather than accepted silently, and
     * the sessions built on the old identity must not be reused, because a session is only as
     * meaningful as the identity it was bound to.
     */
    suspend fun forget(conversationId: Id, deviceId: Id? = null) {
        lock.withLock {
            if (deviceId != null) {
                sessions.remove(sessionKey(conversationId, deviceId))
                persistence.delete(conversationId, deviceId)
                return@withLock
            }
            val prefix = "${conversationId.value}|"
            sessions.keys.removeAll { it.startsWith(prefix) }
            persistence.deleteConversation(conversationId)
        }
    }

    /**
     * The live entry for a device, hydrating it from the persistent store on a first touch.
     *
     * A session restored from disk has no pending X3DH material, and that is correct rather than a
     * lossy shortcut: a session only reaches the store after a successful seal or open, and the
     * initiator clears its pending material on the first open anyway. The cost of getting it wrong in
     * this direction is one redundant prekey preamble on one message; the cost in the other direction
     * is a peer that never learns the session exists.
     *
     * Must be called with [lock] held.
     */
    private fun entryOrNull(conversationId: Id, deviceId: Id): SessionEntry? {
        val key = sessionKey(conversationId, deviceId)
        sessions[key]?.let { return it }
        val restored = persistence.load(conversationId, deviceId) ?: return null
        val entry = SessionEntry(restored, null)
        sessions[key] = entry
        return entry
    }

    /**
     * Derives a responder session for a first message *without committing* it.
     *
     * Returns the session and the one-time prekey id the caller must consume once the decrypt has
     * proved the message was for us. Throws before touching any state when a named prekey is unknown,
     * which is the cheap rejection path for an envelope meant for a device whose prekey ids this one
     * does not share -- the common case for every distribution in a group.
     *
     * Must be called with [lock] held.
     */
    private fun deriveResponder(preamble: Preamble): DerivedResponder {
        val signedPrekey = keys.signedPrekeyPair(preamble.signedPrekeyId)
            ?: throw CryptoError.noSession()
        val oneTimeId = preamble.oneTimePrekeyId
        val oneTimePrekey = oneTimeId?.let {
            keys.oneTimePrekeyPair(it) ?: throw CryptoError.noSession()
        }

        val message = InitialMessage(
            preamble.identity,
            preamble.ephemeralKey,
            preamble.signedPrekeyId,
            oneTimeId,
        )
        val seed = X3dh.respond(keys.identity(), signedPrekey, oneTimePrekey, message)
        val session = RatchetSession.responder(seed, signedPrekey)
        seed.destroy()
        return DerivedResponder(session, oneTimeId)
    }

    private fun preambleOf(message: InitialMessage): Preamble = Preamble(
        message.identity,
        message.ephemeralKey,
        message.signedPrekeyId,
        message.oneTimePrekeyId,
    )

    private fun sessionKey(conversationId: Id, deviceId: Id): String =
        "${conversationId.value}|${deviceId.value}"
}

/**
 * This device's own private key material, as the session layer needs to consult it.
 *
 * An interface rather than a concrete store because the two callers differ in a way that matters: the
 * live client reads from the sealed vault, and the conformance tests read from a fixed table of
 * vectors. Neither should have to pretend to be the other.
 */
interface LocalKeyStore {
    /** This device's long-term identity. */
    fun identity(): IdentitySecret

    /**
     * The private half of a signed prekey, by the id a peer named.
     *
     * Null when the id is unknown, which is not an error: a distribution addressed to another device
     * names that device's prekey ids, and every device in the conversation sees it.
     */
    fun signedPrekeyPair(signedPrekeyId: Long): KeyPair?

    /** The private half of a one-time prekey, by id. Null when unknown or already consumed. */
    fun oneTimePrekeyPair(keyId: Long): KeyPair?

    /**
     * Marks a one-time prekey used, so it is never served to a second session.
     *
     * Called only after a decrypt has succeeded. A prekey consumed on a failed attempt would let
     * anyone exhaust this device's supply by replaying malformed envelopes at it.
     */
    fun consumeOneTimePrekey(keyId: Long)
}

/** Where a peer's published prekey bundle comes from -- in the live client, a `KEY_BUNDLE_FETCH`. */
interface PeerBundleSource {
    /**
     * Fetches the bundle for one device of one user.
     *
     * Suspends because it is a network round trip, and throws when the server has none to serve --
     * a device that has published no keys cannot be written to, and pretending otherwise would
     * produce a message nobody can open.
     */
    suspend fun fetchBundle(userId: Id, deviceId: Id): PrekeyBundle
}

/**
 * Where ratchet state is kept across process death.
 *
 * Every method is synchronous and may block: the implementation is a file write, and the calls happen
 * under [SessionCrypto]'s lock, which is already the serialisation point for this device's sessions.
 * Making them suspend would suggest they can be interleaved, and they cannot.
 */
interface SessionPersistence {
    /** The stored session for a device, or null when there is none. */
    fun load(conversationId: Id, deviceId: Id): RatchetSession?

    /** Stores a session's current state, replacing what was there. */
    fun save(conversationId: Id, deviceId: Id, session: RatchetSession)

    /** Drops one device's session. */
    fun delete(conversationId: Id, deviceId: Id)

    /** Drops every session in a conversation. */
    fun deleteConversation(conversationId: Id)

    /**
     * Keeps everything in memory, which is what the TypeScript reference does.
     *
     * Correct for a test and wrong for the app: an Android process is killed in the background as a
     * matter of routine, and sessions that only lived in memory come back as a conversation that has
     * silently stopped decrypting.
     */
    object None : SessionPersistence {
        override fun load(conversationId: Id, deviceId: Id): RatchetSession? = null
        override fun save(conversationId: Id, deviceId: Id, session: RatchetSession) = Unit
        override fun delete(conversationId: Id, deviceId: Id) = Unit
        override fun deleteConversation(conversationId: Id) = Unit
    }
}

/** A sealed envelope, ready to place in a `MessageSend`. */
class SealedEnvelope(
    /** The `scheme` byte the envelope carries, for the caller's diagnostics. */
    val scheme: Int,
    /**
     * The authoritative `sender_key_id`, always `0` for pairwise traffic.
     *
     * The server accepts `sender_key_id` on `MessageSend` but never echoes it on `MessageEvent`,
     * because the binding copy lives inside the envelope. A sender may set the frame field from this;
     * a receiver reads it from the opened envelope and ignores the frame.
     */
    val senderKeyId: Long,
    /** The opaque envelope bytes. */
    val envelope: ByteArray,
) {
    /** Length only. The bytes are a ciphertext and brief section 174 keeps them out of logs. */
    override fun toString(): String =
        "SealedEnvelope(scheme: $scheme, sender_key_id: $senderKeyId, envelope_len: ${envelope.size})"
}

/** One remote device's ratchet, plus the initiator state that governs the scheme we send. */
private class SessionEntry(
    val session: RatchetSession,
    /**
     * The X3DH material to keep prepending, or null once the peer has replied.
     *
     * Set when we initiate. Until we successfully open a message from the peer we cannot know they
     * received our first one, so every message re-carries this material. Cleared on the first
     * successful open.
     */
    var pendingInit: InitialMessage?,
)

/** A responder session derived but not yet committed, with the prekey id to spend on success. */
private class DerivedResponder(val session: RatchetSession, val oneTimePrekeyId: Long?)
