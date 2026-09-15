package com.migo.core.domain

import com.migo.core.crypto.CALL_KEY_LEN
import com.migo.core.crypto.CallKeyState
import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.CryptoErrorKind
import com.migo.core.crypto.Csprng
import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.CallKeyUpdate
import com.migo.core.protocol.CallRenegotiate
import com.migo.core.protocol.CallSdp
import com.migo.core.protocol.Op
import com.migo.core.session.SessionCrypto
import com.migo.core.wire.Id
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

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
 *    the current epoch and key sealed under a wrapper key derived from the ask's secret
 *    ([CallKeyState.sealedJoinDistribution]). The joiner installs it as its baseline
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
 * # The wrapper key's secret, and this build's reading of it
 *
 * The Rust reference derives the wrapper key from "the pairwise session secret the distributor
 * shares with the joiner". This client's Double Ratchet exposes no such stable secret by design —
 * the root key advances asymmetrically and the X3DH seed is destroyed when the ratchet is built —
 * so this build establishes one for the purpose: the joiner mints a fresh 32-byte secret, carries
 * it to the seated participant inside the pairwise Double Ratchet as the ask's payload, and both
 * sides derive the wrapper key from it under `migo-call-join-v1` with the call id as salt, exactly
 * the derivation the reference's join distribution specifies. The wrapper is therefore anchored to
 * the pairwise channel (only the two devices can read the secret) without inventing an export the
 * ratchet deliberately does not offer. Cross-client agreement on this reading is pending the
 * central reconciliation of the same section; the derivation and the sealed-distribution layout
 * themselves are byte-for-byte the reference's.
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

    /** One outstanding join ask: the secret only this device and the asked holder share. */
    private class PendingJoinAsk(
        val conversationId: Id,
        val secret: ByteArray,
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
     * the secret it was sealed under.
     */
    fun beginJoinAsk(callId: Id, conversationId: Id, secret: ByteArray, holderDevice: Id) {
        lock.withLock { asks[callId] = PendingJoinAsk(conversationId, secret.copyOf(), holderDevice) }
    }

    /** Drops a join ask that never got answered — a retry mints a fresh secret and a fresh ask. */
    fun clearJoinAsk(callId: Id) {
        lock.withLock {
            asks.remove(callId)?.secret?.fill(0)
        }
    }

    /**
     * Tries to complete a pending ask with the sealed answer that just arrived.
     *
     * The answer must open under the ask's own secret and be bound to the call; a blob that does
     * not is not the answer (a future shape this build does not know), the ask stands, and false
     * comes back so the caller can fall through to its other handling.
     */
    fun completeJoinAsk(callId: Id, sealedAnswer: ByteArray): Boolean = lock.withLock {
        val ask = asks[callId] ?: return false
        val state = try {
            CallKeyState.fromJoinDistribution(ask.secret, callId, sealedAnswer)
        } catch (_: CryptoError) {
            return false
        }
        asks.remove(callId)
        ask.secret.fill(0)
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
     * distribution* is sealed under the wrapper key the ask's secret derives, for the joiner
     * alone. One rotation, not two, because two would leave the roster an epoch behind the joiner
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

    /** Drops a call's state: the seat is gone, and so is everything the key was for. */
    fun forget(callId: Id) {
        lock.withLock {
            calls.remove(callId)?.state?.destroy()
            asks.remove(callId)?.secret?.fill(0)
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
     * The ask's payload is the fresh 32-byte secret the wrapper key derives from, carried to the
     * holder inside the pairwise Double Ratchet (see the module note: the ratchet exposes no stable
     * session secret, so one is established for the purpose). It rides a `CALL_RENEGOTIATE`, which
     * the relay projects to a `CALL_SDP` on the holder's side.
     */
    suspend fun requestJoinKey(roster: GroupCallRoster) {
        if (store.holdsKey(roster.callId) || store.pendingAskHolder(roster.callId) != null) return
        val holder = roster.participants.firstOrNull { it.userId != roster.userId } ?: return
        val secret = Csprng.bytes(CALL_KEY_LEN)
        store.beginJoinAsk(roster.callId, roster.conversationId, secret, holder.deviceId)
        try {
            // A copy for the seal, zeroed after — the store's copy is the one that must survive
            // until the answer opens it.
            val ask = secret.copyOf()
            val sealed = try {
                sessionCrypto.seal(roster.conversationId, holder.userId, holder.deviceId, ask)
            } finally {
                ask.fill(0)
            }
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
            // The ask never left; dropping it lets a later roster or retry mint a fresh one rather
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
                store.completeJoinAsk(event.callId, event.sealedSdp)
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
     * The rotation is announced to the rest of the roster as a `CALL_KEY_UPDATE` before the joiner
     * gets its answer, because the server fans an update to every seat and a seat that missed the
     * join rotation is a seat stranded an epoch behind the joiner it was meant to stay in step
     * with. The ask opens under the pairwise session this device shares with the joiner — the
     * account half of that open is bookkeeping only, the same as every pairwise open here — and a
     * fresh 32-byte secret is all the payload is, so anything that opens to another width is a
     * shape this build did not send and is dropped with the state untouched.
     */
    private suspend fun answerJoinAsk(event: CallSdp) {
        val conversationId = store.conversationOf(event.callId) ?: return
        val joinerAccount = directory.accountOfDevice(conversationId, event.fromDevice) ?: return
        val secret = try {
            sessionCrypto.open(conversationId, joinerAccount, event.fromDevice, event.sealedSdp)
        } catch (_: Throwable) {
            return
        }
        if (secret.size != CALL_KEY_LEN) {
            secret.fill(0)
            return
        }
        try {
            val answer = store.rotateForJoin(event.callId, secret) ?: return
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
            secret.fill(0)
        }
    }
}
