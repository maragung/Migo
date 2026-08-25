/**
 * Frame batching.
 *
 * A busy room produces bursts: twenty presence updates and a typing indicator land inside
 * the same 15 milliseconds. Sent individually that is twenty-one WebSocket messages,
 * twenty-one syscalls, twenty-one wake-ups of a phone's radio, and twenty-one frame
 * headers. Batched, it is one.
 *
 * Wire format — the payload of a frame with the `BATCH` flag set:
 *
 * ```text
 * varint  count
 * count × ( varint frame_len, frame_len bytes )
 * ```
 *
 * Each element is a complete frame, header included. That redundancy is deliberate: a
 * batch is a transport optimisation, not a new message type, so the receiver's dispatch
 * loop is identical whether a frame arrived alone or inside a batch. Two rules keep it
 * honest:
 *
 * * At most {@link MAX_BATCH_ITEMS} elements.
 * * **No nesting.** A batch inside a batch is rejected, because otherwise a small frame
 *   could describe an exponentially large expansion.
 */

import { inflateRaw } from './compress.js';
import { WireError } from './errors.js';
import * as flags from './flags.js';
import { decodeFrame, encodeFrame, frameHeader, headerEncodedLen, type Frame } from './frame.js';
import { MAX_BATCH_ITEMS, MAX_FRAME_BYTES } from './limits.js';
import * as varint from './varint.js';

/**
 * Opcode reserved for the batch envelope.
 *
 * The batch carries no payload type of its own, so the opcode is a constant rather than
 * something the IDL generates.
 */
export const BATCH_OPCODE = 0;

/** Minimum bytes an element can occupy: one length byte plus a four-byte minimal frame. */
const MIN_ELEMENT_BYTES = 5;

/**
 * Packs frames into a single batch frame.
 *
 * Returns the batch with the `BATCH` flag set and correlation 0 — the elements carry their
 * own correlation ids, and the envelope has no request of its own to answer.
 *
 * A one-element batch is returned as the bare frame instead: wrapping it would add bytes
 * and buy nothing.
 */
export function encodeBatch(frames: readonly Frame[]): Frame {
  if (frames.length > MAX_BATCH_ITEMS) {
    throw WireError.batchTooLarge(frames.length, MAX_BATCH_ITEMS);
  }
  const only = frames[0];
  if (frames.length === 1 && only !== undefined) {
    return only;
  }

  const payload: number[] = [];
  varint.encodeU64(frames.length, payload);
  for (const frame of frames) {
    if ((frame.header.flags & flags.BATCH) !== 0) {
      throw WireError.nestedBatch();
    }
    const encoded = encodeFrame(frame);
    varint.encodeU64(encoded.length, payload);
    for (const byte of encoded) payload.push(byte);
  }

  const header = { ...frameHeader(BATCH_OPCODE, 0), flags: flags.BATCH };
  const batch: Frame = { header, payload: Uint8Array.from(payload) };
  const len = headerEncodedLen(header) + batch.payload.length;
  if (len > MAX_FRAME_BYTES) {
    throw WireError.frameTooLarge(len, MAX_FRAME_BYTES);
  }
  return batch;
}

/**
 * Turns one received frame into the list of frames to dispatch: inflates a compressed
 * payload, then unpacks a batch.
 *
 * This is the function a transport should call on every inbound frame. A frame without the
 * `BATCH` flag comes back as a one-element list, so the caller keeps a single dispatch
 * path and never has to ask which shape arrived.
 */
export async function unpackFrame(frame: Frame): Promise<Frame[]> {
  const payload =
    (frame.header.flags & flags.COMPRESSED) !== 0
      ? await inflateRaw(frame.payload, MAX_FRAME_BYTES)
      : frame.payload;

  if ((frame.header.flags & flags.BATCH) === 0) {
    return [{ header: frame.header, payload }];
  }
  return decodeBatchPayload(payload);
}

/**
 * Unpacks the already-inflated payload of a batch frame.
 *
 * Separate from {@link unpackFrame} because inflation is asynchronous on this platform and
 * the parsing is not; a caller holding plain bytes should not have to await anything.
 */
export function decodeBatchPayload(payload: Uint8Array): Frame[] {
  const head = varint.scan(payload, 0);
  let offset = head.used;
  const count = head.high === 0 ? head.low : MAX_BATCH_ITEMS + 1;
  if (count > MAX_BATCH_ITEMS) {
    throw WireError.batchTooLarge(count, MAX_BATCH_ITEMS);
  }
  // Every element costs at least a length byte plus a four-byte minimal header, so a count
  // larger than the remaining bytes allow is a lie. Checking it here is what stops a
  // preallocation from being turned into an allocation primitive by a 3-byte frame.
  const remaining = payload.length - offset;
  if (count * MIN_ELEMENT_BYTES > remaining) {
    throw WireError.batchTooLarge(count, Math.floor(remaining / MIN_ELEMENT_BYTES));
  }

  const frames: Frame[] = [];
  for (let i = 0; i < count; i += 1) {
    const scanned = varint.scan(payload, offset);
    offset += scanned.used;
    if (scanned.high !== 0) {
      throw WireError.frameTooLarge(Number.MAX_SAFE_INTEGER, MAX_FRAME_BYTES);
    }
    const len = scanned.low;
    if (len > MAX_FRAME_BYTES) {
      throw WireError.frameTooLarge(len, MAX_FRAME_BYTES);
    }
    const end = offset + len;
    if (end > payload.length) {
      throw WireError.unexpectedEnd(offset, len);
    }
    const element = decodeFrame(payload.subarray(offset, end));
    if ((element.header.flags & flags.BATCH) !== 0) {
      throw WireError.nestedBatch();
    }
    frames.push(element);
    offset = end;
  }

  if (offset !== payload.length) {
    throw WireError.trailingBytes(payload.length - offset);
  }
  return frames;
}
