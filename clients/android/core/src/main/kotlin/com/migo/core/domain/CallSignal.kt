package com.migo.core.domain

import com.migo.core.crypto.AEAD_KEY_LEN
import com.migo.core.crypto.Aead
import com.migo.core.crypto.SymmetricKey
import com.migo.core.protocol.CallInviteEvent
import com.migo.core.protocol.CallStateEvent
import com.migo.core.wire.ID_BYTE_LEN
import com.migo.core.wire.Id
import com.migo.core.wire.idFromBytes
import com.migo.core.wire.idToBytes
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/**
 * The pure halves of the call stack: placeholder sealing, the SDP/ICE payloads it wraps, and the
 * words and numbers the call screen shows.
 *
 * A port of `clients/web/src/lib/migo/call-signal.ts`. Everything here is a pure function so a test
 * can pin it without a peer connection, an audio track, or a Compose tree. [CallsDomain] and the
 * app's call manager use them; the split exists because the two halves fail differently — a wrong
 * label is a wrong screen, a wrong seal is a wire fault — and both are cheaper to hold apart.
 *
 * # The seal, and how the key reaches the peer
 *
 * Call signaling is end-to-end on the wire: the SDP and ICE blobs a client hands the calls domain are
 * sealed, because an SDP body carries DTLS fingerprints and an ICE candidate carries network
 * addresses, and a signaling server that could read either (§165) would learn exactly what the
 * end-to-end promise exists to protect. The server relays opaque blobs and nothing else.
 *
 * The key is *per call*, minted by the caller, and distributed inside the E2EE message layer:
 * immediately before the invite, the caller sends the conversation a control event whose name is
 * [CALL_KEY_EVENT] and whose data is [encodeCallKeyEvent]'s bytes — call id and key — sealed by the
 * messaging domain's group crypto like any message content. Every device in the conversation can
 * read it, which is the right audience, because the invite names the callee *account* and any of its
 * devices may answer; the signaling server sees only another opaque message envelope. The ordering
 * is the caller's half of the contract: the key message is sent and acknowledged *before* the
 * invite, so the callee's devices hold the key before (or within moments of) the ring. The callee's
 * half is the accept path's wait in the call manager: an accept that cannot yet find the key — the
 * frames crossed, briefly — waits for it rather than failing the call.
 *
 * Why not derive the key from the pairwise session crypto instead? Because the pairwise channel is
 * per *device*, and an offer must be readable by whichever of the callee's devices answers — a key
 * the caller cannot know in advance. Re-sealing per device would need one invite blob per device and
 * a wire field for each; a per-call key in one E2EE message needs no new wire surface at all.
 *
 * Every envelope is bound to its call: the call id is the AEAD's associated data, so a blob sealed
 * for one call cannot be replayed as another call's signaling.
 *
 * # The legacy envelope
 *
 * An older build sealed nothing: its envelope carried zero key and nonce slots with the payload in
 * the clear behind them. This build still *opens* those (a call from a peer that has not upgraded is
 * better served honestly than refused), and never writes them — an old envelope's payload was
 * readable by the server, and that is a fact about the peer's build, not a format this side should
 * keep producing.
 */

/** The envelope version this build writes: real per-call encryption under the house AEAD. */
private const val SEAL_VERSION = 2.toByte()

/**
 * The version an older build wrote: a framing envelope with zero key and nonce slots and the payload
 * in the clear behind them. Opened, never written — see [openCallSignal].
 */
private const val LEGACY_SEAL_VERSION = 1.toByte()

/** Bytes of the legacy envelope before the payload: version, 32-byte key slot, 12-byte nonce slot. */
private const val LEGACY_ENVELOPE_PREFIX_BYTES = 1 + 32 + 12

/** An envelope this build cannot read: wrong version, or too short to hold its own header. */
class CallSignalFormatException(reason: String) :
    Exception("migo: unreadable call signal envelope: $reason")

/**
 * The AEAD domain of one call's signaling: the call id, so a blob sealed for one call can never open
 * as another's.
 */
private fun callSignalDomain(callId: Id): ByteArray =
    "migo-call-signal:${callId.value}".encodeToByteArray()

/**
 * Mints the per-call sealing key: 32 random bytes from the platform CSPRNG.
 *
 * One key per call, minted by the caller and carried to the conversation inside the E2EE message
 * layer (see the module doc). There is no key slot for it in the call wire frames — the frames carry
 * only the sealed blobs — which is deliberate: the key's channel is the message layer, where it is
 * already inside an end-to-end envelope, and not the relay the server owns.
 */
