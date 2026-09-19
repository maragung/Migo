package com.migo.core.domain

import com.migo.core.crypto.CALL_KEY_LEN
import com.migo.core.crypto.CallKeyState
import com.migo.core.crypto.Content
import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.CryptoErrorKind
import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.CallKeyUpdate
import com.migo.core.protocol.CallRenegotiate
import com.migo.core.protocol.CallSdp
import com.migo.core.protocol.Op
import com.migo.core.session.SessionCrypto
import com.migo.core.wire.ID_BYTE_LEN
import com.migo.core.wire.Id
import com.migo.core.wire.WireError
import com.migo.core.wire.idFromBytes
import com.migo.core.wire.idToBytes
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * The control-event name a mid-call joiner's key ask carries inside its pairwise envelope.
 *
 * A client-to-client constant, sealed before it leaves the device, so the server never sees it --
 * the same name the SDK's `CALL_KEY_ASK_EVENT` and the desktop's `KEY_ASK_EVENT` pin.
 */
private const val CALL_KEY_ASK_EVENT = "call-key-ask"

/**
 * The frame keys of the group calls this device is seated in, and the section 163 triggers that
 * rotate and distribute them.
 *
 * A group call's media is sealed under one *frame key* shared by every seat, and section 163
 * requires that a participant who leaves cannot decrypt what follows and one who joins cannot
 * decrypt what came before. Two mechanisms hold that line, both living here:
 *
 *  - **Rotation on membership movement.** When the roster snapshot or a join/departure announcement
 *    shows a seat moved in a call this device is seated in, [GroupCallsDomain]'s listener folds the
 *    roster and this domain rotates the frame key (epoch+1) and publishes one `CALL_KEY_UPDATE`,
 *    its key material sealed under the *currently running* key so only a device already in the call
 *    can open it. The rotating device advances its own state before the frame leaves and never
 *    waits to hear its own update back — the server fans the update to every other seat, and this
 *    device's state is already the truth it sealed.
 *  - **The mid-call joiner's first key.** A device that joins a call already in progress holds no
 *    frame key, and the frames it can already see are exactly the ones it must not open. The joiner
 *    asks a seated participant for the current key ([requestJoinKey]); the seated participant
 *    rotates — so the key the joiner receives did not exist while they were outside — announces
 *    that rotation to the rest of the roster as a `CALL_KEY_UPDATE`, and answers the joiner with
 *    the current epoch and key sealed under a wrapper key derived from the pairwise session's
 *    X3DH secret ([CallKeyState.sealedJoinDistribution]). The joiner installs it as its baseline
 *    ([CallKeyState.fromJoinDistribution]) and rides the same rotations as everyone else after
 *    that.
 *
 * # How the ask and its answer travel
 *
 * The server's relay contract (pinned by the desktop-side wire suite) is that a joiner's key
 * request rides a `CALL_RENEGOTIATE` — from the server's side a key request is indistinguishable
 * from a codec renegotiation, which is what keeps the request's content out of the wire's metadata
 * — and the group relay *projects* that renegotiation to a `CALL_SDP` for the target device, so
 * the seated participant receives the ask as a `CALL_SDP` and answers on the same opcode.
 *
 * # The wrapper key's secret, and the unified reading of it
 *
 * The wrapper key is derived from the X3DH shared secret of the pairwise session the joiner and
 * the holder share -- the same bytes the SDK's `SessionCrypto.sessionSecret` and the desktop
 * store's `pairwise_secret` return, and which this client's session layer now retains per session
 * the same way. It once destroyed the X3DH seed as soon as the ratchet was built, which is why an
 * earlier build of this domain minted a fresh 32-byte secret and carried it inside the ask; that
 * shape is still answered, as a legacy branch documented below.
 *
 * The ask itself is the unified content event: a `call-key-ask` control event whose `data` is the
 * joiner's account id, encoded by the content codec with its default bucket padding -- byte for
 * byte what the desktop sends and what the SDK's encoder produces -- and sealed under the pairwise
 * Double Ratchet, whose envelope establishes the session when none exists yet, so both ends hold
 * the secret by the time the answer is sealed. The answer is the join distribution sealed under
 * `migo-call-join-v1` with the call id as salt, byte-for-byte the reference's derivation.
 *
 * Two honest limits, documented rather than papered over:
 *
 *  - **The legacy ask.** A joiner on an older Android build asks with a freshly minted 32-byte
 *    secret as the raw payload. It is still answered, under those very bytes, because that is the
 *    only wrapper the joiner can open; the shape is recognised as "not content, exactly 32 bytes".
 *  - **The session that predates the secret.** A pairwise session established before this build
 *    retained the X3DH secret -- restored from a record written by an older build -- has no secret
 *    to derive the wrapper from, on either half. The holder declines to answer a unified ask and
 *    the joiner declines to install an answer, both quietly; that session never gains a secret,
 *    and recovery is a fresh session, which a peer identity change or a conversation leave and
 *    rejoin produces.
 */

