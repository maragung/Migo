/**
 * MSE decoder.
 *
 * Every method treats its input as hostile. The rule the whole module is built around:
 * **a length is validated against the bytes actually available before a single byte is
 * allocated.** A four-byte frame that claims a four-gigabyte string must cost four bytes
 * to reject, not four gigabytes to discover. That one property is the difference between
 * a codec and a remote out-of-memory primitive.
 *
 * Sub-readers for optional fields are `subarray` views, so skipping an unknown field is
 * free and so is handing a nested struct its own view of the payload.
 */

import { WireError } from './errors.js';
import { ID_BYTE_LEN, idFromBytes, type Id } from './id.js';
import {
  MAX_BYTES_LEN,
  MAX_FRAME_BYTES,
  MAX_LIST_ITEMS,
  MAX_NESTING_DEPTH,
  MAX_STRING_BYTES,
} from './limits.js';
import { fromWire } from './time.js';
import * as varint from './varint.js';

/**
 * `fatal: true` is the entire reason this exists. The default `TextDecoder` replaces
 * malformed sequences with U+FFFD, which would turn "this peer sent invalid UTF-8" into
 * "this display name contains a replacement character" — a silent mutation of user data
 * where the Rust side returns `InvalidUtf8`. Two implementations of one protocol may not
 * disagree about which frames are valid.
 */
const UTF8 = new TextDecoder('utf-8', { fatal: true });

/** Builds the error a length prefix over its limit should raise. */
type TooLong = (len: number, max: number) => WireError;

/*
 * The three length checks below report through a factory, and the factory is hoisted here
 * rather than written as `WireError.stringTooLong` at the call site. Handing a static method
 * around as a value is a hazard in general — the linter cannot see that these particular ones
 * never touch `this` — and a fresh arrow at each call site would allocate a closure for every
 * decoded field, which in a frame full of strings is measurable garbage in exchange for nothing.
 */
const STRING_TOO_LONG: TooLong = (len, max) => WireError.stringTooLong(len, max);
const BYTES_TOO_LONG: TooLong = (len, max) => WireError.bytesTooLong(len, max);
const FRAME_TOO_LARGE: TooLong = (len, max) => WireError.frameTooLarge(len, max);

/** Decodes an MSE payload. */
export class Reader {
  readonly #input: Uint8Array;
  #pos: number;
  #depth: number;

  constructor(input: Uint8Array, depth = 0) {
    this.#input = input;
    this.#pos = 0;
    this.#depth = depth;
  }