fun generateCallKey(): ByteArray {
    val key = SymmetricKey.generate()
    // The copy before destroy is load-bearing: expose hands back the wrapper's own array, and
    // destroy zeroes it in place — returning it directly would hand the caller 32 zero bytes.
    val bytes = key.expose().copyOf()
    key.destroy()
    return bytes
}

/**
 * Seals one signaling payload (an SDP description or an ICE batch) for one call.
 *
 * The envelope is `version || nonce || ciphertext || tag` — version 2, then the house AEAD's own
 * output — under the call's key, with the call id as associated data. Every frame of the call
 * (offer, answer, each ICE batch) seals independently under the same key with a fresh nonce, so two
 * frames never share a nonce and one frame's exposure teaches nothing about another's.
 */
fun sealCallSignal(payload: ByteArray, key: ByteArray, callId: Id): ByteArray {
    val sealed = Aead.seal(SymmetricKey.fromBytes(key), callSignalDomain(callId), payload)
    val out = ByteArray(1 + sealed.size)
    out[0] = SEAL_VERSION
    sealed.copyInto(out, 1)
    return out
}

/**
 * Opens a sealed signaling payload; the inverse of [sealCallSignal].
 *
 * A version this build does not know, an envelope too short to hold its own header, or a body that
 * does not authenticate under the call's key throws [CallSignalFormatException] rather than
 * returning nonsense bytes a media stack would choke on downstream with a worse error. The one
 * exception is the legacy version-1 envelope (see the module doc): its payload was never encrypted,
 * so it is handed back as it arrived.
 */
fun openCallSignal(sealed: ByteArray, key: ByteArray, callId: Id): ByteArray {
    if (sealed.isEmpty()) {
        throw CallSignalFormatException("empty")
    }
    val version = sealed[0]
    if (version == LEGACY_SEAL_VERSION) {
        if (sealed.size < LEGACY_ENVELOPE_PREFIX_BYTES) {
            throw CallSignalFormatException("shorter than its own header")
        }
        return sealed.copyOfRange(LEGACY_ENVELOPE_PREFIX_BYTES, sealed.size)
    }
    if (version != SEAL_VERSION) {
        throw CallSignalFormatException("version $version")
    }
    return try {
        Aead.open(SymmetricKey.fromBytes(key), callSignalDomain(callId), sealed.copyOfRange(1, sealed.size))
    } catch (_: Exception) {
        // The AEAD refuses wrong key, wrong call, and edited bytes identically; the signaling caller
        // needs one fact — this frame is not readable — not which of the three it was.
        throw CallSignalFormatException("body did not open under the call key")
    }
}

// --- the call key's channel: the E2EE message layer ---

/**
 * The control-event name that carries a call's sealing key to the conversation.
 *
 * It rides a control event sent through the messaging domain, so it is sealed by the group crypto
 * like any message content and the server never sees the key. Only this exact event is treated as
 * key material on receipt — the same rule the SDK applies to `sender-key`.
 */
const val CALL_KEY_EVENT: String = "call-key"

/**
 * The bytes a call-key control event carries: the 16 wire bytes of the call id, then the 32-byte
 * key. Fixed widths both, so decoding is two slices with no length prefix to parse.
 */
fun encodeCallKeyEvent(callId: Id, key: ByteArray): ByteArray {
    val id = idToBytes(callId)
    val out = ByteArray(id.size + key.size)
    id.copyInto(out, 0)
    key.copyInto(out, id.size)
    return out
}

/**
 * Decodes a call-key control event's data, or `null` when it is not one: wrong width, or a call id
 * this build cannot parse.
 *
 * `null` rather than a throw, because the data arrives from a peer's client over the E2EE message
 * layer and a malformed event is dropped like any other undecryptable noise — there is no call to
 * fail over it, only a key that never arrives.
 */
fun decodeCallKeyEvent(data: ByteArray): Pair<Id, ByteArray>? {
    if (data.size != ID_BYTE_LEN + AEAD_KEY_LEN) {
        return null
    }
    return try {
        idFromBytes(data.copyOfRange(0, ID_BYTE_LEN)) to data.copyOfRange(ID_BYTE_LEN, data.size)
    } catch (_: Exception) {
        null
    }
}