/**
 * The frame-key state, held across connections.
 *
 * The domain is per-connection (it speaks through one [Rpc]) but a key must not die with a
 * reconnect: a call keeps running across a dropped socket, and a re-keyed epoch the joiner can no
 * longer adopt is a call that has silently stopped decrypting. So the state lives here, on the
 * client, the same split [com.migo.core.session.GroupCrypto] keeps.
 *
 * All methods are non-suspending and guarded by one lock: they are a few map operations and AEAD
 * calls, never I/O — the network round trips belong to the domain, which calls in, does the wire
 * work, and calls back in.
 */
class CallKeyStore {
    private val lock = ReentrantLock()

    /** One seated call: the conversation it belongs to, and this device's frame-key state. */
    private class CallEntry(val conversationId: Id, val state: CallKeyState)

    /**
     * One outstanding join ask: the conversation it belongs to and the holder it was sent to. The
     * secret the answer opens under is not kept here -- it is the pairwise session's own X3DH
     * secret, which the session layer holds for as long as the session lives.
     */
    private class PendingJoinAsk(
        val conversationId: Id,
        val holderDevice: Id,
    )

    private val calls = HashMap<Id, CallEntry>()
    private val asks = HashMap<Id, PendingJoinAsk>()

    /**
     * Installs a call's epoch-0 frame key, derived from the secret this device minted for the call.
     *
     * A no-op when a state already exists: re-running a join (a retried `CALL_SFU_JOIN` re-seats
     * the same call) must not rewind a key that rotations have already advanced.
     */
    fun seated(callId: Id, conversationId: Id, sessionSecret: ByteArray) {
        lock.withLock {
            if (calls[callId] != null) return
            calls[callId] = CallEntry(conversationId, CallKeyState.fromSession(sessionSecret, callId))
        }
    }

    /** Whether this device holds a frame key for the call — the "am I a potential holder" test. */
    fun holdsKey(callId: Id): Boolean = lock.withLock { calls[callId] != null }

    /**
     * The epoch of a call's current frame key, or null when this device holds no key for it.
     *
     * The media plane is the caller: every link it builds is stamped with the epoch it was sealed
     * under, and a link that has not finished negotiating is rebuilt when that stamp stops matching
     * -- which is the whole of what a rotation means to a mesh whose links have not connected yet.
     */
    fun epochOf(callId: Id): Long? = lock.withLock { calls[callId]?.state?.epoch() }

    /** The conversation a call belongs to, from whichever half this device holds. */
    fun conversationOf(callId: Id): Id? = lock.withLock {
        calls[callId]?.conversationId ?: asks[callId]?.conversationId
    }

    /**
     * The device a pending join ask was sent to, or null.
     *
     * The answer must name the device this device asked; a `CALL_SDP` from anyone else is not the
     * answer, and a holder's ask (below) is not either.
     */
    fun pendingAskHolder(callId: Id): Id? = lock.withLock { asks[callId]?.holderDevice }

    /**
     * Rotates a call's key and returns the new epoch with its sealed update, or null when this
     * device holds no key for the call.
     *
     * The state advances here, before any frame is sent: a device creating a rotation must not
     * depend on hearing its own update back.
     */
    fun rotate(callId: Id): Pair<Long, ByteArray>? = lock.withLock {
        val entry = calls[callId] ?: return null
        val sealed = entry.state.rotate()
        entry.state.epoch() to sealed
    }

