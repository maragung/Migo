'use client';

/**
 * The pure halves of the web call stack: placeholder sealing, the SDP/ICE payloads it wraps, and
 * the words and numbers the call screen shows.
 *
 * Everything here is a pure function so a test can pin it without a peer connection, a media
 * stream, or a React tree. The manager ({@link ./call-manager.tsx}) and the overlay use them; the
 * split exists because the two halves fail differently — a wrong label is a wrong screen, a wrong
 * seal is a wire fault — and both are cheaper to hold apart.
 *
 * # The seal, and how the key reaches the peer
 *
 * Call signaling is end-to-end on the wire: the SDP and ICE blobs a client hands the SDK are
 * sealed, because an SDP body carries DTLS fingerprints and an ICE candidate carries network
 * addresses, and a signaling server that could read either (§165) would learn exactly what the
 * end-to-end promise exists to protect. The server relays opaque blobs and nothing else.
 *
 * The key is *per call*, minted by the caller, and distributed inside the E2EE message layer:
 * immediately before the invite, the caller sends the conversation a {@link
 * ContentType.ControlEvent} whose data is {@link encodeCallKeyEvent}'s bytes — call id and key —
 * sealed by the messaging domain's group crypto like any message content. Every device in the
 * conversation can read it, which is the right audience, because the invite names the callee
 * *account* and any of its devices may answer; the signaling server sees only another opaque
 * message envelope. The ordering is the caller's half of the contract: the key message is sent
 * and acknowledged *before* the invite, so the callee's devices hold the key before (or within
 * moments of) the ring. The callee's half is the wait in the call manager: an accept that cannot
 * yet find the key — the frames crossed, briefly — waits for it rather than failing the call.
 *
 * Why not derive the key from the pairwise session crypto instead? Because the pairwise channel
 * (the Double Ratchet behind sender-key distribution) is per *device*, and an offer must be
 * readable by whichever of the callee's devices answers — a key the caller cannot know in
 * advance. Re-sealing per device would need one invite blob per device and a wire field for each;
 * a per-call key in one E2EE message needs no new wire surface at all.
 *
 * Every envelope is bound to its call: the call id is the AEAD's associated data, so a blob
 * sealed for one call cannot be replayed as another call's signaling.
 *
 * # The legacy envelope
 *
 * An older build sealed nothing: its envelope carried zero key and nonce slots with the payload
 * in the clear behind them. This build still *opens* those (a call from a peer that has not
 * upgraded is better served honestly than refused), and never writes them — an old envelope's
 * payload was readable by the server, and that is a fact about the peer's build, not a format
 * this side should keep producing.
 */

import { CallEndReason, CallMediaKind, CallState, aead, wire } from '@migo/sdk';
import type { ActiveCall, CallInviteEvent, CallStateEvent, Id } from '@migo/sdk';

/** The envelope version this build writes: real per-call encryption under the house AEAD. */
const SEAL_VERSION = 2;
/**
 * The version an older build wrote: a framing envelope with zero key and nonce slots and the
 * payload in the clear behind them. Opened, never written — see {@link openCallSignal}.
 */
const LEGACY_SEAL_VERSION = 1;
/** Bytes of the legacy envelope before the payload: version, 32-byte key slot, 12-byte nonce slot. */
const LEGACY_ENVELOPE_PREFIX_BYTES = 1 + 32 + 12;

/** An envelope this build cannot read: wrong version, or too short to hold its own header. */
export class CallSignalFormatError extends Error {
  constructor(reason: string) {
    super(`migo: unreadable call signal envelope: ${reason}`);
    this.name = 'CallSignalFormatError';
  }
}

/**
 * The AEAD domain of one call's signaling: the call id, so a blob sealed for one call can never
 * open as another's.
 */
function callSignalDomain(callId: Id): Uint8Array {
  return new TextEncoder().encode(`migo-call-signal:${callId}`);
}

/**
 * Mints the per-call sealing key: 32 random bytes from the platform CSPRNG.
 *
 * One key per call, minted by the caller and carried to the conversation inside the E2EE message
 * layer (see the module doc). There is no key slot for it in the call wire frames — the frames
 * carry only the sealed blobs — which is deliberate: the key's channel is the message layer,
 * where it is already inside an end-to-end envelope, and not the relay the server owns.
 */
