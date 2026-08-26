package com.migo.core.session

import com.migo.core.crypto.AEAD_TAG_LEN
import com.migo.core.crypto.CHAIN_KEY_LEN
import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.Csprng
import com.migo.core.crypto.Cursor
import com.migo.core.crypto.ENVELOPE_VERSION
import com.migo.core.crypto.IDENTITY_PUBLIC_LEN
import com.migo.core.crypto.IdentityPublic
import com.migo.core.crypto.IdentitySecret
import com.migo.core.crypto.ReceiverKeyState
import com.migo.core.crypto.SCHEME_SENDER_KEY
import com.migo.core.crypto.SIGNATURE_LEN
import com.migo.core.crypto.SenderKeyDistribution
import com.migo.core.crypto.SenderKeyHeader
import com.migo.core.crypto.SenderKeyMessage
import com.migo.core.crypto.SenderKeyState
import com.migo.core.wire.ByteAccumulator
import com.migo.core.wire.Id
import com.migo.core.wire.Varint
import com.migo.core.wire.idToBytes
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * The end-to-end layer for broadcast conversations, which on Migo is every conversation.
 *
 * # Why sender-key, and why even for a direct chat
 *
 * The server fans one message out to a topic: a single sealed `envelope` reaches every device in the
 * conversation, the sender's own other devices included. One ciphertext therefore has to open on many
 * devices at once, and a pairwise Double Ratchet -- one ciphertext per recipient device -- cannot
 * produce that. So content is sealed once with a *sender key*: a symmetric chain the sender owns and
 * hands to each member device. This holds for a two-person chat too, because that chat still has
 * several devices per side and still fans out through one topic.
 *
 * The pairwise ratchet does not disappear; it becomes the *distribution channel*. [SessionCrypto]
 * carries each member device its sender-key distribution privately, and content is then broadcast
 * under the sender key. This class owns the sender-key half; the messaging domain wires the two
 * together.
 *
 * # The envelope (brief section 11, group layout)
 *
 * Section 11 leaves this scheme's concrete bytes to the implementation, so they are pinned here and
 * must match `packages/sdk/src/group-crypto.ts` byte for byte:
 *
 * ```text
 * u8      envelope_version       ENVELOPE_VERSION
 * u8      scheme                 SCHEME_SENDER_KEY
 * varint  sender_key_id          the chain id -- which of the sender's chains this is
 * varint  group_key_epoch        bumped when membership changes, so a removed member's key dies
 * varint  message_counter        index within the chain
 * 64      signature              Ed25519 over the group AAD and the ciphertext
 * bytes   ciphertext             to the end; the trailing 16 bytes are the AEAD tag
 * ```
 *
 * There is no `ratchet_public_key`: the layout omits it unless the scheme needs one, and a symmetric
 * chain does not. `previous_chain_length` is likewise absent because it means nothing here.
 *
 * The signature is what a symmetric chain cannot supply on its own. Every member holds the chain key,
 * so without a per-message identity signature any member could forge any other member's message.
 * [ReceiverKeyState.decrypt] checks it before deriving anything, which also keeps a forgery cheap to
 * reject. The conversation id is bound as the AEAD associated data, so a sealed message cannot be
 * lifted into a different conversation.
 *
 * # Locking, and why it is not a coroutine mutex
 *
 * Nothing in this class does I/O -- no bundle fetch, no request -- so every critical section is a few
 * microseconds of key derivation and the plain lock that costs nothing to hold is the right one. It
 * also keeps the API non-suspending, matching the reference, which matters because the messaging
 * domain seals content in the middle of work it is already doing. [ReentrantLock] specifically
 * because rotation is reachable from a sealing path; the private `*Locked` helpers mean that
 * reentrancy is permitted rather than depended on.
 */