    /**
     * Adopts a distributed update for a call this device holds a key for.
     *
     * Returns false when there is no key to adopt onto (the update is dropped — it is not ours to
     * apply) and throws the crypto layer's own refusal otherwise: [CryptoError.keyAlreadyUsed] is
     * the mechanism a replayed or rolled-back update meets, and the caller treats it as the no-op
     * it is.
     */
    fun adopt(callId: Id, epoch: Long, sealed: ByteArray): Boolean = lock.withLock {
        val entry = calls[callId] ?: return false
        entry.state.adopt(epoch, sealed)
        true
    }

    /**
     * Records a join ask before its frame is sent, so an answer that outruns the send still finds
     * the conversation and the holder it belongs to.
     */
    fun beginJoinAsk(callId: Id, conversationId: Id, holderDevice: Id) {
        lock.withLock { asks[callId] = PendingJoinAsk(conversationId, holderDevice) }
    }

    /** Drops a join ask that never got answered -- a retry sends a fresh ask. */
    fun clearJoinAsk(callId: Id) {
        lock.withLock { asks.remove(callId) }
    }

    /**
     * Tries to complete a pending ask with the sealed answer that just arrived.
     *
     * The answer must open under [sessionSecret] -- the pairwise session's X3DH secret, the same
     * bytes the holder derived its wrapper key from, handed in by the domain because fetching it
     * suspends and these methods do not -- and be bound to the call; a blob that does not is not
     * the answer (a future shape this build does not know), the ask stands, and false comes back
     * so the caller can fall through to its other handling.
     */
    fun completeJoinAsk(callId: Id, sessionSecret: ByteArray, sealedAnswer: ByteArray): Boolean =
        lock.withLock {
            val ask = asks[callId] ?: return false
            val state = try {
                CallKeyState.fromJoinDistribution(sessionSecret, callId, sealedAnswer)
            } catch (_: CryptoError) {
                return false
            }
            asks.remove(callId)
            // The joiner's first key is a baseline constructor, so it only installs where no state
            // stands: a device that already holds a key for the call was never the joiner this answer
            // is for, and its state is ahead of nothing the answer carries.
            if (calls[callId] == null) {
                calls[callId] = CallEntry(ask.conversationId, state)
            } else {
                state.destroy()
            }
            true
        }

    /**
     * The holder's half of a join: rotates on the join and returns the rotation's two sealed
     * halves, or null when this device holds no key for the call (and so is not a holder to
     * answer it).
     *
     * One rotation, two seals: the *update* is sealed under the pre-rotation key for the rest of
     * the roster (the `CALL_KEY_UPDATE` every seat must adopt to stay in step), and the *join
     * distribution* is sealed under the wrapper key [joinSecret] derives -- the pairwise session's
     * X3DH secret for a unified ask, the raw bytes a legacy ask carried -- for the joiner alone.
     * One rotation, not two, because two would leave the roster an epoch behind the joiner
     * — the joiner would hold a key no announcement ever carried.
     *
     * Rotating *before* distributing is what makes the joiner's first key one that did not exist
     * while they were outside the call — pre-join media stays sealed because it is bound to the
     * older epoch.
     */
    fun rotateForJoin(callId: Id, joinSecret: ByteArray): JoinAnswer? = lock.withLock {
        val entry = calls[callId] ?: return null
        val sealedUpdate = entry.state.rotate()
        val sealedJoin = entry.state.sealedJoinDistribution(joinSecret)
        JoinAnswer(entry.state.epoch(), sealedUpdate, sealedJoin)
    }

    /**
     * Seals one media frame under a call's current frame key, or null when this device holds no key
     * for the call.
     *
     * The key is never handed out: sealing happens here so a caller with a frame to send cannot keep
     * the key material, and so every frame a device sends is bound to the epoch it was actually
     * sealed under (the binding is inside [CallKeyState.sealFrame], which the state applies).
     */
    fun sealFrame(callId: Id, frame: ByteArray): ByteArray? = lock.withLock {
        calls[callId]?.state?.sealFrame(frame)
    }

