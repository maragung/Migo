package com.migo.core.wire

import java.io.ByteArrayOutputStream
import java.util.zip.DataFormatException
import java.util.zip.Deflater
import java.util.zip.Inflater

/**
 * Payload compression: raw DEFLATE (RFC 1951).
 *
 * The web client reaches DEFLATE through the browser's own `CompressionStream('deflate-raw')`,
 * for zero bundle bytes; the JVM reaches the same algorithm through `java.util.zip`, which has
 * shipped it since Java 1.1. Because both sides speak standard raw DEFLATE, a payload
 * compressed on one decompresses on the other — which is all the protocol requires. The exact
 * compressed bytes are deliberately *not* pinned: they are a function of the zlib version, and
 * two conforming encoders may differ there and still interoperate.
 *
 * Two guards, both mandatory:
 *
 * * **Never expand.** Compression that does not save at least [Limits.COMPRESS_MIN_GAIN_PERCENT]
 *   is discarded, and payloads under [Limits.COMPRESS_MIN_BYTES] are never attempted — a
 *   40-byte typing indicator grows under DEFLATE.
 * * **Bounded inflation.** Decompression stops at the limit and is checked inside the read
 *   loop, so a few hundred bytes of crafted DEFLATE that would expand to gigabytes costs one
 *   chunk to detect, not gigabytes. An unbounded decompressor is a remote kill switch.
 *
 * Unlike the web port these functions are synchronous: `java.util.zip` does not force the
 * async ceremony the Web Streams API does.
 */
object Compress {
    private const val CHUNK = 8192

    /** True when this runtime can compress. Always true on the JVM. */
    fun isCompressionAvailable(): Boolean = true

    /** Compresses [payload] with raw DEFLATE. */
    fun deflateRaw(payload: ByteArray): ByteArray {
        val deflater = Deflater(Deflater.DEFAULT_COMPRESSION, /* nowrap = */ true)
        try {
            deflater.setInput(payload)
            deflater.finish()
            val out = ByteArrayOutputStream(maxOf(64, payload.size / 2))
            val chunk = ByteArray(CHUNK)
            while (!deflater.finished()) {
                val n = deflater.deflate(chunk)
                if (n > 0) out.write(chunk, 0, n)
            }
            return out.toByteArray()
        } finally {
            deflater.end()
        }
    }

    /**
     * Applies the compression policy. Returns the compressed bytes only when compression is
     * worth the CPU on both sides; otherwise `null`, and the caller sends the payload
     * uncompressed with the `COMPRESSED` flag clear.
     */
    fun maybeDeflate(payload: ByteArray): ByteArray? {
        if (payload.size < Limits.COMPRESS_MIN_BYTES) return null
        val compressed = deflateRaw(payload)
        if (compressed.size >= payload.size) return null
        val saved = payload.size - compressed.size
        val gainPercent = (saved * 100) / payload.size
        return if (gainPercent < Limits.COMPRESS_MIN_GAIN_PERCENT) null else compressed
    }

    /** Decompresses raw DEFLATE, refusing to produce more than [max] bytes. */
    fun inflateRaw(compressed: ByteArray, max: Int = Limits.MAX_FRAME_BYTES): ByteArray {
        val limit = minOf(max, Limits.MAX_FRAME_BYTES)
        val inflater = Inflater(/* nowrap = */ true)
        try {
            inflater.setInput(compressed)
            val out = ByteArrayOutputStream(minOf(limit, maxOf(64, compressed.size * 4)))
            val chunk = ByteArray(CHUNK)
            var total = 0
            while (!inflater.finished()) {
                val n = try {
                    inflater.inflate(chunk)
                } catch (_: DataFormatException) {
                    // The underlying message may quote the payload — which these errors are
                    // forbidden to carry — and is not something a caller can act on anyway.
                    throw WireError.decompressFailed()
                }
                if (n == 0) {
                    // No progress and not finished means either a truncated stream or a preset
                    // dictionary we do not supply; both are malformed input here.
                    if (inflater.finished()) break
                    throw WireError.decompressFailed()
                }
                total += n
                // Checked before the write and every chunk, so a bomb costs one chunk to
                // detect rather than its full expanded size.
                if (total > limit) throw WireError.decompressedTooLarge(limit)
                out.write(chunk, 0, n)
            }
            return out.toByteArray()
        } finally {
            inflater.end()
        }
    }
}