class GroupCrypto(
    private val keys: IdentityProvider,
    private val persistence: GroupPersistence = GroupPersistence.None,
) {
    private val lock = ReentrantLock()
    private val sending = HashMap<Id, SendingEntry>()
    private val receiving = HashMap<String, ReceiverKeyState>()

    /** Whether this device already has an outbound chain for a conversation. */
    fun hasSenderKey(conversationId: Id): Boolean = lock.withLock {
        sendingOrNull(conversationId) != null
    }

    /** The membership epoch of a conversation's outbound chain, or `0` when there is none. */
    fun currentEpoch(conversationId: Id): Long = lock.withLock {
        sendingOrNull(conversationId)?.epoch ?: 0L
    }

    /**
     * Seals a plaintext once, for broadcast to the whole conversation.
     *
     * Establishes an outbound chain on first use, and rotates a chain that has reached its bound
     * rather than sealing past it -- the crypto layer refuses that outright, and a caller that has
     * not checked [needsRotation] should still get a message sent rather than an exception.
     */
    fun sealContent(conversationId: Id, plaintext: ByteArray): SealedEnvelope = lock.withLock {
        val entry = ensureSendingLocked(conversationId)
        val message = entry.state.encrypt(
            keys.identity(),
            conversationContext(conversationId),
            plaintext,
        )
        val envelope = encodeSenderKeyEnvelope(entry.epoch, message)
        persistSendingLocked(conversationId, entry)
        SealedEnvelope(SCHEME_SENDER_KEY, message.header.chainId, envelope)
    }

    /**
     * The serialised sender-key distribution for a conversation's current chain.
     *
     * This is the payload the messaging domain seals through the pairwise channel to one member
     * device. It carries the chain key *as of now*, so a member who receives it cannot read messages
     * sealed before they were given it -- which is the property that makes adding someone to a
     * conversation not also give them its history.
     *
     * The bytes that come back hold a chain key in the clear. That is unavoidable, since they exist
     * to be sealed and sent, but it means the caller must seal them promptly and not hold them.
     */
    fun distributionFor(conversationId: Id): ByteArray = lock.withLock {
        val entry = ensureSendingLocked(conversationId)
        val distribution = entry.state.distribution(keys.identity())
        try {
            serializeDistribution(distribution)
        } finally {
            // `distribution()` copied the chain key out of the state, so this zeroes the copy and
            // leaves the live chain alone.
            distribution.destroy()
        }
    }

    /**
     * Whether a member device still needs the current chain's distribution.
     *
     * True when there is no chain yet, because the first send will create one and every member will
     * need it.
     */
    fun needsDistribution(conversationId: Id, deviceId: Id): Boolean = lock.withLock {
        val entry = sendingOrNull(conversationId) ?: return@withLock true
        !entry.distributed.contains(deviceId)
    }

    /** Records that a member device has received the current chain's distribution. */
    fun markDistributed(conversationId: Id, deviceId: Id) {
        lock.withLock {
            val entry = ensureSendingLocked(conversationId)
            entry.distributed.add(deviceId)
            persistSendingLocked(conversationId, entry)
        }
    }

    /** Whether the outbound chain has reached its bound and must be rotated before further sends. */
    fun needsRotation(conversationId: Id): Boolean = lock.withLock {
        sendingOrNull(conversationId)?.state?.needsRotation() ?: false
    }

    /**
     * Starts a fresh outbound chain and bumps the epoch.
     *
     * Called when membership changes -- someone left, so the old chain must die -- and when the
     * current chain hits its message bound. Every member has to be re-sent the new distribution, so
     * the record of who already has one is cleared: keeping it would leave members holding a chain
     * that no longer seals anything, with no message to tell them so.
     */
    fun rotate(conversationId: Id) {
        lock.withLock { rotateLocked(conversationId) }
    }

    /**
     * Accepts a sender-key distribution from a remote device, so its later messages can be opened.
     *
     * A newer distribution for the same sender replaces the older one, and that is how a rotation is
     * adopted: the sender rotates, re-distributes, and this overwrites the state that can no longer
     * open anything.
     */
    fun acceptDistribution(conversationId: Id, senderDeviceId: Id, distributionBytes: ByteArray) {
        val distribution = parseDistribution(distributionBytes)
        try {
            val state = ReceiverKeyState.accept(distribution)
            lock.withLock {
                val key = receiverKey(conversationId, senderDeviceId)
                // Zero the chain key of what is being replaced rather than waiting for a collector
                // that may never run.
                receiving.put(key, state)?.destroy()
                persistence.saveReceiver(conversationId, senderDeviceId, state)
            }
        } finally {
            // `accept` copied the chain key into its own state, so this zeroes the parsed copy.
            distribution.destroy()
        }
    }

    /** Whether a distribution has been accepted for a remote sender device. */
    fun hasReceiver(conversationId: Id, senderDeviceId: Id): Boolean = lock.withLock {
        receiverOrNull(conversationId, senderDeviceId) != null
    }

    /**
     * Opens a broadcast envelope from a remote device.
     *
     * Throws [CryptoError.noSession] when no distribution has been accepted for the sender yet, and
     * the same when the message names a chain this receiver was never told about. Both mean the same
     * thing to the caller -- hold the message until the sender's distribution arrives -- which is why
     * they are the same error rather than two the caller would have to treat identically.
     *
     * The envelope's epoch is parsed and validated for shape but is not used to select state. The
     * chain id is what discriminates: a rotation mints a new chain, so a stale epoch and a stale chain
     * id arrive together and the chain id is the one the crypto layer can act on. The epoch exists so
     * a membership change is visible in the envelope, not so a receiver can second-guess it.
     */
    fun open(conversationId: Id, senderDeviceId: Id, envelope: ByteArray): ByteArray {
        val parsed = decodeSenderKeyEnvelope(envelope)
        return lock.withLock {
            val receiver = receiverOrNull(conversationId, senderDeviceId)
                ?: throw CryptoError.noSession()
            val plaintext = receiver.decrypt(conversationContext(conversationId), parsed.message)
            // The chain advanced, so the state on disk is now behind the state in memory. Saving
            // after the decrypt rather than before means a failed decrypt writes nothing.
            persistence.saveReceiver(conversationId, senderDeviceId, receiver)
            plaintext
        }
    }

    /**
     * Forgets sender-key state.
     *
     * With [deviceId], drops only that remote sender's inbound state, which is what a single peer's
     * identity change calls for. Without it, drops this device's outbound chain and every inbound
     * receiver for the conversation -- leaving a conversation, or a membership change severe enough
     * that nothing built on the old trust should survive.
     */
    fun forget(conversationId: Id, deviceId: Id? = null) {
        lock.withLock {
            if (deviceId != null) {
                receiving.remove(receiverKey(conversationId, deviceId))?.destroy()
                persistence.deleteReceiver(conversationId, deviceId)
                return@withLock
            }
            sending.remove(conversationId)?.state?.destroy()
            val prefix = "${conversationId.value}|"
            val stale = receiving.keys.filter { it.startsWith(prefix) }
            for (key in stale) receiving.remove(key)?.destroy()
            persistence.deleteConversation(conversationId)
        }
    }

    /**
     * The outbound chain for a conversation, hydrated from the store, created, or rotated as needed.
     *
     * Must be called with [lock] held.
     */
    private fun ensureSendingLocked(conversationId: Id): SendingEntry {
        val existing = sendingOrNull(conversationId)
        if (existing == null) {
            // Epoch 1, not 0: `currentEpoch` reports 0 for "no chain", so a real chain must never
            // carry that value or the two states would be indistinguishable on the wire.
            val created = SendingEntry(SenderKeyState.create(randomChainId()), 1L, HashSet())
            sending[conversationId] = created
            persistSendingLocked(conversationId, created)
            return created
        }
        if (existing.state.needsRotation()) return rotateLocked(conversationId)
        return existing
    }

    /** Rotation proper. Must be called with [lock] held. */
    private fun rotateLocked(conversationId: Id): SendingEntry {
        val previous = sending.remove(conversationId)
        val epoch = (previous?.epoch ?: 0L) + 1L
        previous?.state?.destroy()
        val entry = SendingEntry(SenderKeyState.create(randomChainId()), epoch, HashSet())
        sending[conversationId] = entry
        persistSendingLocked(conversationId, entry)
        return entry
    }

    /** Must be called with [lock] held. */
    private fun persistSendingLocked(conversationId: Id, entry: SendingEntry) {
        persistence.saveSending(conversationId, entry.state, entry.epoch, entry.distributed)
    }

    /** Must be called with [lock] held. */
    private fun sendingOrNull(conversationId: Id): SendingEntry? {
        sending[conversationId]?.let { return it }
        val stored = persistence.loadSending(conversationId) ?: return null
        val entry = SendingEntry(stored.state, stored.epoch, HashSet(stored.distributed))
        sending[conversationId] = entry
        return entry
    }

    /** Must be called with [lock] held. */
    private fun receiverOrNull(conversationId: Id, senderDeviceId: Id): ReceiverKeyState? {
        val key = receiverKey(conversationId, senderDeviceId)
        receiving[key]?.let { return it }
        val stored = persistence.loadReceiver(conversationId, senderDeviceId) ?: return null
        receiving[key] = stored
        return stored
    }
}