export function generateCallKey(): Uint8Array {
  const key = aead.SymmetricKey.generate();
  const bytes = key.expose().slice();
  key.destroy();
  return bytes;
}

/**
 * Seals one signaling payload (an SDP description or an ICE batch) for one call.
 *
 * The envelope is `version || nonce || ciphertext || tag` — version 2, then the house AEAD's own
 * output — under the call's key, with the call id as associated data. Every frame of the call
 * (offer, answer, each ICE batch) seals independently under the same key with a fresh nonce, so
 * two frames never share a nonce and one frame's exposure teaches nothing about another's.
 */
export function sealCallSignal(payload: Uint8Array, key: Uint8Array, callId: Id): Uint8Array {
  const sealed = aead.seal(aead.SymmetricKey.fromBytes(key), callSignalDomain(callId), payload);
  const out = new Uint8Array(1 + sealed.length);
  out[0] = SEAL_VERSION;
  out.set(sealed, 1);
  return out;
}

/**
 * Opens a sealed signaling payload; the inverse of {@link sealCallSignal}.
 *
 * A version this build does not know, an envelope too short to hold its own header, or a body that
 * does not authenticate under the call's key throws {@link CallSignalFormatError} rather than
 * returning nonsense bytes a WebRTC stack would choke on downstream with a worse error. The one
 * exception is the legacy version-1 envelope (see the module doc): its payload was never
 * encrypted, so it is handed back as it arrived.
 */
export function openCallSignal(sealed: Uint8Array, key: Uint8Array, callId: Id): Uint8Array {
  if (sealed.length < 1) {
    throw new CallSignalFormatError('empty');
  }
  const version = sealed[0] ?? 0;
  if (version === LEGACY_SEAL_VERSION) {
    if (sealed.length < LEGACY_ENVELOPE_PREFIX_BYTES) {
      throw new CallSignalFormatError('shorter than its own header');
    }
    return sealed.slice(LEGACY_ENVELOPE_PREFIX_BYTES);
  }
  if (version !== SEAL_VERSION) {
    throw new CallSignalFormatError(`version ${version}`);
  }
  try {
    return aead.open(
      aead.SymmetricKey.fromBytes(key),
      callSignalDomain(callId),
      sealed.subarray(1),
    );
  } catch {
    // The AEAD refuses wrong key, wrong call, and edited bytes identically; the signaling caller
    // needs one fact — this frame is not readable — not which of the three it was.
    throw new CallSignalFormatError('body did not open under the call key');
  }
}

// --- the call key's channel: the E2EE message layer ---

/**
 * The control-event name that carries a call's sealing key to the conversation.
 *
 * It rides a {@link ContentType.ControlEvent} sent through the messaging domain, so it is sealed
 * by the group crypto like any message content and the server never sees the key. Only this exact
 * event is treated as key material on receipt — the same rule the SDK applies to `sender-key`.
 */
export const CALL_KEY_EVENT = 'call-key';

/**
 * The bytes a call-key control event carries: the 16 wire bytes of the call id, then the 32-byte
 * key. Fixed widths both, so decoding is two slices with no length prefix to parse.
 */
export function encodeCallKeyEvent(callId: Id, key: Uint8Array): Uint8Array {
  const id = wire.idToBytes(callId);
  const out = new Uint8Array(id.length + key.length);
  out.set(id, 0);
  out.set(key, id.length);
  return out;
}

/**
 * Decodes a call-key control event's data, or `null` when it is not one: wrong width, or a call id
 * this build cannot parse.
 *
 * `null` rather than a throw, because the data arrives from a peer's client over the E2EE message
 * layer and a malformed event is dropped like any other undecryptable noise — there is no call to
 * fail over it, only a key that never arrives.
 */
export function decodeCallKeyEvent(data: Uint8Array): { callId: Id; key: Uint8Array } | null {
  if (data.length !== wire.ID_BYTE_LEN + 32) {
    return null;
  }
  try {
    return {
      callId: wire.idFromBytes(data.subarray(0, wire.ID_BYTE_LEN)),
      key: data.slice(wire.ID_BYTE_LEN),
    };
  } catch {
    return null;
  }
}

