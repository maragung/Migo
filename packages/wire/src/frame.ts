/**
 * MWP/1 frame header.
 *
 * Every message on every transport is one frame:
 *
 * ```text
 * u8      version          protocol version, 1
 * u8      flags            see ./flags.ts
 * varint  opcode           what this frame is
 * varint  correlation      request/response pairing, 0 for unsolicited
 * [16]u8  trace_id         only when TRACED
 * [8]u8   span_id          only when TRACED
 * varint  fragment_index   only when FRAGMENT
 * varint  fragment_total   only when FRAGMENT
 * ...     payload          the remainder of the frame
 * ```
 *
 * Note what is *not* here: a length. The payload runs to the end of the frame and the
 * transport supplies the boundary. WebSocket and QUIC datagrams already frame messages,
 * so a length field would be redundant bytes on every single message. Stream transports
 * that do not frame prepend a `u32` big-endian length — see
 * {@link encodeFrameLengthPrefixed} — which keeps the cost where it is actually needed
 * instead of taxing the common case.
 *
 * Header fields are ordered by how often they are present, so a parser reads forward once
 * and never seeks.
 */

import { WireError } from './errors.js';
import * as flags from './flags.js';
import { MAX_FRAME_BYTES } from './limits.js';
import * as varint from './varint.js';

/**
 * The wire protocol version this build speaks.
 *
 * Bumping this is a breaking change and is not how features are added: new optional
 * fields and new opcodes are backward compatible by construction. This changes only if
 * the *framing* changes.
 */
export const PROTOCOL_VERSION = 1;

/** Encoded size of a trace context in a frame header. */
export const TRACE_ENCODED_LEN = 24;

/**
 * Distributed-tracing identifiers carried on a frame.
 *
 * W3C Trace Context shapes: a 16-byte trace id and an 8-byte span id, sent as raw bytes
 * rather than the 55-character `traceparent` string. Sampling happens at the edge, so
 * most frames omit this entirely.
 */
export interface TraceContext {
  readonly traceId: Uint8Array;
  readonly spanId: Uint8Array;
}

/**
 * Position of a frame within a fragmented message.
 *
 * Used only for payloads that cannot fit {@link MAX_FRAME_BYTES} — in practice large
 * media metadata and history backfills. Chat messages never fragment.
 */
export interface Fragment {
  readonly index: number;
  readonly total: number;
}

/** The parsed header of an MWP/1 frame. */
export interface FrameHeader {
  /** Protocol version byte as it appeared on the wire. */
  readonly version: number;
  /** Raw flag bits. Use the helpers rather than testing bits inline. */
  readonly flags: number;
  /** The operation this frame carries. */
  readonly opcode: number;
  /** Correlates a response with its request; 0 for server-initiated frames. */
  readonly correlation: number;
  /** Present when the `TRACED` flag is set. */
  readonly trace: TraceContext | null;
  /** Present when the `FRAGMENT` flag is set. */
  readonly fragment: Fragment | null;
}

/** A complete frame: header plus payload bytes, still compressed if `COMPRESSED` is set. */
export interface Frame {
  readonly header: FrameHeader;
  readonly payload: Uint8Array;
}

/** A minimal header: current version, no flags, no trace, no fragment. */
export function frameHeader(opcode: number, correlation = 0): FrameHeader {
  return {
    version: PROTOCOL_VERSION,
    flags: 0,
    opcode,
    correlation,
    trace: null,
    fragment: null,
  };
}

/**
 * True when both identifiers are all zero, which W3C Trace Context defines as invalid.
 * Such a context is dropped rather than propagated.
 */
export function isInvalidTrace(trace: TraceContext): boolean {
  return trace.traceId.every((b) => b === 0) && trace.spanId.every((b) => b === 0);
}

/** True when this is the last fragment of its message. */
export function isLastFragment(fragment: Fragment): boolean {
  return fragment.index + 1 >= fragment.total;
}

/** Encoded size of this header in bytes. */
export function headerEncodedLen(header: FrameHeader): number {
  let len = 2 + varint.encodedLen(header.opcode) + varint.encodedLen(header.correlation);
  if (header.trace !== null) {
    len += TRACE_ENCODED_LEN;
  }
  if (header.fragment !== null) {
    len += varint.encodedLen(header.fragment.index) + varint.encodedLen(header.fragment.total);
  }
  return len;
}

/**
 * Appends this header to `out`.
 *
 * The flag bits and the optional blocks are written from the same source of truth — the
 * nullable fields — so a header can never claim `TRACED` without carrying a trace. A
 * caller-supplied `TRACED` bit with no trace is therefore corrected, not honoured.
 */
export function encodeHeader(header: FrameHeader, out: number[]): void {
  let bits = header.flags & ~(flags.TRACED | flags.FRAGMENT);
  if (header.trace !== null) bits |= flags.TRACED;
  if (header.fragment !== null) bits |= flags.FRAGMENT;
  if ((bits & flags.RESERVED_MASK) !== 0) {
    throw WireError.reservedFlags(bits & flags.RESERVED_MASK);
  }

  out.push(header.version & 0xff);
  out.push(bits & 0xff);
  varint.encodeU64(header.opcode, out);
  varint.encodeU64(header.correlation, out);
  if (header.trace !== null) {
    assertLen(header.trace.traceId, 16, 'trace_id');
    assertLen(header.trace.spanId, 8, 'span_id');
    for (const byte of header.trace.traceId) out.push(byte);
    for (const byte of header.trace.spanId) out.push(byte);
  }
  if (header.fragment !== null) {
    validateFragment(header.fragment);
    varint.encodeU64(header.fragment.index, out);
    varint.encodeU64(header.fragment.total, out);
  }
}

