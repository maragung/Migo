package com.migo.core.crypto

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.wire.Id
import com.migo.core.wire.idFromBytes
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.BeforeClass
import org.junit.Test

/**
 * The crypto conformance vectors — `kdf.json`, `aead.json` and `mac.json` — read from
 * `shared/protocol/vectors/crypto`.
 *
 * The account vectors already had an Android reader; these three did not. That left the layer
 * underneath them unpinned against anything outside this source set, and it is the layer where a
 * disagreement is quietest: a wrong HKDF label, a swapped `derivePair` split, or an XChaCha nonce
 * laid out at the wrong offset all produce code that round-trips against itself perfectly and
 * fails only against another client, on real messages, in production.
 *
 * Two properties of these files are what make them worth reading rather than re-deriving:
 *
 *   * the `rfc` sections are the specifications' own published vectors (RFC 5869, RFC 4231) run
 *     through this module's parameter shape, so a failure there is a genuine implementation bug;
 *   * the `independent-python` cases were computed by `tools/vectors/generate_crypto_vectors.py`
 *     from the RFC constructions in a different language, so a failure there is cross-language
 *     drift — the exact class of bug a single-language suite cannot see.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] — the same C
 * code the device runs, loaded for the host JVM.
 */
class CryptoVectorsTest {

    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }

        /**
         * The crypto vector directory, found by walking up from the module to the repo root.
         *
         * The search is a named function rather than an inline `lazy` body because a lambda's
         * result type is the common supertype of everything that can leave it — both its `return`
         * points and its last expression. Inline, the successful return is a [File] and the
         * trailing `while` is `Unit`, so the supertype is `Any` and `lazy` would hand back a
         * `Lazy<Any>`. Declaring `: File` here says what the search means and lets the compiler
         * check the loop instead of widening it.
         */
        private val vectorDir: File by lazy { findVectorDir() }

        private fun findVectorDir(): File {
            var dir = File(System.getProperty("user.dir")).absoluteFile
            while (true) {
                val candidate = File(dir, "shared/protocol/vectors/crypto")
                if (candidate.isDirectory) return candidate
                dir = dir.parentFile ?: fail(
                    "the conformance vectors are not above ${System.getProperty("user.dir")}",
                )
            }
        }

        private val json = Json { ignoreUnknownKeys = true }

        private fun load(file: String): JsonObject {
            val path = File(vectorDir, file)
            if (!path.isFile) {
                fail("$path is missing; run python3 tools/vectors/generate_crypto_vectors.py")
            }
            return json.parseToJsonElement(path.readText()).jsonObject
        }

        /** A section as a non-empty array: an empty one is a failure, never a silent skip. */
        private fun section(file: JsonObject, name: String, path: String): List<JsonObject> {
            val raw = file[name] ?: fail("$path has no `$name` section")
            val array = raw.jsonArray
            assertTrue("$path `$name` is empty", array.isNotEmpty())
            return array.map { it.jsonObject }
        }

        private fun text(obj: JsonObject, key: String): String =
            (obj[key] ?: fail("missing string field `$key`: $obj")).jsonPrimitive.content

        private fun name(case: JsonObject): String = text(case, "name")

        private fun int(obj: JsonObject, key: String): Int = text(obj, key).toInt()

        /** A field that may be absent or JSON `null`; both mean "not provided". */
        private fun opt(obj: JsonObject, key: String): JsonElement? = obj[key]?.takeIf { it !is JsonNull }

        private fun hex(text: String): ByteArray =
            ByteArray(text.length / 2) { i -> text.substring(i * 2, i * 2 + 2).toInt(16).toByte() }

        private fun assertHex(what: String, expected: String, actual: ByteArray) {
            // The package's own lowercase [hexOf], the same one the constructions use to render
            // public material — so a vector's hex and a produced byte string are compared by one
            // spelling and not two.
            assertEquals(what, expected, hexOf(actual))
        }

        /**
         * Asserts the block fails with the error the case names. The variant name is the stable
         * identity — [CryptoErrorKind] duplicates the Rust enum's spellings for this reason — so a
         * reworded message never breaks a vector.
         */
        private fun expectError(case: JsonObject, context: String, block: () -> Unit) {
            val expected = text(case, "error")
            val why = opt(case, "why")?.jsonPrimitive?.content ?: ""
            try {
                block()
                fail("$context `${name(case)}` was accepted but must fail with $expected: $why")
            } catch (error: CryptoError) {
                assertEquals(
                    "$context `${name(case)}` failed with the wrong error: $why",
                    expected,
                    error.kind.name,
                )
            }
        }
    }

    // --- kdf ----------------------------------------------------------------

    @Test
    fun kdfMatchesTheVectors() {
        for (case in section(load("kdf.json"), "cases", "kdf.json")) {
            val derived = Kdf.derive(
                secret = hex(text(case, "secret")),
                salt = opt(case, "salt")?.jsonPrimitive?.content?.let { hex(it) },
                label = text(case, "label"),
                length = int(case, "length"),
            )
            assertHex("kdf `${name(case)}`", text(case, "okm"), derived)
        }
    }

    @Test
    fun kdfMatchesTheRfcVectors() {
        // RFC 5869's own test vectors, where the info parameter is raw bytes rather than a label.
        for (case in section(load("kdf.json"), "rfc", "kdf.json")) {
            val derived = Kdf.derive(
                secret = hex(text(case, "secret")),
                salt = hex(text(case, "salt")),
                info = hex(text(case, "label_hex")),
                length = int(case, "length"),
            )
            assertHex("rfc kdf `${name(case)}`", text(case, "okm"), derived)
        }
    }

    @Test
    fun derivePairSplitsOneExpansion() {
        for (case in section(load("kdf.json"), "pairs", "kdf.json")) {
            val (first, second) = Kdf.derivePair(
                secret = hex(text(case, "secret")),
                salt = opt(case, "salt")?.jsonPrimitive?.content?.let { hex(it) },
                label = text(case, "label"),
                firstLength = int(case, "first_length"),
                secondLength = int(case, "second_length"),
            )
            assertHex("first of `${name(case)}`", text(case, "first"), first)
            assertHex("second of `${name(case)}`", text(case, "second"), second)
        }
    }

    // --- aead ---------------------------------------------------------------

    @Test
    fun aeadSealsAndOpensAsTheVectorsSay() {
        for (case in section(load("aead.json"), "cases", "aead.json")) {
            val key = SymmetricKey.fromBytes(hex(text(case, "key")))
            val nonce = hex(text(case, "nonce"))
            val aad = hex(text(case, "aad"))
            val plaintext = hex(text(case, "plaintext"))
            val sealed = hex(text(case, "sealed"))

            // Sealing under the vector's own nonce must reproduce the file's bytes exactly. This
            // is what pins the `nonce || ciphertext || tag` layout rather than merely its
            // round-trip: an implementation that emitted `ciphertext || tag || nonce` would pass
            // open(seal(x)) == x and fail here.
            assertHex(
                "sealing `${name(case)}`",
                text(case, "sealed"),
                Aead.sealWithNonce(key, nonce, aad, plaintext),
            )
            assertArrayEquals(
                "opening `${name(case)}`",
                plaintext,
                Aead.open(key, aad, sealed),
            )
        }
    }

    @Test
    fun malformedAeadIsRejected() {
        for (case in section(load("aead.json"), "invalid", "aead.json")) {
            val key = SymmetricKey.fromBytes(hex(text(case, "key")))
            val aad = hex(text(case, "aad"))
            val sealed = hex(text(case, "sealed"))
            expectError(case, "aead") { Aead.open(key, aad, sealed) }
        }
    }

    // --- mac ----------------------------------------------------------------

    @Test
    fun macKeysAndTagsMatchTheVectors() {
        for (case in section(load("mac.json"), "cases", "mac.json")) {
            val message = hex(text(case, "message"))
            val expected = text(case, "tag")

            assertHex(
                "derived tag for `${name(case)}`",
                expected,
                MacKey.derive(hex(text(case, "root")), text(case, "label")).tag(message),
            )
            // And the same tag through the file's own key bytes. [MacKey] keeps its key private —
            // there is no accessor, deliberately — so the derived key is pinned by what it does
            // rather than by what it contains: two keys are the same key exactly when they tag
            // identically, and the second assertion is what makes the first one about the
            // derivation rather than about a construction that ignores `root` and `label`.
            assertHex(
                "tag under the vector's own key for `${name(case)}`",
                expected,
                MacKey.fromBytes(hex(text(case, "key"))).tag(message),
            )
        }
    }

    @Test
    fun macTagPartsFramesEachPart() {
        for (case in section(load("mac.json"), "parts", "mac.json")) {
            val key = MacKey.derive(hex(text(case, "root")), text(case, "label"))
            val parts = (case["parts"] ?: fail("`${name(case)}` has no parts")).jsonArray
                .map { hex(it.jsonPrimitive.content) }
            assertHex("parts tag for `${name(case)}`", text(case, "tag"), key.tagParts(parts))
        }
    }

    @Test
    fun differentPartSplitsDoNotShareATag() {
        // The property [MacKey.tagParts] length-prefixes to get: without the framing, ("a","bc")
        // and ("ab","c") would collide into one tag, and one of them would be a forgery.
        val byName = HashMap<String, ByteArray>()
        for (file in listOf("cases", "parts")) {
            for (case in section(load("mac.json"), file, "mac.json")) {
                byName[name(case)] = hex(text(case, "tag"))
            }
        }
        for (pair in section(load("mac.json"), "distinct_pairs", "mac.json")) {
            val left = text(pair, "left")
            val right = text(pair, "right")
            val why = opt(pair, "why")?.jsonPrimitive?.content ?: ""
            val leftTag = byName[left] ?: fail("`$left` is not a case in mac.json")
            val rightTag = byName[right] ?: fail("`$right` is not a case in mac.json")
            assertNotEquals("`$left` and `$right` share a tag: $why", hexOf(leftTag), hexOf(rightTag))
        }
    }

    @Test
    fun macMatchesTheRfcVectors() {
        // RFC 4231's HMAC-SHA256 cases, with the key supplied directly rather than derived.
        for (case in section(load("mac.json"), "rfc", "mac.json")) {
            val key = MacKey.fromBytes(hex(text(case, "key")))
            assertHex("rfc tag for `${name(case)}`", text(case, "tag"), key.tag(hex(text(case, "message"))))
        }
    }

    @Test
    fun truncatedTagsAreAcceptedOnlyDownToTheFloor() {
        for (case in section(load("mac.json"), "truncation", "mac.json")) {
            val accepted = text(case, "accepted").toBooleanStrict()
            val why = opt(case, "why")?.jsonPrimitive?.content ?: ""
            val key = MacKey.derive(hex(text(case, "root")), text(case, "label"))
            val message = hex(text(case, "message"))
            val tag = key.tag(message).copyOf(int(case, "tag_len"))
            if (accepted) {
                key.verify(message, tag)
            } else {
                val kind = try {
                    key.verify(message, tag)
                    fail("`${name(case)}` verified but must be refused: $why")
                } catch (error: CryptoError) {
                    error.kind.name
                }
                assertEquals("`${name(case)}` refused for the wrong reason: $why", "BadLength", kind)
            }
        }
    }

    // --- aad context --------------------------------------------------------
    //
    // The envelope's bound context is the one structure here that four separate implementations
    // build: the Rust reference the server links, a second Rust build inside the desktop client, the
    // TypeScript SDK, and this client. None of them call each other, so nothing but this file keeps
    // their bytes identical — and a disagreement is not a test failure in the field, it is a message
    // that will not open, reported by a person as "it says delivered but nothing arrives".

    @Test
    fun aadContextsMatchTheVectors() {
        val file = load("aad-context.json")
        assertHex("the domain label", text(file, "domain"), Aad.domain)
        assertEquals(
            "the domain label is ascii, which is what lets a reader print it",
            text(file, "ascii"),
            String(Aad.domain, Charsets.US_ASCII),
        )

        for (case in section(file, "cases", "aad-context.json")) {
            val version = EnvelopeVersion.fromWire(int(case, "envelope_version"))
                ?: fail("`${name(case)}` names a version this build refuses")
            val scheme = int(case, "scheme")
            val sender = idFromBytes(hex(text(case, "sender_device")))
            val conversation = idFromBytes(hex(text(case, "conversation_id")))
            val messageId = opt(case, "message_id")
                ?.jsonPrimitive
                ?.content
                ?.let { idFromBytes(hex(it)) }

            val context = Aad.context(version, scheme, sender, conversation, messageId)
            assertHex("context for `${name(case)}`", text(case, "context"), context)

            // The assembled value is what a ratchet actually authenticates, so the case pins the
            // whole string and not just the half that is new.
            assertHex(
                "associated data for `${name(case)}`",
                text(case, "aad"),
                Aad.assemble(
                    hex(text(case, "associated_data")),
                    hex(text(case, "header")),
                    context,
                ),
            )

            // A context a reader could not take apart again would still authenticate, so this is not
            // redundant with the comparison above: it pins that the layout is unambiguous rather than
            // merely reproducible.
            //
            // Only a version that binds a context has one to take apart. A version-1 case carries the
            // empty context on purpose — that is the compatibility rule, and `parseContext` refusing
            // zero bytes is the rule holding rather than a gap here, so the refusal is asserted
            // instead of the case being skipped. An absent context is not a context.
            if (!version.bindsContext) {
                assertTrue("`${name(case)}` binds no context", context.isEmpty())
                assertEquals(
                    "`${name(case)}`: an absent context is not a context",
                    "BadLength",
                    kindOf { Aad.parseContext(context) },
                )
                continue
            }

            val parsed = try {
                Aad.parseContext(context)
            } catch (error: CryptoError) {
                fail("`${name(case)}` produced an unparseable context: ${error.message}")
            }
            assertEquals("`${name(case)}` version", version, parsed.version)
            assertEquals("`${name(case)}` scheme", scheme, parsed.scheme)
            assertEquals("`${name(case)}` sender", sender, parsed.senderDevice)
            assertEquals("`${name(case)}` conversation", conversation, parsed.conversationId)
            assertEquals("`${name(case)}` message id", messageId, parsed.messageId)
        }
    }

    @Test
    fun aVersionOneEnvelopeBindsNoContext() {
        // The compatibility rule, stated as a test rather than as a comment: a v1 envelope's
        // associated data must be `associated_data || header` and nothing else, whatever metadata the
        // caller happens to hold. If this ever stops being true, every v1 message already stored stops
        // opening, and it fails looking like corruption rather than looking like a version mistake.
        val file = load("aad-context.json")
        val sender = idFromBytes(ByteArray(16) { 0xa0.toByte() })
        val conversation = idFromBytes(ByteArray(16) { 0xc0.toByte() })
        val message = idFromBytes(ByteArray(16) { 0xe0.toByte() })

        val shapes: List<Pair<String, Id?>> =
            listOf("without a message id" to null, "with one" to message)
        for ((label, messageId) in shapes) {
            val context = Aad.context(EnvelopeVersion.V1, 1, sender, conversation, messageId)
            assertTrue(
                "a v1 context must be empty $label, got ${context.size} bytes",
                context.isEmpty(),
            )
        }

        // The two v1 cases in the file must therefore be byte-identical to the plain concatenation,
        // which is the claim in one line.
        val v1 = section(file, "cases", "aad-context.json")
            .filter { int(it, "envelope_version") == 1 }
        assertTrue("the file must carry the v1 rule both ways", v1.size >= 2)
        for (case in v1) {
            assertEquals(
                "v1 case `${name(case)}` is not a plain concatenation",
                text(case, "associated_data") + text(case, "header"),
                text(case, "aad"),
            )
            assertEquals("v1 case `${name(case)}`", "", text(case, "context"))
        }
    }

    @Test
    fun theVersionGateAcceptsExactlyTheVersionsThisBuildWrites() {
        val file = load("aad-context.json")
        for (case in section(file, "invalid", "aad-context.json")) {
            val value = int(case, "envelope_version")
            assertEquals(
                "`${name(case)}` (${text(case, "why")})",
                text(case, "error"),
                if (EnvelopeVersion.fromWire(value) == null) "UnsupportedVersion" else "accepted",
            )
        }

        // The sweep is the part a per-case list cannot express: a reader that accepted, say, only 255
        // and 0 would pass the cases above. Every byte can hold is checked against the two the
        // protocol defines, and each accepted version has to survive its own round trip.
        val declared = (file["versions"] ?: fail("aad-context.json declares no versions"))
            .jsonObject["accepted"]
            ?.jsonArray
            ?.map { it.jsonPrimitive.content.toInt() }
            ?: fail("aad-context.json declares no accepted versions")

        val writers = ArrayList<Int>()
        for (value in 0..255) {
            val version = EnvelopeVersion.fromWire(value) ?: continue
            writers.add(version.wire)
            assertEquals("a version must round-trip through its own byte", value, version.wire)
            assertEquals(
                "version $value disagrees with itself about binding a context",
                value == 2,
                version.bindsContext,
            )
        }
        assertEquals("the gate and the file disagree", declared, writers)
        assertEquals(
            "`ACCEPTED` is the gate, spelled out",
            declared,
            EnvelopeVersion.ACCEPTED.map { it.wire },
        )
    }

    @Test
    fun aContextCarryingBitsThisBuildDoesNotKnowIsRefused() {
        // The parser is not on the receive path — a receiver rebuilds the context and lets the tag
        // decide — but it is what the vectors above read the layout back through, so its two refusal
        // paths are pinned rather than left to a reader to discover by a wrong answer elsewhere.
        val context = Aad.context(
            EnvelopeVersion.V2,
            1,
            idFromBytes(ByteArray(16) { 0xa0.toByte() }),
            idFromBytes(ByteArray(16) { 0xc0.toByte() }),
        )
        assertEquals("the flags byte is the last of the fixed part", 52, context.size)

        val unknownFlag = context.copyOf()
        unknownFlag[unknownFlag.size - 1] = 0x80.toByte()
        assertEquals("an unknown flag must be refused", "MalformedHeader", kindOf {
            Aad.parseContext(unknownFlag)
        })

        val trailing = context + byteArrayOf(0)
        assertEquals("trailing bytes must be refused", "BadLength", kindOf {
            Aad.parseContext(trailing)
        })

        val truncated = context.copyOf(context.size - 1)
        assertEquals("a truncated context must be refused", "BadLength", kindOf {
            Aad.parseContext(truncated)
        })

        val wrongDomain = context.copyOf()
        wrongDomain[0] = (wrongDomain[0].toInt() xor 0x01).toByte()
        assertEquals("a context under another domain must be refused", "MalformedHeader", kindOf {
            Aad.parseContext(wrongDomain)
        })
    }

    // --- the suite is present at all ----------------------------------------

    @Test
    fun everyCryptoVectorFileIsPresentAndPopulated() {
        val expected = listOf(
            "kdf.json" to listOf("cases", "rfc", "pairs"),
            "aead.json" to listOf("cases", "invalid"),
            "mac.json" to listOf("cases", "parts", "distinct_pairs", "rfc", "truncation"),
            "aad-context.json" to listOf("cases", "invalid"),
        )
        var total = 0
        for ((file, sections) in expected) {
            val loaded = load(file)
            assertTrue("$file must record where its expected bytes came from", loaded.containsKey("provenance"))
            for (entry in sections) total += section(loaded, entry, file).size
        }
        assertTrue("only $total crypto vector cases, expected at least 50", total >= 50)
    }
}

/** The [CryptoErrorKind] name [body] fails with, for a refusal the vectors do not label. */
private fun kindOf(body: () -> Unit): String = try {
    body()
    "accepted"
} catch (error: CryptoError) {
    error.kind.name
}

/**
 * Fails the case with [message].
 *
 * JUnit 4's `Assert.fail` returns `void`, which Kotlin types as `Unit` and not as `Nothing`. That
 * matters everywhere this suite uses the elvis form to reject a missing field: `case["parts"] ?:
 * fail(...)` would infer the common supertype `Any` instead of keeping the field's own type, and
 * every access downstream of it — `.jsonArray`, `.jsonPrimitive` — collapses into a receiver-type
 * mismatch. Declaring `Nothing` is what makes the elvis form say what it means: the right-hand side
 * never returns, so the expression is the left-hand side's type.
 */
private fun fail(message: String): Nothing = throw AssertionError(message)