// --- the sealed payloads ---

/**
 * A local or remote SDP description as it travels inside the seal: type plus the SDP text.
 *
 * [type] is `"offer"`, `"answer"`, `"pranswer"` or `"rollback"` — the SDP session description
 * types, kept as text because the seal is the only consumer and a typo must fail loudly at the
 * media stack that cannot apply it, not silently at a codec that maps it to a number.
 */
@Serializable
data class SdpDescription(val type: String, val sdp: String)

/**
 * The exact JSON codec for the sealed payloads. No configuration beyond the two flags that keep
 * the bytes shaped like the web build's `JSON.stringify` output, because the seal is a
 * cross-client contract: nulls are omitted rather than written as `"null"` (web's `undefined`
 * fields never reach the string), and defaulted fields are always written (a `sdpMLineIndex` of
 * zero is a fact, not an absence). Unknown keys are ignored on read — the AEAD tag has already
 * authenticated the bytes; a field this build does not know is a future build's, not an
 * attacker's. Anything else would change the sealed bytes and break the contract with the web
 * build, which seals the same JSON shape.
 */
private val callSignalJson = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    encodeDefaults = true
}

/** The bytes of an SDP description, JSON-encoded — the shape a media stack's setRemote accepts back. */
fun encodeSdpDescription(description: SdpDescription): ByteArray =
    callSignalJson.encodeToString(SdpDescription.serializer(), description).encodeToByteArray()

/** Decodes the bytes of an SDP description, refusing anything that is not one. */
fun decodeSdpDescription(bytes: ByteArray): SdpDescription =
    try {
        callSignalJson.decodeFromString(SdpDescription.serializer(), bytes.decodeToString())
    } catch (_: Exception) {
        throw CallSignalFormatException("not an SDP description")
    }

/**
 * One ICE candidate as it travels inside the seal: the fields a media stack needs to reconstruct
 * the candidate, and nothing else.
 *
 * The JSON counterpart of the web build's `RTCIceCandidateInit` — the same field names, so a batch
 * sealed by one build opens on the other — with [candidate] and [sdpMid] nullable because a
 * candidate end-of-gathering notification carries neither.
 */
@Serializable
data class IceCandidateJson(
    val candidate: String? = null,
    val sdpMid: String? = null,
    val sdpMLineIndex: Int = 0,
    val usernameFragment: String? = null,
)

/**
 * The bytes of an ICE batch: a JSON array of candidates, one relay per gathering run — one frame per
 * candidate is exactly the signaling storm the wire's batch field exists to avoid (§165).
 */
fun encodeIceBatch(candidates: List<IceCandidateJson>): ByteArray =
    callSignalJson
        .encodeToString(ListSerializer(IceCandidateJson.serializer()), candidates)
        .encodeToByteArray()

/** Decodes an ICE batch, refusing anything that is not an array of candidates. */
fun decodeIceBatch(bytes: ByteArray): List<IceCandidateJson> =
    try {
        callSignalJson.decodeFromString(ListSerializer(IceCandidateJson.serializer()), bytes.decodeToString())
    } catch (_: Exception) {
        throw CallSignalFormatException("not an ICE batch")
    }

// --- the words and numbers ---

/**
 * A call duration as `M:SS`, the timer on a connected call and the total on an ended one.
 *
 * Minutes are unbounded (a two-hour call reads `120:05`, still one glance); anything negative or
 * not yet measurable reads as `0:00` rather than a sign or nonsense a screen cannot hide.
 */
fun formatCallDuration(elapsedMs: Long): String {
    val totalSeconds = (elapsedMs / 1000).coerceAtLeast(0)
    val minutes = totalSeconds / 60
    val seconds = totalSeconds % 60
    return "$minutes:${seconds.toString().padStart(2, '0')}"
}

/**
 * The six states a call screen shows, per the product requirement (section 180): the wire's five,
 * plus *degraded* — a connected call whose quality has dropped far enough that video is off — which
 * is a client-side judgement from live media statistics, never a signaling fact.
 */
enum class CallDisplayState {
    Ringing,
    Connecting,
    Connected,
    Reconnecting,
    Degraded,
    Ended,
}

