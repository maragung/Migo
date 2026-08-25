/**
 * The byte reader and writer shared by the two envelope encoders.
 *
 * The 1:1 layer ({@link file://./session-crypto.ts}) and the group layer
 * ({@link file://./group-crypto.ts}) both assemble the opaque `envelope` field of a `MessageSend`
 * from raw bytes and varints — section 11 forbids JSON there, and MSE is the wrong tool because the
 * envelope is a flat, scheme-specific byte layout rather than a schema struct. This pair is that raw
 * codec: fixed-width blocks, single bytes, and LEB128 varints, with bounds checks so a short or
 * malformed envelope fails cleanly rather than reading past its end.
 */

import { varint } from '@migo/wire';

import { SdkError } from './errors.js';

/**
 * A minimal binary writer for an envelope.
 *
 * Scalars and varints accumulate in a small number array; fixed-width blocks (keys, ciphertext) are
 * kept as their own chunks so they are copied once. Everything is joined at {@link finish}.
 */
export class EnvelopeWriter {
  readonly #chunks: Uint8Array[] = [];
  #scalar: number[] = [];

  /** Appends one byte. */
  u8(byte: number): void {
    this.#scalar.push(byte & 0xff);
  }

  /** Appends an unsigned LEB128 varint. */
  varint(value: number): void {
    varint.encodeU64(value, this.#scalar);
  }

  /** Appends a fixed-width block verbatim. */
  bytes(block: Uint8Array): void {
    this.#flushScalar();
    this.#chunks.push(block);
  }

  /** Concatenates everything written so far into one buffer. */
  finish(): Uint8Array {
    this.#flushScalar();
    let total = 0;
    for (const chunk of this.#chunks) {
      total += chunk.length;
    }
    const out = new Uint8Array(total);
    let offset = 0;
    for (const chunk of this.#chunks) {
      out.set(chunk, offset);
      offset += chunk.length;
    }
    return out;
  }

  #flushScalar(): void {
    if (this.#scalar.length > 0) {
      this.#chunks.push(Uint8Array.from(this.#scalar));
      this.#scalar = [];
    }
  }
}

/** A minimal binary reader with bounds checks, so a short or malformed envelope fails cleanly. */
export class EnvelopeReader {
  readonly #bytes: Uint8Array;
  #offset = 0;

  constructor(bytes: Uint8Array) {
    this.#bytes = bytes;
  }

  /** Reads one byte, or throws if the envelope ended. */
  u8(): number {
    const byte = this.#bytes[this.#offset];
    if (byte === undefined) {
      throw new SdkError('envelope: truncated');
    }
    this.#offset += 1;
    return byte;
  }

  /** Reads an unsigned LEB128 varint as a `number`. */
  varint(): number {
    const { value, used } = varint.decodeU32(this.#bytes, this.#offset);
    this.#offset += used;
    return value;
  }

  /** Reads a fixed-width block of `length` bytes, or throws if fewer remain. */
  take(length: number): Uint8Array {
    if (this.#offset + length > this.#bytes.length) {
      throw new SdkError('envelope: truncated');
    }
    const block = this.#bytes.slice(this.#offset, this.#offset + length);
    this.#offset += length;
    return block;
  }

  /** Reads everything left, consuming the reader. */
  rest(): Uint8Array {
    const block = this.#bytes.slice(this.#offset);
    this.#offset = this.#bytes.length;
    return block;
  }
}