/**
 * Parses a header from the front of `input`, returning it and the offset at which the
 * payload begins.
 *
 * Rejects, in this order: a short frame, an unsupported version, reserved flag bits, a
 * truncated trace block, and an impossible fragment pair. Reserved bits are an error and
 * not something to ignore — a peer that sets them is speaking a dialect we do not know,
 * and silently discarding the bits would let a future extension be stripped by an old
 * node that had no idea it was doing so.
 */
export function decodeHeader(input: Uint8Array): { header: FrameHeader; offset: number } {
  if (input.length < 2) {
    throw WireError.unexpectedEnd(input.length, 2);
  }
  const version = input[0] as number;
  if (version !== PROTOCOL_VERSION) {
    throw WireError.unsupportedVersion(version, PROTOCOL_VERSION);
  }
  const bits = input[1] as number;
  if ((bits & flags.RESERVED_MASK) !== 0) {
    throw WireError.reservedFlags(bits & flags.RESERVED_MASK);
  }

  let offset = 2;
  const opcodeRaw = varint.scan(input, offset);
  offset += opcodeRaw.used;
  const correlationRaw = varint.scan(input, offset);
  offset += correlationRaw.used;
  const opcode = narrow(opcodeRaw, 'opcode');
  const correlation = narrow(correlationRaw, 'correlation');

  let trace: TraceContext | null = null;
  if ((bits & flags.TRACED) !== 0) {
    const end = offset + TRACE_ENCODED_LEN;
    if (input.length < end) {
      throw WireError.unexpectedEnd(offset, TRACE_ENCODED_LEN);
    }
    trace = {
      traceId: input.slice(offset, offset + 16),
      spanId: input.slice(offset + 16, end),
    };
    offset = end;
  }

  let fragment: Fragment | null = null;
  if ((bits & flags.FRAGMENT) !== 0) {
    const indexRaw = varint.scan(input, offset);
    offset += indexRaw.used;
    const totalRaw = varint.scan(input, offset);
    offset += totalRaw.used;
    fragment = {
      index: narrow(indexRaw, 'fragment_index'),
      total: narrow(totalRaw, 'fragment_total'),
    };
    validateFragment(fragment);
  }

  return {
    header: { version, flags: bits, opcode, correlation, trace, fragment },
    offset,
  };
}

/**
 * Encodes the frame for a transport that supplies its own message boundaries (WebSocket,
 * QUIC datagram).
 */
export function encodeFrame(frame: Frame): Uint8Array {
  const len = headerEncodedLen(frame.header) + frame.payload.length;
  if (len > MAX_FRAME_BYTES) {
    throw WireError.frameTooLarge(len, MAX_FRAME_BYTES);
  }
  const head: number[] = [];
  encodeHeader(frame.header, head);
  const out = new Uint8Array(head.length + frame.payload.length);
  out.set(head, 0);
  out.set(frame.payload, head.length);
  return out;
}

/**
 * Encodes the frame with a `u32` big-endian length prefix, for stream transports that do
 * not frame messages themselves.
 */
export function encodeFrameLengthPrefixed(frame: Frame): Uint8Array {
  const body = encodeFrame(frame);
  const out = new Uint8Array(4 + body.length);
  new DataView(out.buffer).setUint32(0, body.length, false);
  out.set(body, 4);
  return out;
}

/**
 * Parses one frame from a complete transport message.
 *
 * The size limit is checked before anything is parsed, because the cheapest place to
 * reject an oversized frame is before touching it.
 */
export function decodeFrame(input: Uint8Array): Frame {
  if (input.length > MAX_FRAME_BYTES) {
    throw WireError.frameTooLarge(input.length, MAX_FRAME_BYTES);
  }
  const { header, offset } = decodeHeader(input);
  return { header, payload: input.subarray(offset) };
}

/**
 * Parses one length-prefixed frame, returning it and the number of bytes consumed so a
 * stream reader can advance.
 *
 * Returns `null` when the buffer does not yet hold a whole frame, which is the normal
 * state of a stream transport rather than an error.
 */
export function decodeFrameLengthPrefixed(
  input: Uint8Array,
): { frame: Frame; consumed: number } | null {
  if (input.length < 4) {
    return null;
  }
  const len = new DataView(input.buffer, input.byteOffset, input.byteLength).getUint32(0, false);
  if (len > MAX_FRAME_BYTES) {
    throw WireError.frameTooLarge(len, MAX_FRAME_BYTES);
  }
  if (input.length < 4 + len) {
    return null;
  }
  return { frame: decodeFrame(input.subarray(4, 4 + len)), consumed: 4 + len };
}

/**
 * Validates a fragment pair. A total of zero, or an index at or past the total, is a
 * protocol error rather than something to reassemble optimistically: accepting it would
 * let a peer keep a reassembly buffer alive forever, which is a memory leak a stranger
 * gets to trigger.
 */
function validateFragment(fragment: Fragment): void {
  if (fragment.total === 0 || fragment.index >= fragment.total) {
    throw WireError.invalidFragment(fragment.index, fragment.total);
  }
}

/** Narrows a scanned varint to a `u32` field, naming the field when it does not fit. */
function narrow(scanned: varint.ScannedVarint, field: string): number {
  if (scanned.high !== 0 || scanned.low > 0xffffffff) {
    throw WireError.fieldOverflow(field);
  }
  return scanned.low;
}

/**
 * A trace id of the wrong width is a bug in this process, not a wire event, so it throws a
 * `RangeError` like the varint encoder does rather than a `WireError` a peer could be
 * blamed for.
 */
function assertLen(bytes: Uint8Array, expected: number, what: string): void {
  if (bytes.length !== expected) {
    throw new RangeError(`${what} must be ${expected} bytes, got ${bytes.length}`);
  }
}