    /**
     * Opens one media frame sealed under a call's current frame key, or null when it does not open.
     *
     * Null is the honest answer for every reason a frame is not ours to read -- this device holds no
     * key for the call, the frame is sealed under another call, or it carries an epoch this device
     * has not adopted (the frame crossed a rotation in flight, and a frame bound to a key the device
     * no longer holds is exactly what the epoch binding is for). A caller that must tell "not mine"
     * from "corrupt" cannot from this method: the crypto layer does not distinguish, and an
     * authenticated refusal is the only fact the bytes support.
     *
     * The one thing this must *not* do is throw: it is the discrimination test a media plane runs
     * against every relay it receives, most of which belong to the key exchange rather than to
     * media, so a refusal is the common case and takes the null path.
     */
    fun openFrame(callId: Id, sealed: ByteArray): ByteArray? = lock.withLock {
        val state = calls[callId]?.state ?: return@withLock null
        try {
            state.openFrame(sealed)
        } catch (_: CryptoError) {
            null
        }
    }

    /** Drops a call's state: the seat is gone, and so is everything the key was for. */
    fun forget(callId: Id) {
        lock.withLock {
            calls.remove(callId)?.state?.destroy()
            asks.remove(callId)
        }
    }
}

/** The two sealed halves of one join rotation, as [CallKeyStore.rotateForJoin] mints them. */
class JoinAnswer(
    /** The epoch the rotation landed on; the update frame's own epoch field. */
    val epoch: Long,
    /** The rotation sealed under the pre-rotation key, for the roster's `CALL_KEY_UPDATE`. */
    val sealedUpdate: ByteArray,
    /** The rotation sealed under the join wrapper key, for the joiner's `CALL_SDP` answer. */
    val sealedJoinDistribution: ByteArray,
)

/**
 * The wire half of the group-call frame keys: rotation announcements, join asks, join answers.
 *
 * Stateless beyond [CallKeyStore] — the same shape [GroupCallsDomain] keeps, for the same reason:
 * the domain is per-connection and cheap to rebuild, and the state that must survive a reconnect
 * lives on the client.
 */
