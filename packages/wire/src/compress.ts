/**
 * Payload compression.
 *
 * Raw DEFLATE (RFC 1951), through the platform's own `CompressionStream('deflate-raw')`.
 * The algorithm was chosen *for this side*: browsers ship raw DEFLATE natively, so the web
 * client gets compression for zero bundle bytes. Brotli would compress a little better and
 * cost every user a WASM download; zstd is not in browsers at all. See ADR-0002.
 *
 * WebSocket `permessage-deflate` is deliberately disabled in favour of this. Per-message
 * compression at the protocol layer lets us apply a *policy* — compress only when it pays
 * — and keeps a shared compression window from leaking information between messages.
 *
 * Two guards, both mandatory:
 *
 * * **Never expand.** Compression that does not save at least
 *   {@link COMPRESS_MIN_GAIN_PERCENT} is discarded, and payloads under
 *   {@link COMPRESS_MIN_BYTES} are never attempted. A 40-byte typing indicator grows under
 *   DEFLATE.
 * * **Bounded inflation.** Decompression stops at {@link MAX_FRAME_BYTES}. A few hundred
 *   bytes of crafted DEFLATE can otherwise expand to gigabytes, which makes an unbounded
 *   decompressor a remote kill switch.
 *
 * These functions are `async` because the Web Streams API is. That is the reason
 * `decodeFrame` hands back an opaque payload instead of inflating it: a synchronous codec
 * cannot call an asynchronous decompressor, and making the whole decoder async to serve the
 * small minority of compressed frames would be the wrong trade.
 */

import { WireError } from './errors.js';
import { COMPRESS_MIN_BYTES, COMPRESS_MIN_GAIN_PERCENT, MAX_FRAME_BYTES } from './limits.js';

/** True when this runtime can compress. Node gained `deflate-raw` in 21.2. */
export function isCompressionAvailable(): boolean {
  return typeof CompressionStream === 'function' && typeof DecompressionStream === 'function';
}

/** Compresses `payload` with raw DEFLATE. */
export async function deflateRaw(payload: Uint8Array): Promise<Uint8Array> {
  return transform(new CompressionStream('deflate-raw'), payload, Number.POSITIVE_INFINITY);
}

/**
 * Applies the compression policy.
 *
 * Returns the compressed bytes only when compression is worth the CPU on both sides;
 * otherwise `null`, and the caller sends the payload uncompressed with the `COMPRESSED`
 * flag clear.
 */
export async function maybeDeflate(payload: Uint8Array): Promise<Uint8Array | null> {
  if (payload.length < COMPRESS_MIN_BYTES || !isCompressionAvailable()) {
    return null;
  }
  const compressed = await deflateRaw(payload);
  if (compressed.length >= payload.length) {
    return null;
  }
  const saved = payload.length - compressed.length;
  const gainPercent = Math.floor((saved * 100) / payload.length);
  return gainPercent < COMPRESS_MIN_GAIN_PERCENT ? null : compressed;
}

/** Decompresses raw DEFLATE, refusing to produce more than `max` bytes. */
export async function inflateRaw(
  compressed: Uint8Array,
  max: number = MAX_FRAME_BYTES,
): Promise<Uint8Array> {
  const limit = Math.min(max, MAX_FRAME_BYTES);
  if (!isCompressionAvailable()) {
    throw WireError.decompressFailed();
  }
  return transform(new DecompressionStream('deflate-raw'), compressed, limit);
}

/**
 * Pumps `input` through `stream`, giving up as soon as the output passes `limit`.
 *
 * The check is inside the read loop, not after it. A bomb that expands to two gigabytes
 * must cost one chunk to detect, not two gigabytes — checking the total afterwards would
 * mean the attack had already succeeded by the time it was noticed.
 */
async function transform(
  stream: CompressionStream | DecompressionStream,
  input: Uint8Array,
  limit: number,
): Promise<Uint8Array> {
  const writer = stream.writable.getWriter();
  // The platform types describe a compression stream's readable side as a stream of `any`.
  // Narrowing it here, once, is what keeps the loop below free of casts — and a cast inside a
  // loop over attacker-supplied bytes is exactly where an unchecked assumption hides.
  const reader = (stream.readable as ReadableStream<Uint8Array>).getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;

  const pump = (async () => {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = value;
      total += chunk.length;
      if (total > limit) {
        await reader.cancel().catch(() => undefined);
        throw WireError.decompressedTooLarge(limit);
      }
      chunks.push(chunk);
    }
  })();

  const feed = (async () => {
    await writer.write(input);
    await writer.close();
  })();

  // Both sides are awaited together, and neither may be abandoned. When the platform
  // rejects invalid DEFLATE it rejects *both* the write and the read, and a `try` that
  // awaited them in sequence would return on the first failure and leave the second
  // promise unhandled — which in Node terminates the process with the very platform error
  // this function exists to convert. `allSettled` is the whole fix.
  const results = await Promise.allSettled([feed, pump]);
  let failed = false;
  for (const result of results) {
    if (result.status !== 'rejected') {
      continue;
    }
    // A `WireError` is this package's own verdict — the size limit — and outranks the
    // platform's complaint about the stream it was cancelled out from under.
    if (result.reason instanceof WireError) {
      throw result.reason;
    }
    failed = true;
  }
  if (failed) {
    // Anything else means the bytes were not valid DEFLATE. The underlying message is not
    // something a caller can act on, and it may quote the payload — which is exactly what
    // these errors are forbidden to carry.
    throw WireError.decompressFailed();
  }

  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}