/**
 * The identity secret this layer signs with.
 *
 * Narrower than [LocalKeyStore] on purpose: the group layer only ever signs its own messages and
 * builds its own distributions, and never answers somebody else's prekey. A [LocalKeyStore] satisfies
 * this too, so one object can serve both layers without either being able to reach past its own need.
 */
interface IdentityProvider {
    /** This device's long-term identity secret. */
    fun identity(): IdentitySecret
}

/**
 * Where sender-key state is kept across process death.
 *
 * The inbound half is the one that matters most. A lost outbound chain costs a rotation, which is
 * churn; lost *receiver* state is messages that cannot be opened and never will be, because the
 * sender believes it has already distributed its key and has no reason to do it again.
 */
interface GroupPersistence {
    /** The stored outbound chain for a conversation, or null when there is none. */
    fun loadSending(conversationId: Id): StoredSending?

    /** Stores an outbound chain, its epoch, and who has been given it. */
    fun saveSending(conversationId: Id, state: SenderKeyState, epoch: Long, distributed: Set<Id>)

    /** The stored inbound state for one remote sender device, or null when there is none. */
    fun loadReceiver(conversationId: Id, senderDeviceId: Id): ReceiverKeyState?

    /** Stores one remote sender device's inbound state. */
    fun saveReceiver(conversationId: Id, senderDeviceId: Id, state: ReceiverKeyState)

