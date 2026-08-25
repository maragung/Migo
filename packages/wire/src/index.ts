/**
 * `@migo/wire` — MWP/1 framing and Migo Struct Encoding for TypeScript.
 *
 * This is the second implementation of a protocol whose first implementation is the Rust
 * crate `migo-wire`. It is not a port by inspection: both sides run the same vector files
 * in `shared/protocol/vectors/wire`, and the expected bytes in those files were computed
 * from the specification by a third, independent generator. Where this code and the Rust
 * code disagree, a vector fails and neither gets to be right by default.
 *
 * The module boundaries mirror the crate's, so a change on one side has an obvious home on
 * the other:
 *
 * * {@link Writer} / {@link Reader} — the MSE codec the generated `@migo/protocol` calls.
 * * `frame` — the MWP/1 header, with and without a length prefix.
 * * `batch` — the `BATCH` envelope, plus the one function a transport should call per
 *   inbound frame ({@link unpackFrame}).
 * * `compress` — raw DEFLATE, async because the Web Streams API is.
 * * `id`, `time` — the two value types that appear in almost every message.
 * * `limits`, `flags`, `errors` — the constants and the failure vocabulary.
 */

export { WireError, type WireErrorKind } from './errors.js';
export * as flags from './flags.js';
export * from './limits.js';
export * as varint from './varint.js';

export {
  ID_BYTE_LEN,
  ID_TEXT_LEN,
  NIL_ID,
  idFromBytes,
  idToBytes,
  idUnixMs,
  isId,
  parseId,
  tryParseId,
  type Id,
} from './id.js';

export { MIGO_EPOCH_MS, fromWire, toWire } from './time.js';

export { Reader } from './reader.js';
export { Writer } from './writer.js';

export {
  PROTOCOL_VERSION,
  TRACE_ENCODED_LEN,
  decodeFrame,
  decodeFrameLengthPrefixed,
  decodeHeader,
  encodeFrame,
  encodeFrameLengthPrefixed,
  encodeHeader,
  frameHeader,
  headerEncodedLen,
  isInvalidTrace,
  isLastFragment,
  type Fragment,
  type Frame,
  type FrameHeader,
  type TraceContext,
} from './frame.js';

export { BATCH_OPCODE, decodeBatchPayload, encodeBatch, unpackFrame } from './batch.js';

export { deflateRaw, inflateRaw, isCompressionAvailable, maybeDeflate } from './compress.js';
