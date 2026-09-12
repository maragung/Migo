package com.migo.core.wire

import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The BATCH and COMPRESSED conformance vectors, read from `shared/protocol/vectors/wire`.
 *
 * The same two files the Rust crate and the web build are tested against: `batch.json` pins
 * the envelope that packs whole frames into one transport message, `compress.json` pins the
 * raw DEFLATE streams a COMPRESSED frame carries and the policy that decides when to
 * compress. Until this file existed neither construct was pinned in this language at all —
 * [encodeBatch] and [Compress] could have disagreed with the other two codecs about a single
 * byte and still passed every test in this source set.
 *
 * `batch.json` cases are both directions, because a decoder that is wrong in a way its own
 * encoder compensates for passes a round trip and breaks the *other* client. The compressed
 * cases are decode-only: two conforming DEFLATE encoders may emit different bytes for the
 * same input, so what is pinned is that the authored streams inflate, not that this codec
 * reproduces them byte for byte.
 */
class BatchCompressVectorsTest {

    // --- loading ------------------------------------------------------------

    private fun vectorsDir(): File {
        var dir = File(System.getProperty("user.dir")).absoluteFile
        while (true) {
            val candidate = File(dir, "shared/protocol/vectors/wire")
            if (candidate.isDirectory) return candidate
            dir = dir.parentFile ?: fail(
                "the conformance vectors are not above ${System.getProperty("user.dir")}",
            )
        }
    }

    private fun load(file: String): JsonObject {
        val path = File(vectorsDir(), file)
        if (!path.isFile) {
            fail("$path is missing; run python3 tools/vectors/generate_wire_vectors.py")
        }
        return Json.parseToJsonElement(path.readText()).jsonObject
    }

    /** A section as a non-empty array: an empty one is a failure, never a silent skip. */
    private fun section(file: JsonObject, name: String, path: String): List<JsonObject> {
        val raw = file[name] ?: fail("$path has no `$name` section")
        val array = raw.jsonArray
        assertTrue("$path `$name` is empty", array.isNotEmpty())
        return array.map { it.jsonObject }
    }

    // --- field access -------------------------------------------------------

    private fun text(obj: JsonObject, key: String): String =
        (obj[key] ?: fail("missing string field `$key`: $obj")).jsonPrimitive.content

    private fun name(case: JsonObject): String = text(case, "name")

    /** A value the file may write as a JSON number or as a decimal string. */
    private fun num(obj: JsonObject, key: String): Long =
        (obj[key] ?: fail("field `$key` is missing: $obj")).jsonPrimitive.content.toLong()

    private fun small(obj: JsonObject, key: String): Int = num(obj, key).toInt()

    /** A field that may be absent or JSON `null`, which mean the same thing in these files. */
    private fun opt(obj: JsonObject, key: String): JsonElement? =
        obj[key]?.takeIf { it !is JsonNull }

    private fun hex(text: String): ByteArray =
        ByteArray(text.length / 2) { i -> text.substring(i * 2, i * 2 + 2).toInt(16).toByte() }