  /** Bytes not yet consumed. */
  get remaining(): number {
    return Math.max(0, this.#input.length - this.#pos);
  }

  /** True when the payload is fully consumed. */
  get isEmpty(): boolean {
    return this.remaining === 0;
  }

  /** Current read offset, for error reporting. */
  get position(): number {
    return this.#pos;
  }

  /**
   * Asserts the payload was fully consumed.
   *
   * Called at the top level only. Trailing bytes there mean the sender and receiver
   * disagree about the schema, which is worth failing on: ignoring them silently turns a
   * version mismatch into a mystery bug three releases later.
   */
  finish(): void {
    if (this.remaining > 0) {
      throw WireError.trailingBytes(this.remaining);
    }
  }

  /** Opens a struct. Bounds recursion. */
  enter(): void {
    if (this.#depth >= MAX_NESTING_DEPTH) {
      throw WireError.depthExceeded(MAX_NESTING_DEPTH);
    }
    this.#depth += 1;
  }

  /** Closes a struct. */
  leave(): void {
    this.#depth = Math.max(0, this.#depth - 1);
  }

  /**
   * Reads one byte as a boolean, accepting only `0` and `1`.
   *
   * Anything else is an error rather than "truthy". The codec is canonical by rule, and a
   * byte with 255 legal encodings of `true` breaks that rule exactly the way a padded
   * varint would. Forward compatibility in MSE comes from optional field ids, not from
   * spare bits inside a required field.
   */
  bool(): boolean {
    const byte = this.#u8();
    if (byte === 0) return false;
    if (byte === 1) return true;
    throw WireError.invalidBool(byte);
  }

  /** Reads a varint that must fit in 32 bits. */
  u32(): number {
    const { value, used } = varint.decodeU32(this.#input, this.#pos);
    this.#pos += used;
    return value;
  }

  /** Reads a varint as a `number`, refusing values a double cannot hold exactly. */
  u64(): number {
    const { value, used } = varint.decodeU64Safe(this.#input, this.#pos);
    this.#pos += used;
    return value;
  }

  /** Reads a varint across the full unsigned 64-bit range. */
  u64big(): bigint {
    const { value, used } = varint.decodeU64(this.#input, this.#pos);
    this.#pos += used;
    return value;
  }

  /** Reads Migo-epoch milliseconds and returns Unix milliseconds. */
  timestamp(): number {
    const { value, used } = varint.decodeU64Safe(this.#input, this.#pos);
    this.#pos += used;
    return fromWire(value);
  }

  /** Reads a 16-byte identifier. */
  id(): Id {
    return idFromBytes(this.#take(ID_BYTE_LEN));
  }

  /** Reads a length-prefixed UTF-8 string. */
  str(): string {
    const len = this.#readLength(MAX_STRING_BYTES, STRING_TOO_LONG);
    const bytes = this.#take(len);
    try {
      return UTF8.decode(bytes);
    } catch {
      // The `TypeError` from a fatal decoder says nothing a caller can act on, and it is
      // not a `WireError`, so a `catch (e) { if (e instanceof WireError) }` upstream would
      // let it escape as an internal error instead of a protocol violation.
      throw WireError.invalidUtf8();
    }
  }

  /** Reads a length-prefixed opaque byte field, copied so the caller owns it. */
  bytes(): Uint8Array {
    const len = this.#readLength(MAX_BYTES_LEN, BYTES_TOO_LONG);
    return this.#take(len).slice();
  }

  /**
   * Reads a length-prefixed opaque byte field as a view over the input.
   *
   * No copy, so this is the right choice for a ciphertext that is about to be handed
   * straight to the AEAD. The caller must not mutate it and must not keep it past the
   * lifetime of the frame buffer.
   */
  bytesShared(): Uint8Array {
    const len = this.#readLength(MAX_BYTES_LEN, BYTES_TOO_LONG);
    return this.#take(len);
  }

  /**
   * Reads a list header and returns the item count.
   *
   * Checked against both {@link MAX_LIST_ITEMS} and the remaining bytes: every item costs
   * at least one byte, so a count larger than what is left is a lie — and callers size an
   * array from this number.
   */
  listLen(): number {
    const len = this.#lengthVarint();
    if (len > MAX_LIST_ITEMS) {
      throw WireError.listTooLong(len, MAX_LIST_ITEMS);
    }
    if (len > this.remaining) {
      throw WireError.listTooLong(len, this.remaining);
    }
    return len;
  }

  /**
   * Reads one optional field header and returns its id together with a reader scoped to
   * exactly that field's bytes.
   *
   * The scoping is what makes an unknown field safe to ignore: the caller drops the
   * sub-reader and the outer position has already advanced past the whole field. A
   * malformed unknown field therefore cannot desynchronise the stream — which is the
   * property that lets a v1 client keep talking to a v2 server.
   */
  optional(): [number, Reader] {
    const fieldId = this.u32();
    const len = this.#readLength(MAX_FRAME_BYTES, FRAME_TOO_LARGE);
    return [fieldId, new Reader(this.#take(len), this.#depth)];
  }

  #u8(): number {
    const byte = this.#input[this.#pos];
    if (byte === undefined) {
      throw WireError.unexpectedEnd(this.#pos, 1);
    }
    this.#pos += 1;
    return byte;
  }

  /**
   * Decodes a length prefix as a `number`.
   *
   * A value at or above 2^53 cannot be represented exactly, but it is also thousands of
   * times past every limit in {@link limits}, so the only thing the caller does with it is
   * reject it. The approximation therefore changes the number printed in the error and
   * nothing else — and it keeps the failure classified the same way the Rust reader
   * classifies it, which the shared vectors require.
   */
  #lengthVarint(): number {
    const { low, high, used } = varint.scan(this.#input, this.#pos);
    this.#pos += used;
    return high === 0 ? low : low + high * 2 ** 49;
  }

  /**
   * Reads a length prefix and rejects it against both the configured limit and the bytes
   * actually present, before the caller allocates anything.
   */
  #readLength(max: number, tooLong: TooLong): number {
    const len = this.#lengthVarint();
    if (len > max) {
      throw tooLong(len, max);
    }
    if (len > this.remaining) {
      throw WireError.unexpectedEnd(this.#pos, len - this.remaining);
    }
    return len;
  }

  /** Consumes `len` bytes as a view. */
  #take(len: number): Uint8Array {
    if (len > this.remaining) {
      throw WireError.unexpectedEnd(this.#pos, len - this.remaining);
    }
    const view = this.#input.subarray(this.#pos, this.#pos + len);
    this.#pos += len;
    return view;
  }
}