/** Maps a tracked call's state (plus this client's quality judgement) onto the state the screen shows. */
fun displayStateOf(state: CallState, degraded: Boolean): CallDisplayState {
    if (state == CallState.Connected && degraded) {
        return CallDisplayState.Degraded
    }
    return when (state) {
        CallState.Ringing -> CallDisplayState.Ringing
        CallState.Connecting -> CallDisplayState.Connecting
        CallState.Connected -> CallDisplayState.Connected
        CallState.Reconnecting -> CallDisplayState.Reconnecting
        CallState.Ended -> CallDisplayState.Ended
    }
}

/** The status line for a call state, when the screen is not saying something more specific. */
fun callStateLabel(state: CallDisplayState): String =
    when (state) {
        CallDisplayState.Ringing -> "Ringing"
        CallDisplayState.Connecting -> "Connecting…"
        CallDisplayState.Connected -> "Connected"
        CallDisplayState.Reconnecting -> "Reconnecting…"
        // Section 180's degraded is "connected, but quality fell until video turned off".
        CallDisplayState.Degraded -> "Poor connection — video paused"
        CallDisplayState.Ended -> "Call ended"
    }

/**
 * The reason line an ended call shows.
 *
 * Section 180 requires the reasons to be told apart: a declined call and a failed call and a network
 * death are different facts a user needs before deciding to call back. `ByCaller` and `ByCallee`
 * both read as a plain end — whose button ended it is not a fact worth a line.
 */
fun endReasonLabel(reason: CallEndReason?): String =
    when (reason) {
        CallEndReason.ByCaller -> "Call ended"
        CallEndReason.ByCallee -> "Call ended"
        CallEndReason.Declined -> "Declined"
        CallEndReason.NoAnswer -> "No answer"
        CallEndReason.Failed -> "Failed to connect"
        CallEndReason.Network -> "Connection lost"
        CallEndReason.Busy -> "Busy"
        null -> "Call ended"
    }

/** What a call carries: audio only, or audio plus video. */
enum class CallMediaKind(val wire: Long) {
    Audio(0),
    Video(1),
    ;

    companion object {
        /**
         * Narrows a wire media kind. A kind this build does not know degrades to audio: the call
         * still happens, as the honest lesser version of itself, rather than being dropped over a
         * label.
         */
        fun fromWire(wire: Long): CallMediaKind = if (wire == Video.wire) Video else Audio
    }
}

/** Why a call ended. The wire's reasons, from `CallEnd.reason` and `CallInviteResult.status`. */
enum class CallEndReason(val wire: Long) {
    /** The caller hung up. */
    ByCaller(0),

    /** The callee hung up. */
    ByCallee(1),

    /** The callee declined the invite. */
    Declined(2),

    /** Nobody answered before the invite expired. */
    NoAnswer(3),

    /** The call never connected (media failure, unreachable peer). */
    Failed(4),

    /** The network gave out and the reconnect window closed. */
    Network(5),

    /** The callee's devices were occupied — a decline reason, not a hang-up. */
    Busy(6),
    ;

    companion object {
        /** Narrows a wire reason; an unknown value yields `null`, never a guess. */
        fun fromWire(wire: Long): CallEndReason? = entries.firstOrNull { it.wire == wire }
    }
}

/** Why a callee declined. `Busy` answers faster than a ring that can never be picked up. */
enum class CallDeclineReason(val wire: Long) {
    Busy(0),
    Declined(1),
}

/** "voice call" or "video call", for the incoming screen's second line and the accept button's label. */
fun mediaKindLabel(kind: CallMediaKind): String = if (kind == CallMediaKind.Video) "video call" else "voice call"

// --- the invite's verdict, before any call exists ---

/** The wire's `CallInviteResult.status`: the invite is out and the callee is being rung. */
const val INVITE_RINGING: Long = 0

/** The wire's `CallInviteResult.status`: a callee (or their settings) refused the invite. */
const val INVITE_DECLINED: Long = 1

/** The wire's `CallInviteResult.status`: the invite expired before anyone answered. */
const val INVITE_EXPIRED: Long = 2

/** The wire's `CallInviteResult.status`: a block or call policy excludes the caller. */
const val INVITE_BLOCKED: Long = 3

/** The wire's `CallInviteResult.status`: the callee's devices were occupied (a busy decline). */
const val INVITE_BUSY: Long = 4