/** A local or remote SDP description as it travels inside the seal: type plus the SDP text. */
export type SdpDescription = { type: 'offer' | 'answer' | 'pranswer' | 'rollback'; sdp: string };

/** The bytes of an SDP description, JSON-encoded — the shape `setRemoteDescription` accepts back. */
export function encodeSdpDescription(description: SdpDescription): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(description));
}

/** Decodes the bytes of an SDP description, refusing anything that is not one. */
export function decodeSdpDescription(bytes: Uint8Array): SdpDescription {
  const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes));
  if (
    typeof parsed !== 'object' ||
    parsed === null ||
    typeof (parsed as { sdp?: unknown }).sdp !== 'string' ||
    typeof (parsed as { type?: unknown }).type !== 'string'
  ) {
    throw new CallSignalFormatError('not an SDP description');
  }
  return parsed as SdpDescription;
}

/**
 * The bytes of an ICE batch: a JSON array of candidate inits, one relay per gathering run —
 * one frame per candidate is exactly the signaling storm the wire's batch field exists to avoid.
 */
export function encodeIceBatch(candidates: RTCIceCandidateInit[]): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(candidates));
}

/** Decodes an ICE batch, refusing anything that is not an array of candidate inits. */
export function decodeIceBatch(bytes: Uint8Array): RTCIceCandidateInit[] {
  const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes));
  if (!Array.isArray(parsed)) {
    throw new CallSignalFormatError('not an ICE batch');
  }
  return parsed as RTCIceCandidateInit[];
}

/**
 * A call duration as `M:SS`, the timer on a connected call and the total on an ended one.
 *
 * Minutes are unbounded (a two-hour call reads `120:05`, still one glance); anything negative or
 * not yet measurable reads as `0:00` rather than a sign or `NaN` a stylesheet cannot hide.
 */
