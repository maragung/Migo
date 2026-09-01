package com.migo.core.account

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.crypto.Keccak
import com.migo.core.crypto.Sodium
import com.migo.core.crypto.hexOf
import java.io.File
import java.math.BigInteger
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * The transaction and EIP-712 conformance vectors: the same two files the Rust reference, the
 * TypeScript client and the web client are tested against, read from
 * `shared/protocol/vectors/crypto`.
 *
 * `account-tx.json` was written by an independent Python implementation plus one real Avalanche
 * C-Chain transaction, and `account-eip712.json` by the Python implementation plus the EIP-712
 * specification's own worked example. This suite is the Android half of the agreement: every body
 * byte, signing hash, recovered sender and EIP-712 digest this module produces is compared against
 * the file, not against a value this module itself computed. A port that is self-consistent and
 * still wrong is exactly what the chain-sourced case is here to catch.
 */
class TxVectorsTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            // The wallet derivation runs HKDF through the same C code the device runs; the
            // Android artifact's native library only loads on a device, so the desktop handle is
            // injected here exactly as the account-root suite does it.
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }

        /** The crypto vector directory, found by walking up from the module to the repo root. */
        private val vectorDir: File by lazy {
            var dir = File(System.getProperty("user.dir")).absoluteFile
            while (dir != null) {
                val candidate = File(dir, "shared/protocol/vectors/crypto")
                if (candidate.isDirectory) return@lazy candidate
                dir = dir.parentFile
            }
            fail("the conformance vectors are not above ${System.getProperty("user.dir")}")
            throw IllegalStateException("unreachable")
        }

        private val json = Json { ignoreUnknownKeys = true }

        private fun cases(file: String): List<JsonObject> =
            json.parseToJsonElement(File(vectorDir, file).readText())
                .jsonObject["cases"]!!
                .jsonArray
                .map { it.jsonObject }

        private fun hex(text: String): ByteArray =
            ByteArray(text.length / 2) { i -> text.substring(i * 2, i * 2 + 2).toInt(16).toByte() }

        private fun field(case: JsonObject, name: String): String = case[name]!!.jsonPrimitive.content

        /** The integer fields arrive as decimal strings: wei values live far above 2^53. */
        private fun bigInt(case: JsonObject, name: String): BigInteger = BigInteger(field(case, name))

        private fun assertHexBytes(what: String, expected: String, actual: ByteArray) {
            assertEquals(what, expected, hexOf(actual))
        }

        private fun assertBig(what: String, expected: BigInteger, actual: BigInteger) {
            assertEquals(what, expected, actual)
        }

        private fun string(node: JsonObject, name: String, path: String): String =
            node[name]?.jsonPrimitive?.content
                ?: throw AssertionError("$path.$name must be a string")

        private fun list(node: JsonObject, name: String, path: String): List<JsonElement> {
            val value = node[name] as? JsonArray
                ?: throw AssertionError("$path.$name must be a list")
            return value
        }

        private fun strings(node: JsonObject, name: String, path: String): List<String> =
            list(node, name, path).mapIndexed { i, value ->
                (value as? kotlinx.serialization.json.JsonPrimitive)?.content
                    ?: throw AssertionError("$path.$name[$i] must be a string")
            }
    }

    @Test
    fun transactionBodiesAndSigningHashesMatchTheIndependentGenerator() {
        var signed = 0
        for (case in cases("account-tx.json")) {
            if (field(case, "provenance") == "chain-sourced") {
                continue // no root to sign with; its own test follows
            }
            val name = field(case, "name")
            val wallet = EvmWallet.fromRoot(MigoRoot.fromBytes(hex(field(case, "root"))), case["index"]!!.jsonPrimitive.longOrNull!!.toInt())

            val tx = Eip1559Tx(
                chainId = bigInt(case, "chain_id").toLong(),
                nonce = bigInt(case, "nonce").toLong(),
                maxPriorityFeePerGas = bigInt(case, "max_priority_fee_per_gas"),
                maxFeePerGas = bigInt(case, "max_fee_per_gas"),
                gasLimit = bigInt(case, "gas_limit").toLong(),
                to = hex(field(case, "recipient")),
                value = bigInt(case, "value_wei"),
                data = hex(field(case, "data")),
            )
            assertHexBytes("body `$name`", field(case, "body_rlp"), tx.bodyRlp())
            assertHexBytes("digest `$name`", field(case, "signing_hash"), tx.signingHash())
            assertHexBytes("sender `$name`", field(case, "sender"), wallet.address())

            // The signature bytes are deliberately not pinned: each port signs with its own library
            // and nonce, and any valid low-s signature is the same transaction to the chain. Proving
            // validity is recovering the sender from the port's own raw transaction.
            val signedTx = tx.sign(wallet)
            assertHexBytes("recovered sender `$name`", hexOf(wallet.address()), recoverSender(signedTx.raw))
            // The raw transaction is a well-formed envelope on its way out, whatever broadcast does.
            val envelope = rlpDecode(signedTx.raw.copyOfRange(1, signedTx.raw.size))
            assertTrue("envelope `$name`", envelope is RlpItem.List && envelope.items.size == 12)
            signed += 1
        }
        assertTrue("account-tx.json carries no signable case", signed > 0)
    }

    @Test
    fun theChainSourcedTransactionRecoversToItsObservedSender() {
        // A real Avalanche C-Chain mainnet transaction: the sender and hash are what the chain
        // observed, so this is the one case that can catch a port that is self-consistent and
        // still wrong.
        val observed = cases("account-tx.json").filter { field(it, "provenance") == "chain-sourced" }
        assertEquals("account-tx.json must carry exactly one chain-sourced case", 1, observed.size)
        val case = observed[0]

        val raw = hex(field(case, "raw"))
        assertHexBytes(
            "recovered sender disagrees with the chain",
            field(case, "sender"),
            recoverSender(raw),
        )
        assertHexBytes(
            "keccak256(raw) disagrees with the chain",
            field(case, "tx_hash"),
            Keccak.digest256(raw),
        )

        // Decode the envelope strictly and rebuild the transaction: every observed field must match
        // the case's record, and the re-encoded body must be the body the signature was made over.
        val envelope = rlpDecode(raw.copyOfRange(1, raw.size))
        assertTrue("envelope must be a 12-item list", envelope is RlpItem.List && envelope.items.size == 12)
        val body = (envelope as RlpItem.List).items.subList(0, 9)
        fun bodyBytes(i: Int): ByteArray = (body[i] as RlpItem.String).bytes
        val tx = Eip1559Tx(
            chainId = rlpAsUint(body[0]).toLong(),
            nonce = rlpAsUint(body[1]).toLong(),
            maxPriorityFeePerGas = rlpAsUint(body[2]),
            maxFeePerGas = rlpAsUint(body[3]),
            gasLimit = rlpAsUint(body[4]).toLong(),
            to = bodyBytes(5),
            value = rlpAsUint(body[6]),
            data = bodyBytes(7),
        )
        assertHexBytes("re-encoded body differs", hexOf(rlpEncode(RlpItem.List(body))), tx.bodyRlp())
        assertEquals("chain id", bigInt(case, "chain_id").toLong(), tx.chainId)
        assertEquals("nonce", bigInt(case, "nonce").toLong(), tx.nonce)
        assertBig("priority fee", bigInt(case, "max_priority_fee_per_gas"), tx.maxPriorityFeePerGas)
        assertBig("max fee", bigInt(case, "max_fee_per_gas"), tx.maxFeePerGas)
        assertEquals("gas limit", bigInt(case, "gas_limit").toLong(), tx.gasLimit)
        assertHexBytes("recipient", field(case, "recipient"), tx.to)
        assertBig("value", bigInt(case, "value_wei"), tx.value)
        assertHexBytes("call data", field(case, "data"), tx.data)
    }

    @Test
    fun theRlpCodecIsStrictInBothDirections() {
        // The four shapes a tolerant decoder accepts and a canonical one must not, pinned from the
        // specification's own strictness rules — this parser reads bytes that arrived over a network.
        for ((bytes, why) in listOf(
            "8105" to "single byte below 0x80 must encode as itself",
            "b8012a" to "length written in long form for a short-form payload",
            "b9002a2a" to "length has a leading zero byte",
            "c001" to "trailing bytes after a complete item",
        )) {
            assertEquals("$bytes must be rejected", why, malformedRlpWhat { rlpDecode(hex(bytes)) })
        }

        // And canonical on the way in: the specification's appendix examples, plus the two integer
        // rules hand-rolled encoders get wrong — zero is the empty string, and 1024 needs two bytes.
        assertEquals("83646f67", hexOf(rlpEncode(RlpItem.String(hex("646f67")))))
        assertEquals(
            "c88363617483646f67",
            hexOf(rlpEncode(RlpItem.List(listOf(RlpItem.String(hex("636174")), RlpItem.String(hex("646f67")))))),
        )
        assertEquals("80", hexOf(rlpEncode(RlpItem.String(rlpUint(BigInteger.ZERO)))))
        assertEquals("820400", hexOf(rlpEncode(RlpItem.String(rlpUint(BigInteger.valueOf(1024))))))
        assertEquals("00", hexOf(rlpEncode(RlpItem.String(hex("00")))))
        assertEquals(
            "c2c0c0",
            hexOf(rlpEncode(RlpItem.List(listOf(RlpItem.List(emptyList()), RlpItem.List(emptyList()))))),
        )

        // Round trip: whatever the decoder accepts, the encoder hands back byte for byte — the
        // identity that makes "re-encode the body" a valid strictness check above.
        val raw = hex(field(cases("account-tx.json").first { field(it, "provenance") == "chain-sourced" }, "raw"))
        val envelope = raw.copyOfRange(1, raw.size)
        assertHexBytes("round trip", hexOf(envelope), rlpEncode(rlpDecode(envelope)))
    }

    @Test
    fun parseAddressAcceptsLowercaseAndChecksEip55OnMixedCase() {
        // The send flow's last line of defense before funds move: a typo in a checksummed recipient
        // must fail here, not on the chain.
        val withLetters = cases("account-evm.json").first {
            field(it, "address_checksummed").any { c -> c in 'a'..'f' || c in 'A'..'F' }
        }
        val checksummed = field(withLetters, "address_checksummed")
        val lowercase = checksummed.lowercase().removePrefix("0x")
        assertHexBytes("checksummed", lowercase, parseAddress(checksummed))
        assertHexBytes("lowercase", lowercase, parseAddress(lowercase))
        assertHexBytes("prefixed", lowercase, parseAddress("0x$lowercase"))

        // One flipped letter: still mixed case, still valid hex, and no longer the checksum the
        // EIP-55 casing encodes — the exact shape a pasted-address typo produces.
        val at = checksummed.indexOfFirst { it in 'a'..'f' || it in 'A'..'F' }
        val flipped = checksummed.substring(0, at) +
            (if (checksummed[at].isLowerCase()) checksummed[at].uppercaseChar() else checksummed[at].lowercaseChar()) +
            checksummed.substring(at + 1)
        assertNotEquals("the flip must change the string", checksummed, flipped)
        assertEquals(
            "a flipped checksum letter must fail the checksum",
            AccountErrorKind.AddressChecksumFailed,
            addressFailureOf(flipped),
        )
        assertEquals(
            "not hex at all",
            AccountErrorKind.BadAddress,
            addressFailureOf("not-an-address"),
        )
        assertEquals(
            "39 characters",
            AccountErrorKind.BadAddress,
            addressFailureOf(lowercase.substring(0, 39)),
        )
    }

    @Test
    fun eip712DigestsMatchTheIndependentGeneratorAndTheSpecificationExample() {
        val cases = cases("account-eip712.json")

        // The first case is the EIP-712 specification's own worked example, its expected values
        // pinned to the EIP's published digest. A port cannot pass that by agreeing with the
        // generator on a shared mistake, which is why its presence is asserted, not assumed.
        assertEquals("eip712-spec-example", field(cases[0], "name"))
        assertEquals("eip712-spec-example", field(cases[0], "provenance"))

        for (case in cases) {
            val name = field(case, "name")
            val domainNode = case["domain"]!!.jsonObject
            val domain = Eip712Domain(
                name = domainNode["name"]?.jsonPrimitive?.content,
                version = domainNode["version"]?.jsonPrimitive?.content,
                chainId = domainNode["chain_id"]?.jsonPrimitive?.longOrNull,
                verifyingContract = domainNode["verifying_contract"]?.jsonPrimitive?.content?.let { hex(it) },
                salt = domainNode["salt"]?.jsonPrimitive?.content?.let { hex(it) },
            )

            val message = case["message"]!!.jsonObject["struct"]!!.jsonObject
            val primary = string(message, "primary_type", name)
            val referenced = strings(message, "referenced_types", name)
            val values = list(message, "values", name).mapIndexed { i, child ->
                eip712Value(child.jsonObject, "$name[$i]")
            }

            // The encodeType appendix — referenced declarations, sorted by name — is the part of
            // EIP-712 every hand-rolled implementation gets wrong, so the string itself is pinned
            // before any hash.
            assertEquals("encodeType `$name`", field(case, "encode_type"), eip712EncodeType(primary, referenced))

            val expected = case["expected"]!!.jsonObject
            val typeHash = eip712TypeHash(primary, referenced)
            assertHexBytes("type hash `$name`", string(expected, "type_hash", name), typeHash)
            assertHexBytes("separator `$name`", string(expected, "domain_separator", name), domain.separator())
            val structHash = eip712HashStruct(typeHash, values)
            assertHexBytes("hashStruct `$name`", string(expected, "struct_hash", name), structHash)
            assertHexBytes(
                "digest `$name`",
                string(expected, "digest", name),
                eip712Digest(domain.separator(), structHash),
            )
        }
    }

    /**
     * Converts the vector file's recursive value model into this module's typed values. Structs
     * become their own hashStruct — the EIP-712 rule that makes the type recursive — and a struct
     * is recognized *before* its fields are read: an array node carries `values`, not `value`, and
     * the eager read is the bug this suite's Python, Rust and TypeScript siblings each hit once.
     */
    private fun eip712Value(node: JsonObject, path: String): Eip712Value {
        val struct = node["struct"]?.jsonObject
        if (struct != null) {
            val typeHash = eip712TypeHash(
                string(struct, "primary_type", path),
                strings(struct, "referenced_types", path),
            )
            val values = list(struct, "values", path).mapIndexed { i, child ->
                eip712Value(child.jsonObject, "$path[$i]")
            }
            return Eip712Value.Bytes32(eip712HashStruct(typeHash, values))
        }
        return when (val type = string(node, "type", path)) {
            "address" -> Eip712Value.Address(hex(string(node, "value", path)))
            "bytes32" -> Eip712Value.Bytes32(hex(string(node, "value", path)))
            "bytes" -> Eip712Value.Data(hex(string(node, "value", path)))
            // The file writes uint256 as hex (shorter than 32 bytes when the value is small); the
            // padding to 32 is the port's job.
            "uint256" -> Eip712Value.Uint256(BigInteger(string(node, "value", path), 16))
            "string" -> Eip712Value.Text(string(node, "value", path))
            "array" -> Eip712Value.ArrayValue(
                list(node, "values", path).mapIndexed { i, child ->
                    eip712Value(child.jsonObject, "$path[$i]")
                },
            )
            else -> throw AssertionError("$path: unknown EIP-712 value type `$type`")
        }
    }

    /** The `what` of the MalformedRlp a block throws — the strictness reason, pinned by the test. */
    private fun malformedRlpWhat(block: () -> Unit): String =
        try {
            block()
            throw AssertionError("the decoder must reject this input")
        } catch (error: AccountError) {
            if (error.kind != AccountErrorKind.MalformedRlp) {
                throw AssertionError("expected MalformedRlp, got ${error.kind}")
            }
            @Suppress("UNCHECKED_CAST")
            (error.detail["what"] as String)
        }

    /** The kind of the refusal [parseAddress] reports for this input. */
    private fun addressFailureOf(text: String): AccountErrorKind =
        try {
            parseAddress(text)
            throw AssertionError("parseAddress must refuse `$text`")
        } catch (error: AccountError) {
            error.kind
        }
}