/**
 * The ended reason for an invite that never rang.
 *
 * Expired is [CallEndReason.NoAnswer]; busy is [CallEndReason.Busy]; every other refusal is
 * [CallEndReason.Declined], because the wire's reason enum has no Blocked member. The distinction
 * the wire did draw — blocked — rides on the tracked call as its raw `inviteStatus` for the screen
 * to read through [endedReasonLine].
 */
fun inviteEndReason(status: Long): CallEndReason =
    when (status) {
        INVITE_EXPIRED -> CallEndReason.NoAnswer
        INVITE_BUSY -> CallEndReason.Busy
        else -> CallEndReason.Declined
    }

/**
 * The reason line an ended call shows, including the one distinction the reason enum cannot carry: a
 * blocked refusal.
 *
 * The server answers a block and a call policy that excludes the caller with the same status,
 * deliberately, so the word must not say which it was — but it must still differ from a human's
 * "Declined", which is a different fact before the caller decides what to do next.
 */
fun endedReasonLine(inviteStatus: Long?, endReason: CallEndReason?): String =
    if (inviteStatus == INVITE_BLOCKED) {
        "Unavailable"
    } else {
        endReasonLabel(endReason)
    }

// --- the ring's lifecycle ---

/**
 * How long the caller's own ring screen waits before ending the call locally, measured from the
 * invite reply's `expiresAt`.
 *
 * The server sweeps its own expiry, but its `Ended` event can be late or lost; this mirror is what
 * guarantees a "Calling…" screen never outlives the invite. Clamped at zero so a reply that arrived
 * late (or a clock that disagrees with the server) fires the mirror at once rather than scheduling a
 * negative delay.
 */
fun ringTimeoutMs(expiresAt: Long, now: Long): Long = (expiresAt - now).coerceAtLeast(0)

/**
 * Whether a state event says the ringing inbound call was answered on another device: a `Connecting`
 * or `Connected` transition for the call this device is being rung for and has not answered itself.
 *
 * The server rings every device on the account and publishes the answer to both parties, so a
 * sibling device hears the call move on without it. Without this check that device keeps ringing
 * until the invite expires — for a call that is already being spoken on elsewhere.
 */
fun answersRingingCall(event: CallStateEvent, ringingCallId: Id?): Boolean {
    if (ringingCallId == null || event.callId != ringingCallId) {
        return false
    }
    val state = CallState.fromWire(event.state)
    return state == CallState.Connecting || state == CallState.Connected
}

/**
 * Whether a state event retires the inbound ring it names: an `Ended` for the call this device is
 * being rung for and has not answered.
 *
 * The caller canceled (or the invite expired on the server); without this check the callee's ring
 * outlives the call it belongs to, because a ringing inbound call is tracked only as the incoming
 * invite — there is no active call for a state event to land on.
 */
fun endsRingingCall(event: CallStateEvent, ringingCallId: Id?): Boolean =
    ringingCallId != null &&
        event.callId == ringingCallId &&
        CallState.fromWire(event.state) == CallState.Ended

/** What the call manager does with an inbound invite. */
sealed interface IncomingInviteDisposition {
    /** Ring this device: the invite is fresh and the device is free. */
    data object Ring : IncomingInviteDisposition

    /** Expected under at-least-once delivery, or already dead: never news, never declined. */
    data object Ignore : IncomingInviteDisposition

    /** A different call while this device is occupied: answer Busy, which stops the new caller's
     * ring without implying a human refusal. */
    data object DeclineBusy : IncomingInviteDisposition
}

/**
 * Places an inbound invite against this device's occupancy.
 *
 * At-least-once delivery of Critical frames makes a redelivered invite expected, not news: one
 * naming the ring already showing — or the call already answered, or already over — is ignored,
 * because declining it would hang up the very call the user is being rung for or is in. A
 * *different* call while this device is occupied is answered `Busy`. An invite that expired in
 * flight (a push that woke this device too late) rings nobody at all.
 */
fun incomingInviteDisposition(
    event: CallInviteEvent,
    ringingCallId: Id?,
    activeCallId: Id?,
    busy: Boolean,
    now: Long,
): IncomingInviteDisposition {
    if (event.expiresAt <= now) {
        return IncomingInviteDisposition.Ignore
    }
    if (event.callId == ringingCallId || event.callId == activeCallId) {
        return IncomingInviteDisposition.Ignore
    }
    if (busy || ringingCallId != null) {
        return IncomingInviteDisposition.DeclineBusy
    }
    return IncomingInviteDisposition.Ring
}
