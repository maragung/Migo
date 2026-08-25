/**
 * LEB128, and the two decisions that make it safe.
 *
 * **Canonical.** A padded encoding of a value is rejected, not accepted-and-normalised.
 * Mesh handshake frames are signed over their bytes and deduplicated by hash, so two
 * spellings of one value would mean two valid signatures for one logical message.
 *
 * **Bounded before allocation.** Ten bytes is the longest legal encoding of a 64-bit
 * value, and the eleventh byte is refused before it is read. A decoder that keeps
 * shifting for as long as the continuation bit is set is a remote hang.
 *
 * ## Why `low` and `high` instead of `bigint`
 *
 * A `bigint` per decoded integer would allocate on the hot path, and 84 of the 99
 * varint fields in the schema are `u32`. So the scanner accumulates into two ordinary
 * numbers — `low` carries bits 0..48 and `high` carries bits 49..63, with the value
 * being `low + high * 2**49` — and only the callers that genuinely need 64 bits pay for
 * a `bigint`. Both halves stay well inside the 53 bits a double represents exactly, so
 * this is not an approximation: it is the same integer, in two registers.
 */

import { WireError } from './errors.js';
import { MAX_VARINT_BYTES } from './limits.js';

/** Bit position where the high half starts. */
const HIGH_SHIFT = 49;

/** A decoded varint, split as described in the module docs. */
export interface ScannedVarint {
  /** Bits 0..48. */
  readonly low: number;
  /** Bits 49..63. Zero for every value below 2**49. */
  readonly high: number;
  /** How many bytes the encoding occupied. */
  readonly used: number;
}

/**
 * Reads one varint at `offset`.
 *
 * Rejects, in this order: an eleventh byte, a truncated encoding, a tenth byte carrying
 * more than one payload bit, and a terminal byte of zero after the first — which is the
 * padded form.
 */
export function scan(input: Uint8Array, offset: number): ScannedVarint {
  let low = 0;
  let high = 0;
  let index = 0;

  for (;;) {
    if (index >= MAX_VARINT_BYTES) {
      throw WireError.varintTooLong(offset, MAX_VARINT_BYTES);
    }
    const byte = input[offset + index];
    if (byte === undefined) {
      throw WireError.unexpectedEnd(offset + index, 1);
    }
    const shift = index * 7;
    index += 1;
    const payload = byte & 0x7f;

    // At shift 63 only bit 63 is left, so a payload above 1 describes a value that does
    // not exist in 64 bits. Caught here rather than silently wrapping.
    if (shift === 63 && payload > 1) {
      throw WireError.varintTooLong(offset, MAX_VARINT_BYTES);
    }

    if (shift < HIGH_SHIFT) {
      low += payload * 2 ** shift;
    } else {
      high += payload * 2 ** (shift - HIGH_SHIFT);
    }

    if ((byte & 0x80) === 0) {
      if (index > 1 && byte === 0) {
        throw WireError.nonMinimalVarint(offset);
      }
      return { low, high, used: index };
    }
  }
}

/** Reads a varint that must fit `u32`. */
export function decodeU32(input: Uint8Array, offset: number): { value: number; used: number } {
  const { low, high, used } = scan(input, offset);
  if (high !== 0 || low > 0xffffffff) {
    throw WireError.lengthOverflow(toBigInt(low, high));
  }
  return { value: low, used };
}

/**
 * Reads a varint as a `number`.
 *
 * Refuses a value above `Number.MAX_SAFE_INTEGER` instead of rounding it. The schema
 * uses `u64` for sequence numbers and byte counts, where silently landing on a
 * neighbouring integer is worse than a decode failure — it would make two different
 * frames compare equal.
 */
export function decodeU64Safe(input: Uint8Array, offset: number): { value: number; used: number } {
  const { low, high, used } = scan(input, offset);
  if (high === 0) {
    return { value: low, used };
  }
  const wide = toBigInt(low, high);
  if (wide > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw WireError.lengthOverflow(wide);
  }
  return { value: Number(wide), used };
}

/** Reads a varint across the full unsigned 64-bit range. */
export function decodeU64(input: Uint8Array, offset: number): { value: bigint; used: number } {
  const { low, high, used } = scan(input, offset);
  return { value: toBigInt(low, high), used };
}

function toBigInt(low: number, high: number): bigint {
  return high === 0 ? BigInt(low) : BigInt(low) + (BigInt(high) << BigInt(HIGH_SHIFT));
}

/** Somewhere to put bytes. Kept minimal so `Writer` and a plain array both qualify. */
export interface ByteSink {
  push(byte: number): void;
}

/**
 * Appends `value` as a varint.
 *
 * `number` and `bigint` are both accepted because the schema needs both and forcing
 * every `u32` caller through `BigInt()` would allocate for nothing. A negative or
 * non-integer `number` is a programming error in this process, not a wire event, so it
 * throws a `RangeError` rather than a `WireError`.
 */
export function encodeU64(value: number | bigint, out: ByteSink): void {
  if (typeof value === 'bigint') {
    if (value < 0n || value > 0xffffffffffffffffn) {
      throw new RangeError(`varint value out of range: ${value}`);
    }
    let remaining = value;
    while (remaining >= 0x80n) {
      out.push(Number(remaining & 0x7fn) | 0x80);
      remaining >>= 7n;
    }
    out.push(Number(remaining));
    return;
  }

  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RangeError(`varint value must be a non-negative safe integer: ${value}`);
  }
  // Division rather than `>>>`, which would truncate to 32 bits at the fifth group.
  let remaining = value;
  while (remaining >= 0x80) {
    out.push((remaining % 0x80) | 0x80);
    remaining = Math.floor(remaining / 0x80);
  }
  out.push(remaining);
}

/** Bytes `encodeU64` would append. Used to size a buffer before writing into it. */
export function encodedLen(value: number | bigint): number {
  if (typeof value === 'bigint') {
    if (value < 0n) throw new RangeError(`varint value out of range: ${value}`);
    let remaining = value;
    let len = 1;
    while (remaining >= 0x80n) {
      remaining >>= 7n;
      len += 1;
    }
    return len;
  }
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RangeError(`varint value must be a non-negative safe integer: ${value}`);
  }
  let remaining = value;
  let len = 1;
  while (remaining >= 0x80) {
    remaining = Math.floor(remaining / 0x80);
    len += 1;
  }
  return len;
}

/**
 * Maps a signed value onto an unsigned one so that small negatives stay small:
 * `0, -1, 1, -2` become `0, 1, 2, 3`.
 *
 * Without this, `-1` would be `0xFFFFFFFFFFFFFFFF` and cost ten bytes. `BigInt` shifts
 * behave as infinite-precision two's complement, so the mask is what makes the result a
 * 64-bit quantity rather than an arbitrarily large one.
 */
export function zigzagEncode(value: bigint): bigint {
  return ((value << 1n) ^ (value >> 63n)) & 0xffffffffffffffffn;
}

/** Inverse of {@link zigzagEncode}. */
export function zigzagDecode(value: bigint): bigint {
  return (value >> 1n) ^ -(value & 1n);
}
