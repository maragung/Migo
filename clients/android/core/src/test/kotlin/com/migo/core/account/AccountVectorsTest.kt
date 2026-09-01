package com.migo.core.account

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.crypto.Sodium
import com.migo.core.crypto.hexOf
import java.io.File
import kotlinx.serialization.json.Json
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
 * The account-root conformance vectors: the same four files the Rust reference, the desktop
 * client and the web client are tested against, read from `shared/protocol/vectors/crypto`.
 *
 * Two of the files were written by an independent Python implementation and two by the Rust
 * reference crate, and this suite is the Android half of the agreement: every seed, public key,
 * signature byte, address character and container byte this module produces is compared against
 * the file, not against a value this module itself computed. If BouncyCastle's ML-DSA disagreed
 * with FIPS 204 final, or the BIP-32 walk took a wrong step, or the JSON payload drifted one
 * byte from serde's compact form, a case here fails and names it.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] — the
 * same C code the device runs, loaded for the host JVM.
 */
class AccountVectorsTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
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

        private fun assertHexBytes(what: String, expected: String, actual: ByteArray) {
            assertEquals(what, expected, hexOf(actual))
        }
    }

    @Test
    fun domainsMatchTheVectors() {
        for (case in cases("account-domains.json")) {
            val root = MigoRoot.fromBytes(hex(field(case, "root")))
            assertHexBytes(
                "domain seed for ${field(case, "name")}",
                field(case, "seed"),
                root.domainSeed(field(case, "label")),
            )
            // The E2EE case carries the founding device's two sub-seeds — a second HKDF round
            // over the domain seed, which is what makes the account's E2EE history recoverable
            // from a container.
            case["e2ee_signing_seed"]?.let {
                val (signing, exchange) = foundingDeviceE2eeSeeds(root)
                assertHexBytes("e2ee signing seed", it.jsonPrimitive.content, signing)
                assertHexBytes("e2ee exchange seed", field(case, "e2ee_exchange_seed"), exchange)
            }
        }
    }

    @Test
    fun mldsaKeysAndSignaturesMatchTheVectors() {
        for (case in cases("account-mldsa.json")) {
            val seed = hex(field(case, "seed"))
            val payload = hex(field(case, "payload"))
            val context = field(case, "context")
            val (publicKey, signature) = when (context) {
                CONTEXT_LOGIN -> {
                    val key = IdentityKey.fromSeed(seed)
                    key.publicKey() to key.signLogin(payload)
                }
                CONTEXT_ROTATE -> {
                    val key = IdentityKey.fromSeed(seed)
                    key.publicKey() to key.signRotate(payload)
                }
                CONTEXT_LOGIN_DEVICE -> {
                    val credential = DeviceCredential.fromSeed(seed)
                    credential.publicKey() to credential.signLogin(payload)
                }
                else -> throw AssertionError("the vector names a context this build does not know: $context")
            }
            assertHexBytes("public key for ${field(case, "name")}", field(case, "public_key"), publicKey)
            assertHexBytes("signature for ${field(case, "name")}", field(case, "signature"), signature)

            // The pinned signature must verify under its own context, and only under it: the
            // context is mixed into the digest, which is the replay-between-purposes defence.
            verifyIdentity(hex(field(case, "public_key")), payload, context, signature)
            val wrongContext = if (context == CONTEXT_ROTATE) CONTEXT_LOGIN else CONTEXT_ROTATE
            var rejected = false
            try {
                verifyIdentity(hex(field(case, "public_key")), payload, wrongContext, signature)
            } catch (_: AccountError) {
                rejected = true
            }
            assertTrue("a signature under the wrong context must not verify", rejected)
        }
    }

    @Test
    fun evmDerivationMatchesTheVectors() {
        for (case in cases("account-evm.json")) {
            val name = field(case, "name")
            val index = case["index"]!!.jsonPrimitive.longOrNull!!.toInt()
            val wallet = EvmWallet.fromRoot(MigoRoot.fromBytes(hex(field(case, "root"))), index)
            assertHexBytes("private key for $name", field(case, "private_key"), wallet.privateKeyBytes())
            assertHexBytes("chain code for $name", field(case, "chain_code"), wallet.chainCodeBytes())
            assertHexBytes("address for $name", field(case, "address"), wallet.address())
            // The only form that should ever be shown to a user.
            assertEquals("checksummed address for $name", field(case, "address_checksummed"), wallet.addressChecksummed())
        }
    }

    @Test
    fun containersMatchTheVectorsByteForByte() {
        for (case in cases("account-container.json")) {
            val name = field(case, "name")
            val root = MigoRoot.fromBytes(hex(field(case, "root")))
            val file = AccountFile.new(root, case["created_at"]!!.jsonPrimitive.longOrNull!!)
            val params = ContainerParams(
                memoryKib = case["memory_kib"]!!.jsonPrimitive.longOrNull!!,
                timeCost = case["time_cost"]!!.jsonPrimitive.longOrNull!!,
                lanes = case["lanes"]!!.jsonPrimitive.longOrNull!!,
            )
            val sealed = sealContainerWith(
                field(case, "credential"),
                file,
                params,
                hex(field(case, "salt")),
                hex(field(case, "nonce")),
            )
            assertHexBytes("container bytes for $name", field(case, "container"), sealed)

            // And the round trip: the same bytes this port sealed must open back to the same
            // root — the restore path a new device actually takes.
            val opened = openContainer(field(case, "credential"), sealed)
            assertEquals("created_at survives the round trip for $name", file.createdAt, opened.createdAt)
            assertEquals("root survives the round trip for $name", root.asBytes().toList(), opened.root().asBytes().toList())
        }
    }

    @Test
    fun aWrongCredentialAndATamperedContainerFailIdentically() {
        val case = cases("account-container.json").first()
        val root = MigoRoot.fromBytes(hex(field(case, "root")))
        val file = AccountFile.new(root, case["created_at"]!!.jsonPrimitive.longOrNull!!)
        val sealed = sealContainerWith(
            field(case, "credential"),
            file,
            ContainerParams(
                memoryKib = case["memory_kib"]!!.jsonPrimitive.longOrNull!!,
                timeCost = case["time_cost"]!!.jsonPrimitive.longOrNull!!,
                lanes = case["lanes"]!!.jsonPrimitive.longOrNull!!,
            ),
            hex(field(case, "salt")),
            hex(field(case, "nonce")),
        )

        val wrong = openFailureOf("not the credential", sealed)
        val tampered = sealed.copyOf().also { it[HEADER_LEN + 5] = (it[HEADER_LEN + 5].toInt() xor 1).toByte() }
        val edited = openFailureOf(field(case, "credential"), tampered)
        assertEquals("the two failures an attacker would grind against must look the same", wrong, edited)
    }

    @Test
    fun aFileThatIsNotAContainerIsSaidSo() {
        val case = cases("account-container.json").first()
        val root = MigoRoot.fromBytes(hex(field(case, "root")))
        val file = AccountFile.new(root, case["created_at"]!!.jsonPrimitive.longOrNull!!)
        val sealed = sealContainerWith(
            field(case, "credential"),
            file,
            ContainerParams(
                memoryKib = case["memory_kib"]!!.jsonPrimitive.longOrNull!!,
                timeCost = case["time_cost"]!!.jsonPrimitive.longOrNull!!,
                lanes = case["lanes"]!!.jsonPrimitive.longOrNull!!,
            ),
            hex(field(case, "salt")),
            hex(field(case, "nonce")),
        )
        assertEquals(AccountErrorKind.NotAContainer, openFailureOf("credential", sealed.copyOf(HEADER_LEN - 1)))
        val wrongMagic = sealed.copyOf().also { it[0] = 'X'.code.toByte() }
        assertEquals(AccountErrorKind.NotAContainer, openFailureOf("credential", wrongMagic))
        // Future versions are a named refusal, not the wrong-credential one.
        val future = sealed.copyOf().also { it[9] = 0; it[10] = 2 }
        assertEquals(AccountErrorKind.UnsupportedVersion, openFailureOf("credential", future))
        val unknownKdf = sealed.copyOf().also { it[13] = 9 }
        assertEquals(AccountErrorKind.UnknownKdf, openFailureOf("credential", unknownKdf))
    }

    @Test
    fun freshRootsDeriveDistinctIdentitiesAndWallets() {
        val a = MigoRoot.generate()
        val b = MigoRoot.generate()
        assertNotEquals(IdentityKey.fromRoot(a).publicKey().toList(), IdentityKey.fromRoot(b).publicKey().toList())
        assertNotEquals(EvmWallet.fromRoot(a, 0).address().toList(), EvmWallet.fromRoot(b, 0).address().toList())
        assertNotEquals(
            EvmWallet.fromRoot(a, 0).address().toList(),
            EvmWallet.fromRoot(a, 1).address().toList(),
        )
        // The device credential is random, not derived: a leaked root cannot impersonate it.
        assertNotEquals(
            IdentityKey.fromRoot(a).publicKey().toList(),
            DeviceCredential.generate().publicKey().toList(),
        )
    }

    /** The kind of the failure [openContainer] reports for this credential and file. */
    private fun openFailureOf(credential: String, bytes: ByteArray): AccountErrorKind =
        try {
            openContainer(credential, bytes)
            // `fail` returns Unit on the JVM, which would widen this block's type past the
            // declared one; a throw is Nothing and keeps the expression a kind.
            throw AssertionError("the container must not open")
        } catch (e: AccountError) {
            e.kind
        }
}
