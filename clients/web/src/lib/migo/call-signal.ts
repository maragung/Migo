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
 * # The placeholder key material
 *
 * Call signaling is end-to-end on the wire: the SDP and ICE blobs a client hands the SDK are
 * sealed for the peer's *device*, because an SDP body carries DTLS fingerprints and an ICE
 * candidate carries network addresses, and the signaling server must not read either. Real
 * per-device sealing rides the 1:1 session layer and is future work; until it lands the seal is
 * the same placeholder media attachments use — a framed envelope with a version byte, a 32-byte
 * zero key, and a 12-byte zero nonce, all of the lengths the real crypto will use, with the
 * payload riding after them in the clear. The signaling flow is already end-to-end correct *in
 * shape*: swapping the placeholder for real key material touches only {@link sealCallSignal} and
 * {@link openCallSignal}.
 */

import { CallEndReason, CallMediaKind, CallState } from '@migo/sdk';
import type { ActiveCall } from '@migo/sdk';

/** The envelope version this build writes; an unknown version refuses to open rather than guessing. */
const SEAL_VERSION = 1;
/** Placeholder key material, zero-filled at the lengths the future real crypto will use. */
const PLACEHOLDER_KEY = new Uint8Array(32);
const PLACEHOLDER_NONCE = new Uint8Array(12);
/** Bytes of envelope before the payload: version, key, nonce. */
const ENVELOPE_PREFIX_BYTES = 1 + PLACEHOLDER_KEY.length + PLACEHOLDER_NONCE.length;

/** An envelope this build cannot read: wrong version, or too short to hold its own header. */
export class CallSignalFormatError extends Error {
  constructor(reason: string) {
    super(`migo: unreadable call signal envelope: ${reason}`);
    this.name = 'CallSignalFormatError';
  }
}

/**
 * Seals one signaling payload (an SDP description or an ICE batch) into the placeholder envelope.
 *
 * See the module doc: this is framing, not encryption — the bytes ride in the clear behind key and
 * nonce slots that the real per-device sealing will fill. Both sides of a call run this build, so
 * the round trip is symmetric.
 */
export function sealCallSignal(payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(ENVELOPE_PREFIX_BYTES + payload.length);
  out[0] = SEAL_VERSION;
  out.set(PLACEHOLDER_KEY, 1);
  out.set(PLACEHOLDER_NONCE, 1 + PLACEHOLDER_KEY.length);
  out.set(payload, ENVELOPE_PREFIX_BYTES);
  return out;
}

/**
 * Opens a sealed signaling payload; the inverse of {@link sealCallSignal}.
 *
 * A malformed or future-versioned envelope throws {@link CallSignalFormatError} rather than
 * returning nonsense bytes a WebRTC stack would choke on downstream with a worse error.
 */
export function openCallSignal(sealed: Uint8Array): Uint8Array {
  if (sealed.length < ENVELOPE_PREFIX_BYTES) {
    throw new CallSignalFormatError('shorter than its own header');
  }
  if (sealed[0] !== SEAL_VERSION) {
    throw new CallSignalFormatError(`version ${sealed[0]}`);
  }
  return sealed.slice(ENVELOPE_PREFIX_BYTES);
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
    default:
      return 'Call ended';
  }
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
