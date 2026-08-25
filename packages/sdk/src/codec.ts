/**
 * The typed bridge between protocol message structs and frame payloads, plus the frame-level
 * classification the transport needs but the wire layer can't provide.
 *
 * `@migo/wire` knows how to read and write a {@link Frame} — a header and an opaque payload. It
 * does not know that opcode 32 carries a `MessageSend`, nor that a Critical frame consumes a
 * `frame_seq`. `@migo/protocol` knows the message shapes and the opcode table but never touches a
 * socket. This module joins them: {@link encodeBody}/{@link decodeBody} turn a struct into payload
 * bytes and back, and {@link isSequenced}/{@link requiresAck}/{@link frameDeliveryClass} answer the
 * questions the transport asks of every inbound frame.
 *
 * The sequencing rule is the subtle one and it must match the server exactly. The server assigns a
 * `frame_seq` to a server→client frame if and only if that frame is Critical AND it left through
 * the session mailbox. The client cannot see the mailbox, so it reconstructs the counter by
 * classifying each inbound frame: Critical iff the ERROR flag is set (an error reply is always
 * Critical, whatever opcode it rode in on) or the opcode's declared class is Critical. Two frames
 * are Critical yet unsequenced because the server writes them straight to the transport, bypassing
 * the mailbox — WELCOME (consumed by the handshake before counting starts) and RECONNECT_HINT
 * (sent once at graceful shutdown). WELCOME never reaches this classifier; RECONNECT_HINT is
 * excluded explicitly. Get this wrong by one and a resume request is rejected with RESUME_REQUIRED.
 */

import { Reader, Writer } from '@migo/wire';
import type { Frame } from '@migo/wire';
import { FLAG, OP, OPCODES } from '@migo/protocol';
import type { DeliveryClass, OpcodeMeta } from '@migo/protocol';

/** A generated `encodeX(w, v)` function, seen generically. */
export type BodyEncoder<T> = (w: Writer, v: T) => void;
/** A generated `decodeX(r): T` function, seen generically. */
export type BodyDecoder<T> = (r: Reader) => T;

/**
 * Serialises a message struct to its MSE payload bytes.
 *
 * Pass the generated encoder for the concrete type — `encodeBody(encodeMessageSend, msg)` — and
 * the return type is inferred from it, so a mismatched struct is a compile error, not a wire fault.
 */
export function encodeBody<T>(encode: BodyEncoder<T>, value: T): Uint8Array {
  const w = new Writer();
  encode(w, value);
  return w.finish();
}

/**
 * Deserialises an MSE payload back into a message struct.
 *
 * Asserts the payload was fully consumed: trailing bytes mean the peer encoded a schema this build
 * doesn't share, which is a fault worth surfacing now rather than a field silently dropped.
 */
export function decodeBody<T>(decode: BodyDecoder<T>, payload: Uint8Array): T {
  const r = new Reader(payload);
  const value = decode(r);
  r.finish();
  return value;
}

/** The opcode table entry for a code, or `undefined` if the code is unknown to this build. */
export function opcodeMeta(opcode: number): OpcodeMeta | undefined {
  return OPCODES[opcode];
}

/** The human-facing opcode name for logging, falling back to the numeric code. */
export function opcodeLabel(opcode: number): string {
  return OPCODES[opcode]?.name ?? `opcode(${opcode})`;
}

/** Whether a frame's body is a protocol `Error` rather than the opcode's declared response. */
export function hasErrorFlag(frame: Frame): boolean {
  return (frame.header.flags & FLAG.ERROR) !== 0;
}

/**
 * Whether the sender demands a cumulative acknowledgement for this frame.
 *
 * Set on server→client MESSAGE_EVENT frames; the client answers by sending an `Ack` carrying the
 * highest `frame_seq` it has counted so far. The flag is advisory about *when* to ack, not *what*
 * to ack — the watermark is always the running Critical-frame count.
 */
export function requiresAck(frame: Frame): boolean {
  return (frame.header.flags & FLAG.ACK_REQUIRED) !== 0;
}

/**
 * The effective delivery class of an inbound frame.
 *
 * An error reply is Critical regardless of the opcode it reuses; otherwise the class is the
 * opcode's declared class. An unknown opcode has no declared class and is treated as Droppable —
 * the transport rejects unknown opcodes on its own, so this default only governs the (unreachable)
 * case where classification runs before that check.
 */
export function frameDeliveryClass(frame: Frame): DeliveryClass {
  if (hasErrorFlag(frame)) {
    return 'Critical';
  }
  return OPCODES[frame.header.opcode]?.cls ?? 'Droppable';
}

/**
 * Whether an inbound frame consumes a `frame_seq` on the client's counter.
 *
 * Mirrors the server's mailbox sequencing: Critical frames are sequenced, except RECONNECT_HINT,
 * which the server writes directly to the transport at graceful shutdown and never sequences.
 * WELCOME is the other unsequenced Critical frame, but it is consumed by the handshake before the
 * receive loop starts counting, so it never reaches here.
 */
export function isSequenced(frame: Frame): boolean {
  if (frame.header.opcode === OP.RECONNECT_HINT) {
    return false;
  }
  return frameDeliveryClass(frame) === 'Critical';
}