    private fun hexOf(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it.toInt() and 0xFF) }

    private fun expectError(case: JsonObject, context: String, block: () -> Unit) {
        val expected = text(case, "error")
        val why = opt(case, "why")?.jsonPrimitive?.content ?: ""
        try {
            block()
            fail("$context `${name(case)}` was accepted but must fail with $expected: $why")
        } catch (error: WireError) {
            assertEquals(
                "$context `${name(case)}` failed with the wrong error: $why",
                expected,
                error.kind.name,
            )
        }
    }

    // --- frames -------------------------------------------------------------

    private fun headerFromCase(frame: JsonObject): FrameHeader {
        val trace = opt(frame, "trace")?.jsonObject?.let {
            TraceContext(hex(text(it, "trace_id")), hex(text(it, "span_id")))
        }
        val fragment = opt(frame, "fragment")?.jsonObject?.let {
            Fragment(num(it, "index"), num(it, "total"))
        }
        val metadata = opt(frame, "metadata")?.jsonObject?.let {
            MetadataBlock(
                num(it, "frame_seq"),
                num(it, "sent_at_delta"),
                opt(it, "payload_len")?.let { _ -> num(it, "payload_len") },
            )
        }
        return FrameHeader(
            version = small(frame, "version"),
            flags = small(frame, "flags"),
            opcode = num(frame, "opcode"),
            correlation = num(frame, "correlation"),
            trace = trace,
            fragment = fragment,
            metadata = metadata,
        )
    }

    /** Compares headers field by field; [TraceContext] is not a data class, so bytes are. */
    private fun assertSameHeader(case: String, expected: FrameHeader, actual: FrameHeader) {
        assertEquals("version for `$case`", expected.version, actual.version)
        assertEquals("flags for `$case`", expected.flags, actual.flags)
        assertEquals("opcode for `$case`", expected.opcode, actual.opcode)
        assertEquals("correlation for `$case`", expected.correlation, actual.correlation)
        assertEquals("fragment for `$case`", expected.fragment, actual.fragment)
        assertEquals("metadata for `$case`", expected.metadata, actual.metadata)
        if (expected.trace == null) {
            assertNull("unexpected trace for `$case`", actual.trace)
        } else {
            assertNotNull("missing trace for `$case`", actual.trace)
            assertEquals(
                "trace id for `$case`",
                hexOf(expected.trace.traceId),
                hexOf(actual.trace!!.traceId),
            )
            assertEquals(
                "span id for `$case`",
                hexOf(expected.trace.spanId),
                hexOf(actual.trace!!.spanId),
            )
        }
    }

    /** Builds the elements of a batch case, one frame per spec. */
    private fun elementsOf(case: JsonObject): List<Frame> {
        val raw = case["elements"] ?: fail("`${name(case)}` has no elements")
        return raw.jsonArray.map { spec ->
            val header = headerFromCase(spec.jsonObject)
            Frame(header, hex(text(spec.jsonObject, "payload")))
        }
    }

    /** Compares unpacked elements one by one: headers and payloads, in order. */
    private fun expectFrames(case: String, got: List<Frame>, expected: List<Frame>) {
        assertEquals("element count for `$case`", expected.size, got.size)
        for (i in expected.indices) {
            assertSameHeader("$case element $i", expected[i].header, got[i].header)
            assertEquals(
                "element $i payload for `$case`",
                hexOf(expected[i].payload),
                hexOf(got[i].payload),
            )
        }
    }

    // --- batch --------------------------------------------------------------

    @Test
    fun batchesEncodeAndDecodeAsTheVectorsSay() {
        for (case in section(load("batch.json"), "cases", "batch.json")) {
            val elements = elementsOf(case)
            val expected = hex(text(case, "hex"))

            val packed = encodeBatch(elements)
            assertEquals("packing `${name(case)}`", text(case, "hex"), hexOf(encodeFrame(packed)))

            val decoded = decodeFrame(expected)
            val envelope = (case["frame"] ?: fail("`${name(case)}` has no frame")).jsonObject
            assertSameHeader(name(case), headerFromCase(envelope), decoded.header)
            expectFrames(name(case), unpackFrame(decoded), elements)
        }
    }

    @Test
    fun compressedBatchesUnpackAsTheVectorsSay() {
        // Decode-only by design: the payload is raw DEFLATE, whose exact bytes are not
        // pinned across implementations. What is pinned is that the envelope carries both
        // flags, and that it unpacks to exactly these elements.
        for (case in section(load("batch.json"), "compressed_cases", "batch.json")) {
            val elements = elementsOf(case)
            val decoded = decodeFrame(hex(text(case, "hex")))

            assertEquals(
                "`${name(case)}` must carry both flags",
                Flags.BATCH or Flags.COMPRESSED,
                decoded.header.flags and (Flags.BATCH or Flags.COMPRESSED),
            )
            expectFrames(name(case), unpackFrame(decoded), elements)
        }
    }

    @Test
    fun malformedBatchesAreRejected() {
        for (case in section(load("batch.json"), "invalid", "batch.json")) {
            // The headers of these frames are well-formed; it is the payload that is
            // hostile, so the frame decodes and the envelope does not.
            val frame = decodeFrame(hex(text(case, "hex")))
            expectError(case, "batch") { unpackFrame(frame) }
        }
    }

    // --- compress -----------------------------------------------------------

    @Test
    fun deflateStreamsInflateAsTheVectorsSay() {
        for (case in section(load("compress.json"), "cases", "compress.json")) {
            val compressed = hex(text(case, "compressed_hex"))
            val plain = hex(text(case, "plain_hex"))

            assertEquals(
                "inflating `${name(case)}`",
                hexOf(plain),
                hexOf(Compress.inflateRaw(compressed, Limits.MAX_FRAME_BYTES)),
            )

            // The encode direction is not byte-pinned — two conforming DEFLATE encoders
            // may differ — but this implementation's own output must still inflate back
            // to the same plain bytes, or its compressor and decompressor disagree.
            val own = Compress.deflateRaw(plain)
            assertEquals(
                "own round trip for `${name(case)}`",
                hexOf(plain),
                hexOf(Compress.inflateRaw(own, Limits.MAX_FRAME_BYTES)),
            )
        }
    }

    @Test
    fun compressedFramesInflateToTheirPayloads() {
        for (case in section(load("compress.json"), "frames", "compress.json")) {
            val decoded = decodeFrame(hex(text(case, "hex")))
            val plain = hex(text(case, "plain_hex"))

            assertEquals(
                "`${name(case)}` must carry the COMPRESSED flag",
                Flags.COMPRESSED,
                decoded.header.flags and Flags.COMPRESSED,
            )
            val envelope = (case["frame"] ?: fail("`${name(case)}` has no frame")).jsonObject
            assertSameHeader(name(case), headerFromCase(envelope), decoded.header)
            assertEquals(
                "inflated payload for `${name(case)}`",
                hexOf(plain),
                hexOf(Compress.inflateRaw(decoded.payload, Limits.MAX_FRAME_BYTES)),
            )
        }
    }

    @Test
    fun theCompressionPolicyDecidesAsTheVectorsSay() {
        for (case in section(load("compress.json"), "policy", "compress.json")) {
            val plain = hex(text(case, "plain_hex"))
            val expected = opt(case, "compresses")?.jsonPrimitive?.content?.toBooleanStrict()
                ?: fail("`${name(case)}` must say whether it compresses")

            val decision = Compress.maybeDeflate(plain)
            assertEquals("policy decision for `${name(case)}`", expected, decision != null)
            if (decision != null) {
                assertEquals(
                    "compressed round trip for `${name(case)}`",
                    hexOf(plain),
                    hexOf(Compress.inflateRaw(decision, Limits.MAX_FRAME_BYTES)),
                )
            }
        }
    }

    @Test
    fun malformedDeflateIsRejected() {
        for (case in section(load("compress.json"), "invalid", "compress.json")) {
            val input = hex(text(case, "hex"))
            expectError(case, "compress") {
                Compress.inflateRaw(input, Limits.MAX_FRAME_BYTES)
            }
        }
    }

    // --- the suite is present at all ----------------------------------------

    @Test
    fun everyBatchAndCompressVectorFileIsPresentAndPopulated() {
        val expected = listOf(
            "batch.json" to listOf("cases", "compressed_cases", "invalid"),
            "compress.json" to listOf("cases", "frames", "policy", "invalid"),
        )
        for ((file, sections) in expected) {
            val loaded = load(file)
            assertNotNull("$file must record where its expected bytes came from", loaded["provenance"])
            for (entry in sections) section(loaded, entry, file)
        }
    }
}

/**
 * Fails the case with [message].
 *
 * JUnit 4's `Assert.fail` returns `void`, which Kotlin types as `Unit` and not as `Nothing`.
 * That matters everywhere this suite uses the elvis form to reject a missing field: the
 * right-hand side never returns, so the expression keeps the left-hand side's type instead of
 * widening to `Any`.
 */
private fun fail(message: String): Nothing = throw AssertionError(message)
