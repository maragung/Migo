/**
 * Codec failures.
 *
 * One class, one `kind` discriminant. The kind strings are the Rust enum's variant
 * names, character for character, because the conformance vectors in
 * `shared/protocol/vectors` name the expected failure and both languages have to read
 * that name the same way. A vector that says `NonMinimalVarint` must mean the same
 * thing here as it does in `migo-wire`, and the cheapest way to guarantee that is to
 * refuse to invent a second vocabulary.
 *
 * These errors never carry payload bytes. A hex dump of attacker-controlled data in a
 * log line is a log-injection vector and, if the frame was a private message, a
 * privacy incident.
 */

/** The closed set of things that can go wrong in the codec. */
export type WireErrorKind =
  | 'UnexpectedEnd'
  | 'VarintTooLong'
  | 'NonMinimalVarint'
  | 'StringTooLong'
  | 'BytesTooLong'
  | 'ListTooLong'
  | 'DepthExceeded'
  | 'FrameTooLarge'
  | 'InvalidUtf8'
  | 'InvalidBool'
  | 'UnsupportedVersion'
  | 'ReservedFlags'
  | 'TrailingBytes'
  | 'BatchTooLarge'
  | 'NestedBatch'
  | 'DecompressFailed'
  | 'DecompressedTooLarge'
  | 'LengthOverflow'
  | 'InvalidFragment'
  | 'FieldOverflow';

/** Numbers and static strings only — never peer-supplied text or bytes. */
export type WireErrorDetail = Readonly<Record<string, number | string>>;

/**
 * A codec failure.
 *
 * `instanceof` works across the whole workspace because there is exactly one of these,
 * which matters for the caller that wants to distinguish "the peer sent nonsense"
 * (drop the connection) from "a bug in our own encoder" (report it).
 */
export class WireError extends Error {
  readonly kind: WireErrorKind;
  readonly detail: WireErrorDetail;

  constructor(kind: WireErrorKind, message: string, detail: WireErrorDetail = {}) {
    super(message);
    this.name = 'WireError';
    this.kind = kind;
    this.detail = detail;
  }

  static unexpectedEnd(offset: number, needed: number): WireError {
    return new WireError(
      'UnexpectedEnd',
      `unexpected end of input: needed ${needed} more bytes at offset ${offset}`,
      { offset, needed },
    );
  }

  static varintTooLong(offset: number, max: number): WireError {
    return new WireError('VarintTooLong', `varint longer than ${max} bytes at offset ${offset}`, {
      offset,
      max,
    });
  }

  static nonMinimalVarint(offset: number): WireError {
    return new WireError('NonMinimalVarint', `non-minimal varint encoding at offset ${offset}`, {
      offset,
    });
  }

  static stringTooLong(len: number, max: number): WireError {
    return new WireError('StringTooLong', `string field is ${len} bytes, limit is ${max}`, {
      len,
      max,
    });
  }

  static bytesTooLong(len: number, max: number): WireError {
    return new WireError('BytesTooLong', `bytes field is ${len} bytes, limit is ${max}`, {
      len,
      max,
    });
  }

  static listTooLong(len: number, max: number): WireError {
    return new WireError('ListTooLong', `list has ${len} items, limit is ${max}`, { len, max });
  }

  static depthExceeded(max: number): WireError {
    return new WireError('DepthExceeded', `struct nesting deeper than ${max}`, { max });
  }

  static frameTooLarge(len: number, max: number): WireError {
    return new WireError('FrameTooLarge', `frame is ${len} bytes, limit is ${max}`, { len, max });
  }

  static invalidUtf8(): WireError {
    return new WireError('InvalidUtf8', 'string field is not valid UTF-8');
  }

  static invalidBool(found: number): WireError {
    return new WireError('InvalidBool', `boolean byte is ${found}, expected 0 or 1`, { found });
  }

  static unsupportedVersion(found: number, supported: number): WireError {
    return new WireError(
      'UnsupportedVersion',
      `unsupported protocol version ${found}, this build speaks ${supported}`,
      { found, supported },
    );
  }

  static reservedFlags(bits: number): WireError {
    return new WireError(
      'ReservedFlags',
      `reserved flag bits set: 0x${bits.toString(16).padStart(2, '0')}`,
      { bits },
    );
  }

  static trailingBytes(count: number): WireError {
    return new WireError('TrailingBytes', `${count} trailing bytes after decoding`, { count });
  }

  static batchTooLarge(len: number, max: number): WireError {
    return new WireError('BatchTooLarge', `batch has ${len} items, limit is ${max}`, { len, max });
  }

  static nestedBatch(): WireError {
    return new WireError('NestedBatch', 'nested batch frames are not allowed');
  }

  static decompressFailed(): WireError {
    return new WireError('DecompressFailed', 'cannot decompress payload');
  }

  static decompressedTooLarge(max: number): WireError {
    return new WireError(
      'DecompressedTooLarge',
      `payload expands past the ${max} byte limit when decompressed`,
      { max },
    );
  }

  /**
   * A length did not fit the platform's index type.
   *
   * On the Rust side this is a `usize` conversion failure; here it is the point where
   * a `u64` off the wire exceeds what a JavaScript number can index. Same name because
   * it is the same event from the peer's point of view: a length nobody can honour.
   */
  static lengthOverflow(len: number | bigint): WireError {
    return new WireError('LengthOverflow', `length prefix ${len} does not fit an array index`, {
      len: String(len),
    });
  }

  static invalidFragment(index: number, total: number): WireError {
    return new WireError('InvalidFragment', `invalid fragment ${index} of ${total}`, {
      index,
      total,
    });
  }

  /** `field` is a static string chosen by this package, never peer-supplied text. */
  static fieldOverflow(field: string): WireError {
    return new WireError('FieldOverflow', `value does not fit field \`${field}\``, { field });
  }
}