export function formatCallDuration(elapsedMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(elapsedMs / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

/**
 * The six states a call screen shows, per the product requirement (section 180): the wire's five,
 * plus *degraded* — a connected call whose quality has dropped far enough that video is off —
 * which is a client-side judgement from live media statistics, never a signaling fact.
 */
export type CallDisplayState =
  'ringing' | 'connecting' | 'connected' | 'reconnecting' | 'degraded' | 'ended';

/** Maps a tracked call (plus this client's quality judgement) onto the state the screen shows. */
export function displayStateOf(call: ActiveCall, degraded: boolean): CallDisplayState {
  if (call.state === CallState.Connected && degraded) {
    return 'degraded';
  }
  switch (call.state) {
    case CallState.Ringing:
      return 'ringing';
    case CallState.Connecting:
      return 'connecting';
    case CallState.Connected:
      return 'connected';
    case CallState.Reconnecting:
      return 'reconnecting';
    case CallState.Ended:
      return 'ended';
    default: {
      const unreachable: never = call.state;
      return unreachable;
    }
  }
}

/** The status line for a call state, when the screen is not saying something more specific. */
export function callStateLabel(state: CallDisplayState): string {
  switch (state) {
    case 'ringing':
      return 'Ringing';
    case 'connecting':
      return 'Connecting…';
    case 'connected':
      return 'Connected';
    case 'reconnecting':
      return 'Reconnecting…';
    case 'degraded':
      // Section 180's degraded is "connected, but quality fell until video turned off".
      return 'Poor connection — video paused';
    case 'ended':
      return 'Call ended';
    default: {
      const unreachable: never = state;
      return unreachable;
    }
  }
}

/**
 * The reason line an ended call shows.
 *
 * Section 180 requires the reasons to be told apart: a declined call and a failed call and a
 * network death are different facts a user needs before deciding to call back. `ByCaller` and
 * `ByCallee` both read as a plain end — whose button ended it is not a fact worth a line.
 */
export function endReasonLabel(reason: CallEndReason | undefined): string {
  switch (reason) {
    case CallEndReason.ByCaller:
      return 'Call ended';
    case CallEndReason.ByCallee:
      return 'Call ended';
    case CallEndReason.Declined:
      return 'Declined';
    case CallEndReason.NoAnswer:
      return 'No answer';
    case CallEndReason.Failed:
      return 'Failed to connect';
    case CallEndReason.Network:
      return 'Connection lost';
    case CallEndReason.Busy:
      return 'Busy';
    default:
      return 'Call ended';
  }
}

// --- the invite's verdict, before any call exists ---

/** The wire's `CallInviteResult.status`: the invite is out and the callee is being rung. */
export const INVITE_RINGING = 0;
/** The wire's `CallInviteResult.status`: a callee (or their settings) refused the invite. */
export const INVITE_DECLINED = 1;
/** The wire's `CallInviteResult.status`: the invite expired before anyone answered. */
export const INVITE_EXPIRED = 2;
/** The wire's `CallInviteResult.status`: a block or call policy excludes the caller. */
export const INVITE_BLOCKED = 3;
/** The wire's `CallInviteResult.status`: the callee's devices were occupied (a busy decline). */
export const INVITE_BUSY = 4;

/**
 * The ended reason for an invite that never rang.
 *
 * Expired is {@link CallEndReason.NoAnswer}; busy is {@link CallEndReason.Busy}; every other
 * refusal is {@link CallEndReason.Declined}, because the wire's reason enum has no Blocked
 * member. The distinction the wire did draw — blocked — rides on the tracked call as its raw
 * `inviteStatus` for the screen to read through {@link endedReasonLine}.
 */
export function inviteEndReason(status: number): CallEndReason {
  if (status === INVITE_EXPIRED) {
    return CallEndReason.NoAnswer;
  }
  if (status === INVITE_BUSY) {
    return CallEndReason.Busy;
  }
  return CallEndReason.Declined;
}

/**
 * The reason line an ended call shows, including the one distinction the reason enum cannot
 * carry: a blocked refusal.
 *
 * The server answers a block and a call policy that excludes the caller with the same status,
 * deliberately, so the word must not say which it was — but it must still differ from a human's
 * "Declined", which is a different fact before the caller decides what to do next.
 */
export function endedReasonLine(call: ActiveCall): string {
  if (call.inviteStatus === INVITE_BLOCKED) {
    return 'Unavailable';
  }
  return endReasonLabel(call.endReason);
}

// --- the ring's lifecycle ---

/**
 * How long the caller's own ring screen waits before ending the call locally, measured from the
 * invite reply's `expiresAt`.
 *
 * The server sweeps its own expiry, but its `Ended` event can be late or lost; this mirror is
 * what guarantees a "Calling…" screen never outlives the invite. Clamped at zero so a reply that
 * arrived late (or a clock that disagrees with the server) fires the mirror at once rather than
 * scheduling a negative delay.
 */
export function ringTimeoutMs(expiresAt: number, now: number): number {
  return Math.max(0, expiresAt - now);
}

/**
 * Whether a state event says the ringing inbound call was answered on another device: a
 * `Connecting` or `Connected` transition for the call this device is being rung for and has not
 * answered itself.
 *
 * The server rings every device on the account and publishes the answer to both parties, so a
 * sibling device hears the call move on without it. Without this check that device keeps ringing
 * until the invite expires — for a call that is already being spoken on elsewhere.
 */
export function answersRingingCall(event: CallStateEvent, ringingCallId: Id | null): boolean {
  if (ringingCallId === null || event.callId !== ringingCallId) {
    return false;
  }
  const state = callStateOf(event.state);
  return state === CallState.Connecting || state === CallState.Connected;
}

/**
 * Whether a state event retires the inbound ring it names: an `Ended` for the call this device
 * is being rung for and has not answered.
 *
 * The caller canceled (or the invite expired on the server); without this check the callee's
 * ring outlives the call it belongs to, because a ringing inbound call is tracked only as the
 * incoming invite — there is no active call for a state event to land on.
 */
export function endsRingingCall(event: CallStateEvent, ringingCallId: Id | null): boolean {
  return (
    ringingCallId !== null &&
    event.callId === ringingCallId &&
    callStateOf(event.state) === CallState.Ended
  );
}

/** What the call manager does with an inbound invite. */
export type IncomingInviteDisposition = 'ring' | 'ignore' | 'decline-busy';

/** What the manager needs to know about this device's calls to place a new invite. */
export interface IncomingCallOccupancy {
  /** The call currently ringing inbound, when one is showing. */
  ringingCallId: Id | null;
  /** The tracked call, live or just ended and still on screen. */
  activeCallId: Id | null;
  /** Whether the tracked call still occupies this device — an ended screen does not. */
  busy: boolean;
}

/**
 * Places an inbound invite against this device's occupancy.
 *
 * At-least-once delivery of Critical frames makes a redelivered invite expected, not news: one
 * naming the ring already showing — or the call already answered, or already over — is ignored,
 * because declining it would hang up the very call the user is being rung for or is in. A
 * *different* call while this device is occupied is answered `Busy`, which stops the new caller's
 * ring without implying a human refusal. An invite that expired in flight (a push that woke this
 * device too late) rings nobody at all.
 */
export function incomingInviteDisposition(
  event: CallInviteEvent,
  occupancy: IncomingCallOccupancy,
  now: number,
): IncomingInviteDisposition {
  if (event.expiresAt <= now) {
    return 'ignore';
  }
  if (event.callId === occupancy.ringingCallId || event.callId === occupancy.activeCallId) {
    return 'ignore';
  }
  if (occupancy.busy || occupancy.ringingCallId !== null) {
    return 'decline-busy';
  }
  return 'ring';
}

/** "voice call" or "video call", for the incoming screen's second line and the accept button's label. */
export function mediaKindLabel(mediaKind: CallMediaKind): string {
  return mediaKind === CallMediaKind.Video ? 'video call' : 'voice call';
}

/*
 * The wire carries the call enums as bare numbers, and a number is not a state: comparing one
 * against an enum is exactly the mistake the lint rule forbids, and the honest fix is to narrow
 * once, at the boundary, where a value this build does not recognize can be refused rather than
 * guessed at. The tables below are keyed by the enum values themselves, so a future wire value
 * simply misses and yields `undefined`.
 */

const WIRE_CALL_STATES: Readonly<Record<number, CallState>> = {
  [CallState.Ringing]: CallState.Ringing,
  [CallState.Connecting]: CallState.Connecting,
  [CallState.Connected]: CallState.Connected,
  [CallState.Reconnecting]: CallState.Reconnecting,
  [CallState.Ended]: CallState.Ended,
};

/** Narrows a wire `CallStateEvent.state`; a state this build does not know yields `undefined`. */
export function callStateOf(state: number): CallState | undefined {
  return WIRE_CALL_STATES[state];
}

const WIRE_CALL_END_REASONS: Readonly<Record<number, CallEndReason>> = {
  [CallEndReason.ByCaller]: CallEndReason.ByCaller,
  [CallEndReason.ByCallee]: CallEndReason.ByCallee,
  [CallEndReason.Declined]: CallEndReason.Declined,
  [CallEndReason.NoAnswer]: CallEndReason.NoAnswer,
  [CallEndReason.Failed]: CallEndReason.Failed,
  [CallEndReason.Network]: CallEndReason.Network,
  [CallEndReason.Busy]: CallEndReason.Busy,
};

/** Narrows a wire `CallStateEvent.reason`; an absent or unknown reason yields `undefined`. */
export function callEndReasonOf(reason: number | undefined): CallEndReason | undefined {
  return reason === undefined ? undefined : WIRE_CALL_END_REASONS[reason];
}

const WIRE_CALL_MEDIA_KINDS: Readonly<Record<number, CallMediaKind>> = {
  [CallMediaKind.Audio]: CallMediaKind.Audio,
  [CallMediaKind.Video]: CallMediaKind.Video,
};

/**
 * Narrows a wire media kind. A kind this build does not know degrades to audio: the call still
 * happens, as the honest lesser version of itself, rather than being dropped over a label.
 */
export function callMediaKindOf(mediaKind: number): CallMediaKind {
  return WIRE_CALL_MEDIA_KINDS[mediaKind] ?? CallMediaKind.Audio;
}