    /** Drops one remote sender device's inbound state. */
    fun deleteReceiver(conversationId: Id, senderDeviceId: Id)

    /** Drops the outbound chain and every inbound state for a conversation. */
    fun deleteConversation(conversationId: Id)

    /** Keeps everything in memory, as the TypeScript reference does. Correct for a test only. */
    object None : GroupPersistence {
        override fun loadSending(conversationId: Id): StoredSending? = null
        override fun saveSending(
            conversationId: Id,
            state: SenderKeyState,
            epoch: Long,
            distributed: Set<Id>,
        ) = Unit
        override fun loadReceiver(conversationId: Id, senderDeviceId: Id): ReceiverKeyState? = null
        override fun saveReceiver(conversationId: Id, senderDeviceId: Id, state: ReceiverKeyState) =
            Unit
        override fun deleteReceiver(conversationId: Id, senderDeviceId: Id) = Unit
        override fun deleteConversation(conversationId: Id) = Unit
    }
}

/** A restored outbound chain: the chain itself, its epoch, and who already holds it. */
class StoredSending(val state: SenderKeyState, val epoch: Long, val distributed: Set<Id>)

/** One conversation's outbound sender key: the chain, its epoch, and who already has it. */
private class SendingEntry(
    val state: SenderKeyState,
    /** The membership epoch this chain belongs to; travels in every message it seals. */
    val epoch: Long,
    /** Device ids already given a distribution for *this* chain, so resends are skipped. */
    val distributed: MutableSet<Id>,
)

/** The map key for an inbound receiver: conversation, then sender device. */
private fun receiverKey(conversationId: Id, senderDeviceId: Id): String =
    "${conversationId.value}|${senderDeviceId.value}"

/**
 * The 16 conversation bytes bound into every group message as associated data.
 *
 * The raw id rather than its text form, and no domain label: this must be the same byte string the
 * web client's `conversationContext` produces, and that is `idToBytes(conversationId)`. A label added
 * on one side only would make every message from that side undecryptable everywhere else.
 */
private fun conversationContext(conversationId: Id): ByteArray = idToBytes(conversationId)

/**
 * A fresh random chain id, as an unsigned 32-bit value in a [Long].
 *
 * Random rather than a counter because a chain id has to be unique across this device's *reinstalls*
 * as well as within one run: a counter restarting at zero after a reinstall would mint a new chain
 * that collides with one a peer still holds, and the peer would decide the message belongs to the
 * chain it already has and fail to open it. Thirty-two random bits make that negligible.
 */
