/**
 * MSE encoder.
 *
 * Every limit is checked *before* the bytes are appended, not after the buffer has grown.
 * The difference matters when the value came from a peer: a check afterwards has already
 * allocated whatever it was about to reject.
 */

import { WireError } from './errors.js';
import { idToBytes, type Id } from './id.js';
import {
  MAX_BYTES_LEN,
  MAX_FRAME_BYTES,
  MAX_LIST_ITEMS,
  MAX_NESTING_DEPTH,
  MAX_STRING_BYTES,
} from './limits.js';
import { toWire } from './time.js';
import * as varint from './varint.js';

const UTF8 = new TextEncoder();
const EMPTY = new Uint8Array(0);

/** One suspended scope: the buffer an `optional` field interrupted. */
interface SavedScope {
  readonly buf: Uint8Array;
  readonly len: number;
}

/** A growable byte buffer with the MSE grammar on top of it. */
export class Writer {
  #buf: Uint8Array;
  #len = 0;

  /** Buffers of enclosing scopes, innermost last. See {@link Writer.optional}. */
  readonly #saved: SavedScope[] = [];
  /**
   * Retired inner buffers, reused so a struct with twelve optional fields does not
   * allocate twelve arrays. They come back already grown, which is the point.
   */
  readonly #pool: Uint8Array[] = [];
  /**
   * Bytes held in suspended scopes, so {@link Writer.length} is the whole frame and not
   * just the innermost scope. Without it the frame limit would be applied per field, and
   * a struct of a hundred 200 KiB fields would encode happily.
   */
  #outerLen = 0;
  #depth = 0;

  constructor(capacity = 256) {
    this.#buf = new Uint8Array(Math.min(Math.max(capacity, 16), MAX_FRAME_BYTES));
  }

  /** Bytes written so far, across all open scopes. */
  get length(): number {
    return this.#outerLen + this.#len;
  }

  /** Appends one raw byte. Present so a `Writer` satisfies {@link varint.ByteSink}. */
  push(byte: number): void {
    this.#reserve(1);
    this.#buf[this.#len] = byte;
    this.#len += 1;
  }

  /**
   * Opens a struct scope.
   *
   * Nothing is written: the grammar has no struct delimiter, because the required field
   * list is fixed by the schema and the optional count is explicit. This exists purely to
   * bound recursion, and it is what stops a hostile 200-deep nesting from becoming 200
   * stack frames in a generated encoder.
   */
  enter(): void {
    if (this.#depth >= MAX_NESTING_DEPTH) {
      throw WireError.depthExceeded(MAX_NESTING_DEPTH);
    }
    this.#depth += 1;
  }

  /** Closes a struct scope. */
  leave(): void {
    this.#depth = Math.max(0, this.#depth - 1);
  }

  /** One byte, `0` or `1`. */
  bool(value: boolean): void {
    this.push(value ? 1 : 0);
  }

  /** Unsigned 32-bit, LEB128. */
  u32(value: number): void {
    varint.encodeU64(value, this);
  }

  /**
   * Unsigned 64-bit, LEB128, as a `number`.
   *
   * Refuses anything above `Number.MAX_SAFE_INTEGER` rather than writing a rounded
   * value: the schema uses `u64` for sequence numbers and byte counts, and silently
   * landing on a neighbouring integer would make two different frames compare equal.
   * Fields that genuinely need all 64 bits are declared `bitmask64` and go through
   * {@link Writer.u64big}.
   */
  u64(value: number): void {
    varint.encodeU64(value, this);
  }

  /** Unsigned 64-bit, LEB128, across the full range. */
  u64big(value: bigint): void {
    varint.encodeU64(value, this);
  }

  /** Unix milliseconds, written relative to the Migo epoch. */
  timestamp(unixMs: number): void {
    varint.encodeU64(toWire(unixMs), this);
  }

  /** Sixteen raw bytes, with no length prefix — the width is fixed by the schema. */
  id(value: Id): void {
    const bytes = idToBytes(value);
    this.#guardGrowth(bytes.length);
    this.#extend(bytes);
  }

