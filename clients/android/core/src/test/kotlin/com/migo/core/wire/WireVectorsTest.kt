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
 * The wire conformance vectors: the same three files the Rust crate and the web build are
 * tested against, read from `shared/protocol/vectors/wire`.
 *
 * This is the Android half of the agreement, and it was the missing one. [Flags], [Varint],
 * [Frame] and the MSE codec all documented themselves by reference to these vectors — the
 * comment on [decodeHeader] says the vectors probe the `0xFFFFFFFF` correlation boundary, and
 * [WireTime] says a build that forgets the epoch "fails crypto-free, in mse.json, on the first
 * timestamp case". None of that was true of this module: nothing here read the files, so a
 * Kotlin codec that disagreed with the other two about a single byte would have passed every
 * test in this source set. A test suite that compares an implementation to itself cannot find
 * the one bug class this directory exists for.
 *
 * Both directions are checked for every case, because a decoder that is wrong in a way its own
 * encoder compensates for passes a round-trip test and breaks the *other* client.
 *
 * The `invalid` sections matter as much as the happy path. A decoder that accepts a four-
 * gigabyte length prefix is a remote out-of-memory primitive, and no amount of round-tripping
 * valid frames will find it.
 */
class WireVectorsTest {

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

    /**
     * A section as a non-empty array. Empty is a failure rather than a skip: a vector suite that
     * silently runs zero cases reports coverage it does not have, and does so for as long as
     * nobody looks.
     */
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

    /** As [num], but across the full unsigned 64-bit range. */
    private fun bigNum(obj: JsonObject, key: String): ULong =
        (obj[key] ?: fail("field `$key` is missing: $obj")).jsonPrimitive.content.toULong()

    private fun small(obj: JsonObject, key: String): Int = num(obj, key).toInt()

    /** A field that may be absent or JSON `null`, which mean the same thing in these files. */
    private fun opt(obj: JsonObject, key: String): JsonElement? =
        obj[key]?.takeIf { it !is JsonNull }

    private fun hex(text: String): ByteArray =
        ByteArray(text.length / 2) { i -> text.substring(i * 2, i * 2 + 2).toInt(16).toByte() }