private fun randomChainId(): Long {
    val bytes = Csprng.bytes(4)
    var value = 0L
    for (byte in bytes) value = (value shl 8) or (byte.toLong() and 0xFF)
    return value
}

/** Serialises a distribution: chain id, message number, the chain key, the sender's identity. */
private fun serializeDistribution(distribution: SenderKeyDistribution): ByteArray {
    val out = ByteAccumulator(10 + CHAIN_KEY_LEN + IDENTITY_PUBLIC_LEN)
    Varint.encodeU64(distribution.chainId, out)
    Varint.encodeU64(distribution.messageNumber, out)
    out.append(distribution.exposeChainKey())
    out.append(distribution.identity.toBytes())
    return out.toByteArray()
}

/**
 * Parses a distribution written by [serializeDistribution].
 *
 * These bytes arrived sealed through the pairwise channel, so they are authenticated -- but a peer
 * whose own state is corrupt can still send a well-sealed malformed distribution, and every failure
 * here is [CryptoError.malformedHeader] with nothing of the input in it.
 */
private fun parseDistribution(bytes: ByteArray): SenderKeyDistribution {
    val cursor = Cursor(bytes)
    val chainId = cursor.varintU32()
    val messageNumber = cursor.varintU32()
    val chainKey = cursor.take(CHAIN_KEY_LEN)
    val identity = try {
        IdentityPublic.parse(cursor.take(IDENTITY_PUBLIC_LEN))
    } catch (_: CryptoError) {
        throw CryptoError.malformedHeader()
    }
    // Constructed bare: `varintU32` has already narrowed both ids to `u32` and `take` returned
    // exactly CHAIN_KEY_LEN bytes, so the constructor's own length checks cannot fire here. A catch
    // for a branch that cannot be taken would describe a failure mode that does not exist.
    return SenderKeyDistribution(chainId, messageNumber, chainKey, identity)
}

/** Assembles the section 11 group envelope from a sealed sender-key message. */
private fun encodeSenderKeyEnvelope(epoch: Long, message: SenderKeyMessage): ByteArray {
    val out = ByteAccumulator(2 + 15 + SIGNATURE_LEN + message.ciphertext.size)
    out.push(ENVELOPE_VERSION)
    out.push(SCHEME_SENDER_KEY)
    Varint.encodeU64(message.header.chainId, out)
    Varint.encodeU64(epoch, out)
    Varint.encodeU64(message.header.messageNumber, out)
    out.append(message.signature)
    out.append(message.ciphertext)
    return out.toByteArray()
}

/** A parsed group envelope: the membership epoch, and the message to hand the crypto layer. */
private class ParsedSenderKeyEnvelope(val epoch: Long, val message: SenderKeyMessage)

/**
 * Parses a group envelope, rejecting a version or scheme this build does not understand.
 *
 * Every failure is [CryptoError.malformedHeader], as on the 1:1 path: these bytes came from the
 * network, the caller's response is the same whatever was wrong, and brief section 174 keeps them out
 * of any message that might be logged.
 */
private fun decodeSenderKeyEnvelope(bytes: ByteArray): ParsedSenderKeyEnvelope {
    val cursor = Cursor(bytes)
    if (cursor.u8() != ENVELOPE_VERSION) throw CryptoError.malformedHeader()
    // A 1:1 envelope reaching the group layer lands here, which is the mirror of `Envelope.decode`
    // refusing a sender-key envelope on the 1:1 path. Neither layer guesses at the other's bytes.
    if (cursor.u8() != SCHEME_SENDER_KEY) throw CryptoError.malformedHeader()
    val chainId = cursor.varintU32()
    val epoch = cursor.varintU32()
    val messageNumber = cursor.varintU32()
    val signature = cursor.take(SIGNATURE_LEN)
    val ciphertext = cursor.rest()
    if (ciphertext.size < AEAD_TAG_LEN) throw CryptoError.malformedHeader()
    val header = SenderKeyHeader(chainId, messageNumber)
    return ParsedSenderKeyEnvelope(epoch, SenderKeyMessage(header, ciphertext, signature))
}