  /** Varint byte length, then UTF-8. */
  str(value: string): void {
    const bytes = UTF8.encode(value);
    if (bytes.length > MAX_STRING_BYTES) {
      throw WireError.stringTooLong(bytes.length, MAX_STRING_BYTES);
    }
    this.#guardGrowth(bytes.length);
    varint.encodeU64(bytes.length, this);
    this.#extend(bytes);
  }

  /** Varint byte length, then the bytes. */
  bytes(value: Uint8Array): void {
    if (value.length > MAX_BYTES_LEN) {
      throw WireError.bytesTooLong(value.length, MAX_BYTES_LEN);
    }
    this.#guardGrowth(value.length);
    varint.encodeU64(value.length, this);
    this.#extend(value);
  }

  /** Item count for a `list<T>`. The items follow, written by the caller. */
  listLen(len: number): void {
    if (len > MAX_LIST_ITEMS) {
      throw WireError.listTooLong(len, MAX_LIST_ITEMS);
    }
    varint.encodeU64(len, this);
  }

  /**
   * Writes one optional field as `field_id, byte_len, bytes`.
   *
   * The length prefix is the whole point of the design: it is what lets a decoder skip a
   * field id it has never heard of, which is how a v1 client survives a v2 server without
   * renegotiating a version for every added field. Because the prefix is a varint its
   * width is not known until the body has been written — hence the buffer swap rather
   * than a reserved placeholder that would have to be right the first time.
   *
   * The *count* of present optional fields is written by the caller, before the first of
   * these. That is the struct's business, not the field's.
   */
  optional(fieldId: number, write: (writer: Writer) => void): void {
    if (this.#saved.length >= MAX_NESTING_DEPTH) {
      throw WireError.depthExceeded(MAX_NESTING_DEPTH);
    }

    this.#saved.push({ buf: this.#buf, len: this.#len });
    this.#outerLen += this.#len;
    this.#buf = this.#pool.pop() ?? new Uint8Array(64);
    this.#len = 0;

    let body: Uint8Array = EMPTY;
    let bodyLen = 0;
    try {
      write(this);
    } finally {
      // Restore inside `finally` so a throw from `write` cannot leave this writer
      // pointing at an inner buffer. Nobody should reuse a Writer after an error, but a
      // codec that corrupts its own state on the error path makes the *next* failure
      // unreadable — and that is the one somebody will be debugging at 3am.
      body = this.#buf;
      bodyLen = this.#len;
      const outer = this.#saved.pop();
      if (outer !== undefined) {
        this.#buf = outer.buf;
        this.#len = outer.len;
        this.#outerLen -= outer.len;
      }
    }

    this.#guardGrowth(bodyLen);
    varint.encodeU64(fieldId, this);
    varint.encodeU64(bodyLen, this);
    this.#extend(body.subarray(0, bodyLen));
    this.#pool.push(body);
  }

  /** Finishes the frame, checking the size limit one last time. */
  finish(): Uint8Array {
    if (this.#saved.length > 0) {
      throw new Error('Writer.finish called while an optional field is still open');
    }
    if (this.#len > MAX_FRAME_BYTES) {
      throw WireError.frameTooLarge(this.#len, MAX_FRAME_BYTES);
    }
    return this.#buf.slice(0, this.#len);
  }

  #extend(bytes: Uint8Array): void {
    this.#reserve(bytes.length);
    this.#buf.set(bytes, this.#len);
    this.#len += bytes.length;
  }

  #reserve(additional: number): void {
    const needed = this.#len + additional;
    if (needed <= this.#buf.length) {
      return;
    }
    let capacity = Math.max(this.#buf.length, 16) * 2;
    while (capacity < needed) {
      capacity *= 2;
    }
    const grown = new Uint8Array(capacity);
    grown.set(this.#buf.subarray(0, this.#len));
    this.#buf = grown;
  }

  /**
   * Refuses a write that would take the frame past the limit.
   *
   * Checked against the projected total rather than the current one, so a 300 KiB string
   * is rejected before it is copied. That is the difference between a bounded encoder and
   * one that allocates exactly what it is about to throw away.
   */
  #guardGrowth(additional: number): void {
    const projected = this.length + additional;
    if (projected > MAX_FRAME_BYTES) {
      throw WireError.frameTooLarge(projected, MAX_FRAME_BYTES);
    }
  }
}