class GroupCallKeysDomain(
    private val rpc: Rpc,
    private val accountId: Id,
    private val deviceId: Id,
    private val scope: CoroutineScope,
    private val store: CallKeyStore,
    private val sessionCrypto: SessionCrypto,
    private val directory: DeviceDirectory,
    private val onEventError: EventErrorHandler? = null,
) {
    private val subscriptions = ArrayList<Subscription>()

    /**
     * Serialises inbound handling, the same discipline [MessagingDomain] keeps: a rotation update
     * only opens under the epoch before it, so two updates for one call must be adopted in arrival
     * order, and freely-launched coroutines would not guarantee that.
     */
    private val eventLock = Mutex()

    /** Begins delivering frame-key frames. Idempotent. */
    fun start() {
        if (subscriptions.isNotEmpty()) return
        subscriptions += rpc.on(Op.CALL_KEY_UPDATE, { r -> CallKeyUpdate.decode(r) }) { event, _ ->
            scope.launch(start = CoroutineStart.UNDISPATCHED) { handleKeyUpdate(event) }
        }
        // The ask arrives here because the group relay projects a joiner's CALL_RENEGOTIATE to a
        // CALL_SDP for its target; the answer arrives here because it *is* a CALL_SDP. Both are
        // addressed relays, so only frames naming this device are considered.
        subscriptions += rpc.on(Op.CALL_SDP, { r -> CallSdp.decode(r) }) { event, _ ->
            if (event.toDevice != deviceId) return@on
            scope.launch(start = CoroutineStart.UNDISPATCHED) { handleCallSdp(event) }
        }
    }

    /** Stops delivering. Registered handlers are kept for a later [start]. */
    fun stop() {
        for (subscription in subscriptions) {
            subscription.cancel()
        }
        subscriptions.clear()
    }

    /**
     * Installs the frame key of a call this device created, derived from the per-call secret the
     * join minted. Call it once per join, before the roster snapshot can arrive.
     */
    fun seated(callId: Id, conversationId: Id, callKey: ByteArray) {
        store.seated(callId, conversationId, callKey)
    }

    /**
     * Rotates a seated call's frame key and publishes the update: the membership movement trigger.
     *
     * A no-op for a call this device holds no key for (an un-seated screen's roster news), and a
     * no-op on the wire when the rotation's frame cannot be sent — the local state has already
     * advanced, which is the honest half: this device's media from here on is sealed under the new
     * epoch whether or not the announcement landed, and the next rotation carries the same
     * guarantee forward.
     */
    suspend fun rotateForMembership(callId: Id) {
        val rotated = store.rotate(callId) ?: return
        val request = CallKeyUpdate(callId = callId, epoch = rotated.first, sealedKeyMaterial = rotated.second)
        try {
            rpc.call(Op.CALL_KEY_UPDATE, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            // The state has advanced; the peers that missed the announcement are behind, not wrong
            // — their next rotation lands on a later epoch all the same. There is no retry here
            // because the sealed update is bound to the epoch it rotated from: re-sending it after
            // a peer has advanced past that epoch would be a frame they cannot open either way.
        }
    }

    /**
     * The mid-call joiner's ask: requests the running key of a call this device just joined and
     * holds no key for.
     *
     * The distributor is chosen deterministically from the roster snapshot: **the first seated
     * participant in the server's join order whose account is not the joiner's own**. The server's
     * order is stable for a given snapshot, every joiner sees the same list, and skipping the
     * joiner's own account keeps the ask off this account's other devices — a sibling device holds
     * no frame-key state for this seat and could not answer. First-in-order rather than any other
     * tie-break because the earliest seat is the one least likely to be mid-departure.
     *
     * The ask's payload is the unified content event — a `call-key-ask` control event carrying
     * this account's id in `data`, encoded by the content codec with its default bucket padding so
     * the bytes are identical to the SDK's and desktop's asks — sealed under the pairwise Double
     * Ratchet, whose envelope establishes the session when none exists yet. It rides a
     * `CALL_RENEGOTIATE`, which the relay projects to a `CALL_SDP` on the holder's side.
     */
    suspend fun requestJoinKey(roster: GroupCallRoster) {
        if (store.holdsKey(roster.callId) || store.pendingAskHolder(roster.callId) != null) return
        val holder = roster.participants.firstOrNull { it.userId != roster.userId } ?: return
        store.beginJoinAsk(roster.callId, roster.conversationId, holder.deviceId)
        try {
            val ask = Content.ControlEvent(CALL_KEY_ASK_EVENT, idToBytes(accountId)).encode()
            val sealed =
                sessionCrypto.seal(roster.conversationId, deviceId, holder.userId, holder.deviceId, ask)
            val request = CallRenegotiate(
                callId = roster.callId,
                fromDevice = deviceId,
                toDevice = holder.deviceId,
                sealedSdp = sealed.envelope,
            )
            rpc.call(Op.CALL_RENEGOTIATE, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
        } catch (cancelled: CancellationException) {
            store.clearJoinAsk(roster.callId)
            throw cancelled
        } catch (_: Exception) {
            // The ask never left; dropping it lets a later roster or retry send a fresh one rather
            // than waiting on an answer that cannot come.
            store.clearJoinAsk(roster.callId)
        }
    }

    /** Drops all frame-key state for a call — the seat is gone. */
    fun forget(callId: Id) {
        store.forget(callId)
    }

    /**
     * Whether this device holds a frame key for the call — the screen's branch between the two
     * joiner stories: a device holding the key of a call whose roster just showed other seats is
     * seated among them and rotates; a device holding none is the mid-call joiner and asks.
     */
    fun holdsKey(callId: Id): Boolean = store.holdsKey(callId)

    /** The epoch of a call's current frame key, or null when this device holds no key for it. */
    fun epochOf(callId: Id): Long? = store.epochOf(callId)

    /**
     * Seals one media frame under a call's current frame key, or null when this device holds no key
     * for it.
     *
     * The media plane reaches the frame key through the key domain rather than the store, so the
     * store stays private to the domain that keeps its rotation discipline -- a caller holding the
     * store directly could seal under a state the domain is midway through advancing.
     */
    fun sealFrame(callId: Id, frame: ByteArray): ByteArray? = store.sealFrame(callId, frame)

    /**
     * Opens one media frame under a call's current frame key, or null when it does not open.
     *
     * This is also the group media plane's discrimination test: `CALL_SDP` carries both the key
     * exchange and the media descriptions, and the two are told apart by which key opens the blob --
     * the media plane tries this, the key domain tries its own session-crypto open, and AEAD
     * guarantees at most one of them succeeds.
     */
    fun openFrame(callId: Id, sealed: ByteArray): ByteArray? = store.openFrame(callId, sealed)

    /**
     * Adopts a distributed rotation update.
     *
     * An update for a call this device holds no key for is dropped quietly: it is either a call
     * this screen is not seated in or the joiner's pre-answer window (the answer, not the update,
     * is what installs the joiner's first key). A refusal of a non-advancing epoch is the replay
     * guard working and passes as the no-op it is; anything else goes to the error sink.
     */
    private suspend fun handleKeyUpdate(event: CallKeyUpdate) {
        eventLock.withLock {
            if (!store.holdsKey(event.callId)) return@withLock
            try {
                store.adopt(event.callId, event.epoch, event.sealedKeyMaterial)
            } catch (refused: CryptoError) {
                if (refused.kind != CryptoErrorKind.KeyAlreadyUsed) {
                    onEventError?.invoke(Op.CALL_KEY_UPDATE, refused)
                }
            }
        }
    }

    /**
     * Handles one addressed `CALL_SDP` for a call this device has frame-key business with.
     *
     * Two shapes share the opcode, told apart by what this device holds, not by the bytes: the
     * *answer* to this device's own ask (a pending ask names the device that was asked), and the
     * *ask* of a later joiner (this device holds a key and is therefore a potential holder). A
     * frame that is neither — no ask pending, no key held, or the crypto refuses the bytes — is
     * dropped quietly, because this build's group calls carry no other `CALL_SDP` traffic and a
     * blob that will not open is not this call's to interpret.
     */
    private suspend fun handleCallSdp(event: CallSdp) {
        eventLock.withLock {
            if (store.pendingAskHolder(event.callId) == event.fromDevice) {
                // The answer opens under the pairwise session's X3DH secret — the same bytes the
                // holder derived its wrapper key from. Fetched here rather than inside the store
                // because the lookup suspends and the store's methods do not. A null secret is a
                // session whose establishment predates this build's retention of it: there is
                // nothing honest to open the answer with, so it is declined quietly and the ask
                // stands — that session never gains a secret (see the module note).
                val conversationId = store.conversationOf(event.callId) ?: return@withLock
                val sessionSecret =
                    sessionCrypto.sessionSecret(conversationId, event.fromDevice) ?: return@withLock
                try {
                    store.completeJoinAsk(event.callId, sessionSecret, event.sealedSdp)
                } finally {
                    sessionSecret.fill(0)
                }
                return@withLock
            }
            if (store.holdsKey(event.callId)) {
                answerJoinAsk(event)
            }
        }
    }

    /**
     * The holder's half of a mid-call join: opens the joiner's ask, rotates on the join, and
     * answers with the running key sealed for that joiner.
     *
     * The ask opens under the pairwise session this device shares with the joiner — the envelope
     * establishes that session when none exists, which is the point of sending the ask as one —
     * and then dispatches on what the plaintext is:
     *
     *  - the **unified ask**: a `call-key-ask` content event. The answer is sealed under the
     *    wrapper key the *session's* X3DH secret derives.
     *  - the **legacy ask**: exactly [CALL_KEY_LEN] bytes that are not content — a joiner on an
     *    older Android build carrying a freshly minted secret. Answered under those very bytes,
     *    the only wrapper that joiner can open.
     *  - anything else — another shape this build did not send, or content that is not the ask —
     *    is dropped quietly with the state untouched.
     *
     * The rotation is announced to the rest of the roster as a `CALL_KEY_UPDATE` before the joiner
     * gets its answer, because the server fans an update to every seat and a seat that missed the
     * join rotation is a seat stranded an epoch behind the joiner it was meant to stay in step
     * with. The account half of the open is bookkeeping only, the same as every pairwise open
     * here — the unified ask names the joiner's account in its own body.
     */
    private suspend fun answerJoinAsk(event: CallSdp) {
        val conversationId = store.conversationOf(event.callId) ?: return
        // The directory's account — or its absence — cannot gate the open (the parameter is
        // bookkeeping only), so a device the directory does not know yet still has its ask opened;
        // the unified ask carries the joiner's account itself.
        val knownAccount = directory.accountOfDevice(conversationId, event.fromDevice)
        val plaintext = try {
            sessionCrypto.open(
                conversationId,
                knownAccount ?: event.fromDevice,
                event.fromDevice,
                event.sealedSdp,
            )
        } catch (_: Throwable) {
            return
        }
        try {
            // Content first: the unified ask is a content event, and a random 32-byte legacy
            // secret decodes as one only by astronomical accident — an accident that still lands
            // in the quiet drop below rather than anywhere state moves. `Content.decode` parks an
            // unknown type byte as `Unsupported` rather than throwing, so "not content" covers
            // both the refusal and the park.
            val content = try {
                Content.decode(plaintext)
            } catch (_: WireError) {
                null
            }
            when {
                content is Content.ControlEvent && content.event == CALL_KEY_ASK_EVENT ->
                    answerUnifiedAsk(event, conversationId, content, knownAccount)
                plaintext.size == CALL_KEY_LEN && (content == null || content is Content.Unsupported) ->
                    answerWithRunningKey(event, plaintext)
                // Anything else — content that is not the ask, or a shape this build did not
                // send — is dropped quietly, the state untouched.
            }
        } finally {
            plaintext.fill(0)
        }
    }

    /**
     * Answers the unified ask: identifies the joiner, fetches the pairwise session's secret, and
     * hands the running key over sealed under the wrapper it derives.
     */
    private suspend fun answerUnifiedAsk(
        event: CallSdp,
        conversationId: Id,
        ask: Content.ControlEvent,
        directoryAccount: Id?,
    ) {
        // The joiner's account, identified or the ask is not answered — the same line the
        // pre-unified build drew on the directory alone.
        joinerAccountOf(ask, directoryAccount) ?: return
        // The open above established the session when none existed, so a null here is a session
        // restored from a record that predates the secret's retention: decline quietly, the ask
        // is the joiner's to retry (see the module note for why that session never recovers).
        val wrapperSecret = sessionCrypto.sessionSecret(conversationId, event.fromDevice) ?: return
        try {
            answerWithRunningKey(event, wrapperSecret)
        } finally {
            wrapperSecret.fill(0)
        }
    }

    /**
     * The joiner's account off a unified ask: the ask's own `data` when it carries one, the
     * directory's answer when an older peer's ask carried none (the SDK's asks predate the field).
     *
     * The desktop's designated-rotator rule reads this fact off the ask to pick who rotates; this
     * build's rotations are roster-driven in the call manager, so the account's whole duty here is
     * attribution — a holder that cannot attribute an ask to any account is not one that can
     * answer it, which is the pre-unified build's own gate.
     */
    private fun joinerAccountOf(ask: Content.ControlEvent, directoryAccount: Id?): Id? {
        val data = ask.data ?: return directoryAccount
        if (data.size != ID_BYTE_LEN) return null
        return idFromBytes(data)
    }

    /**
     * The answer itself: one rotation, announced to the roster, then handed to the joiner sealed
     * under the wrapper key [joinSecret] derives — the pairwise session's X3DH secret for a
     * unified ask, the raw bytes a legacy ask carried.
     */
    private suspend fun answerWithRunningKey(event: CallSdp, joinSecret: ByteArray) {
        try {
            val answer = store.rotateForJoin(event.callId, joinSecret) ?: return
            try {
                val update = CallKeyUpdate(
                    callId = event.callId,
                    epoch = answer.epoch,
                    sealedKeyMaterial = answer.sealedUpdate,
                )
                rpc.call(
                    Op.CALL_KEY_UPDATE,
                    { w -> update.encode(w) },
                    { r -> Acknowledged.decode(r) },
                )
                val reply = CallSdp(
                    callId = event.callId,
                    fromDevice = deviceId,
                    toDevice = event.fromDevice,
                    sealedSdp = answer.sealedJoinDistribution,
                )
                rpc.call(Op.CALL_SDP, { w -> reply.encode(w) }, { r -> Acknowledged.decode(r) })
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // The state has advanced; the frames that did not land are the same honest loss
                // [rotateForMembership] documents. The ask is the joiner's to retry, not this
                // device's to answer twice — a second answer would rotate a second time and strand
                // the roster between two epochs.
            }
        } finally {
            joinSecret.fill(0)
        }
    }
}