    private fun hexOf(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it.toInt() and 0xFF) }

    /**
     * Asserts that [block] fails with the error the case names, and says which case when it does
     * not — a bare "expected an exception" in a loop over forty cases tells you nothing when it
     * breaks. The variant name is the stable identity, which is why [WireErrorKind] duplicates
     * the Rust enum's spellings.
     */
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

    // --- varint -------------------------------------------------------------

    @Test
    fun varintsEncodeAndDecodeAsTheVectorsSay() {
        for (case in section(load("varint.json"), "cases", "varint.json")) {
            val value = bigNum(case, "value")
            val expected = hex(text(case, "hex"))

            val sink = ByteAccumulator(expected.size + 4)
            Varint.encodeU64(value, sink)
            assertEquals("encoding $value for `${name(case)}`", text(case, "hex"), hexOf(sink.toByteArray()))
            assertEquals("predicted length for `${name(case)}`", expected.size, Varint.encodedLen(value))

            val scanned = Varint.scan(expected, 0)
            assertEquals("decoding `${name(case)}`", value, scanned.value)
            assertEquals("bytes consumed by `${name(case)}`", expected.size, scanned.used)
        }
    }

    @Test
    fun theZigzagMappingMatchesTheVectors() {
        for (case in section(load("varint.json"), "zigzag", "varint.json")) {
            val signed = text(case, "value").toLong()
            val encoded = bigNum(case, "encoded")
            assertEquals("zigzagEncode for `${name(case)}`", encoded, Varint.zigzagEncode(signed))
            assertEquals("zigzagDecode for `${name(case)}`", signed, Varint.zigzagDecode(encoded))
        }
    }

    @Test
    fun malformedVarintsAreRejected() {
        for (case in section(load("varint.json"), "invalid", "varint.json")) {
            val input = hex(text(case, "hex"))
            expectError(case, "varint") { Varint.scan(input, 0) }
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

    /**
     * Compares headers field by field. [TraceContext] is deliberately not a data class — the
     * codec never compares two trace contexts — so the bytes are compared here.
     */
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

    @Test
    fun framesEncodeAndDecodeAsTheVectorsSay() {
        for (case in section(load("frames.json"), "cases", "frames.json")) {
            val spec = (case["frame"] ?: fail("`${name(case)}` has no frame")).jsonObject
            val expected = hex(text(case, "hex"))
            val header = headerFromCase(spec)
            val payload = hex(text(spec, "payload"))

            val encoded = encodeFrame(Frame(header, payload))
            assertEquals("encoding `${name(case)}`", text(case, "hex"), hexOf(encoded))
            assertEquals(
                "predicted length for `${name(case)}`",
                expected.size,
                headerEncodedLen(header) + payload.size,
            )

            val decoded = decodeFrame(expected)
            assertSameHeader(name(case), header, decoded.header)
            assertEquals("decoded payload for `${name(case)}`", hexOf(payload), hexOf(decoded.payload))
        }
    }

    @Test
    fun lengthPrefixedFramesMatchTheVectors() {
        for (case in section(load("frames.json"), "length_prefixed", "frames.json")) {
            val body = hex(text(case, "frame_hex"))
            val expected = hex(text(case, "hex"))

            val frame = decodeFrame(body)
            assertEquals(
                "length-prefixed encoding of `${name(case)}`",
                text(case, "hex"),
                hexOf(encodeFrameLengthPrefixed(frame)),
            )

            val parsed = decodeFrameLengthPrefixed(expected)
                ?: fail("`${name(case)}` must be a complete frame")
            assertEquals("`${name(case)}` consumed", expected.size, parsed.consumed)
            assertSameHeader(name(case), frame.header, parsed.frame.header)
            assertEquals(
                "`${name(case)}` payload",
                hexOf(frame.payload),
                hexOf(parsed.frame.payload),
            )
        }
    }

    @Test
    fun malformedFramesAreRejected() {
        for (case in section(load("frames.json"), "invalid", "frames.json")) {
            val input = hex(text(case, "hex"))
            expectError(case, "frame") { decodeFrame(input) }
        }
    }

    // --- MSE ----------------------------------------------------------------

    /**
     * Replays a writer program from a vector file.
     *
     * One interpreter covers every struct shape in the schema, which is why the vectors describe
     * programs rather than named types: neither this runner nor the ones in the other two
     * languages has to be regenerated when a struct is added.
     */
    private fun writeOps(writer: Writer, ops: List<JsonObject>) {
        for (op in ops) {
            when (text(op, "op")) {
                "enter" -> writer.enter()
                "leave" -> writer.leave()
                "bool" -> writer.bool(op["value"]!!.jsonPrimitive.content.toBooleanStrict())
                "u32" -> writer.u32(num(op, "value"))
                // The `u64` op covers the full unsigned range: the vectors carry u64::MAX, which
                // the signed-shaped overload refuses by design.
                "u64" -> writer.u64big(bigNum(op, "value"))
                "timestamp" -> writer.timestamp(WireTime.fromWire(num(op, "value")))
                "id" -> writer.id(idFromBytes(hex(text(op, "value"))))
                "string" -> writer.str(text(op, "value"))
                "bytes" -> writer.bytes(hex(text(op, "value")))
                "list_len" -> writer.listLen(num(op, "value").toInt())
                "optional" -> {
                    val fieldId = small(op, "id")
                    val nested = (op["ops"] ?: fail("optional without ops")).jsonArray
                        .map { it.jsonObject }
                    writer.optional(fieldId) { child -> writeOps(child, nested) }
                }
                else -> fail("unknown write op `${text(op, "op")}`")
            }
        }
    }

    /**
     * Replays the same program as reads.
     *
     * An op carrying a `value` is asserted against it; an op without one is read and discarded,
     * which is what the malformed-input cases need. An op marked `unknown` is the
     * forward-compatibility path: the field is dropped by its length instead of being decoded,
     * exactly as a generated decoder does with a field id from a newer peer.
     */
    private fun readOps(reader: Reader, ops: List<JsonObject>, case: String) {
        for (op in ops) {
            when (text(op, "op")) {
                "enter" -> reader.enter()
                "leave" -> reader.leave()
                "bool" -> {
                    val got = reader.bool()
                    opt(op, "value")?.let {
                        assertEquals("bool in `$case`", it.jsonPrimitive.content.toBooleanStrict(), got)
                    }
                }
                "u32" -> {
                    val got = reader.u32()
                    opt(op, "value")?.let { assertEquals("u32 in `$case`", num(op, "value"), got) }
                }
                "u64" -> {
                    val got = reader.u64big()
                    opt(op, "value")?.let { assertEquals("u64 in `$case`", bigNum(op, "value"), got) }
                }
                "timestamp" -> {
                    val got = reader.timestamp()
                    opt(op, "value")?.let {
                        assertEquals("timestamp in `$case`", num(op, "value"), WireTime.toWire(got))
                    }
                }
                "id" -> {
                    val got = reader.id()
                    opt(op, "value")?.let {
                        assertEquals("id in `$case`", text(op, "value"), hexOf(idToBytes(got)))
                    }
                }
                "string" -> {
                    val got = reader.str()
                    opt(op, "value")?.let { assertEquals("string in `$case`", text(op, "value"), got) }
                }
                "bytes" -> {
                    val got = reader.bytes()
                    opt(op, "value")?.let {
                        assertEquals("bytes in `$case`", text(op, "value"), hexOf(got))
                    }
                }
                "list_len" -> {
                    val got = reader.listLen()
                    opt(op, "value")?.let {
                        assertEquals("list_len in `$case`", num(op, "value").toInt(), got)
                    }
                }
                "optional" -> {
                    val (fieldId, inner) = reader.optional()
                    opt(op, "id")?.let {
                        assertEquals("field id in `$case`", small(op, "id").toLong(), fieldId)
                    }
                    // An unknown field is dropped with its sub-reader. That the outer position is
                    // already past it is the property under test, and the outer `finish` checks it.
                    val unknown = opt(op, "unknown")
                        ?.jsonPrimitive?.content?.toBooleanStrictOrNull() ?: false
                    val nested = opt(op, "ops")?.jsonArray?.map { it.jsonObject }
                    if (!unknown && nested != null) {
                        readOps(inner, nested, case)
                        inner.finish()
                    }
                }
                else -> fail("unknown read op `${text(op, "op")}`")
            }
        }
    }

    @Test
    fun mseProgramsEncodeAndDecodeAsTheVectorsSay() {
        for (case in section(load("mse.json"), "cases", "mse.json")) {
            val ops = (case["ops"] ?: fail("`${name(case)}` has no ops")).jsonArray.map { it.jsonObject }
            val expected = hex(text(case, "hex"))

            val writer = Writer()
            writeOps(writer, ops)
            assertEquals("encoding `${name(case)}`", text(case, "hex"), hexOf(writer.finish()))

            val reader = Reader(expected)
            readOps(reader, ops, name(case))
            reader.finish()
        }
    }

    @Test
    fun malformedMseIsRejected() {
        for (case in section(load("mse.json"), "invalid", "mse.json")) {
            val input = hex(text(case, "hex"))
            val ops = (case["read_ops"] ?: fail("`${name(case)}` has no read_ops"))
                .jsonArray.map { it.jsonObject }
            expectError(case, "mse") {
                val reader = Reader(input)
                readOps(reader, ops, name(case))
                reader.finish()
            }
        }
    }

    // --- the suite is present at all ----------------------------------------

    @Test
    fun everyVectorFileIsPresentAndPopulated() {
        // Guards against the failure this whole directory exists to prevent being itself defeated
        // by a missing file: deleting `mse.json` must not turn three tests into three silent
        // no-ops, and renaming a section must not either.
        val expected = listOf(
            "varint.json" to listOf("cases", "zigzag", "invalid"),
            "frames.json" to listOf("cases", "length_prefixed", "invalid"),
            "mse.json" to listOf("cases", "invalid"),
        )
        var total = 0
        for ((file, sections) in expected) {
            val loaded = load(file)
            assertNotNull("$file must record where its expected bytes came from", loaded["provenance"])
            for (entry in sections) total += section(loaded, entry, file).size
        }
        assertTrue("only $total wire vector cases, expected at least 60", total >= 60)
    }
}

/**
 * Fails the case with [message].
 *
 * JUnit 4's `Assert.fail` returns `void`, which Kotlin types as `Unit` and not as `Nothing`. That
 * matters everywhere this suite uses the elvis form to reject a missing field: `file[name] ?:
 * fail(...)` would infer the common supertype `Any` instead of keeping the field's own type, and
 * every access downstream of it — `.jsonArray`, `.jsonPrimitive`, `.jsonObject` — collapses into a
 * receiver-type mismatch. Declaring `Nothing` is what makes the elvis form say what it means: the
 * right-hand side never returns, so the expression is the left-hand side's type.
 */
private fun fail(message: String): Nothing = throw AssertionError(message)
