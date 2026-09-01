package com.migo.core.account

import com.migo.core.crypto.Aead
import com.migo.core.crypto.Argon2
import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.Csprng
import com.migo.core.crypto.Kdf
import com.migo.core.crypto.SymmetricKey
import com.migo.core.crypto.concatBytes
import com.migo.core.crypto.hexOf
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** The container magic. The trailing digit is the format generation. */
const val CONTAINER_MAGIC = "MIGOACCT1"

/** The format version this build writes and reads. */
const val FORMAT_VERSION = 1

/** The crypto version this build writes and reads. Bumped when the key schedule or AEAD changes. */
const val CRYPTO_VERSION = 1

/** Argon2id, the only KDF id this build understands. */
const val KDF_ARGON2ID = 1

/** Argon2id salt length in bytes. */
const val SALT_LEN = 16

/** The AEAD nonce length in bytes (XChaCha20-Poly1305). */
const val NONCE_LEN = 24

/** Total header length: magic, two versions, KDF id, three cost words, salt, nonce. */
const val HEADER_LEN = 66

/** Argon2id memory cost for new containers, in KiB: 64 MiB, matching the desktop vault. */
const val MEMORY_KIB = 64 * 1024

/** Argon2id passes for new containers. */
const val TIME_COST = 3

/** Argon2id lanes for new containers. */
const val LANES = 1

/** Shortest recovery credential accepted. Length is the rule; composition rules push people
 * towards dictionary words. */
const val MIN_CREDENTIAL_BYTES = 8

/** Longest recovery credential accepted, so a pasted file cannot turn one open into a minute of hashing. */
const val MAX_CREDENTIAL_BYTES = 1024

/**
 * The Argon2id parameters, read from a header or chosen for a new container.
 */
data class ContainerParams(
    /** Memory cost, KiB. */
    val memoryKib: Long,
    /** Time cost, passes. */
    val timeCost: Long,
    /** Lanes. */
    val lanes: Long,
) {
    companion object {
        /** The parameters new containers are sealed with. */
        val CURRENT = ContainerParams(memoryKib = MEMORY_KIB.toLong(), timeCost = TIME_COST.toLong(), lanes = LANES.toLong())
    }

    /**
     * Rejects parameters this build will not spend memory on.
     *
     * A stored cost is attacker-controlled in the sense that anyone who can write the file can set
     * it. The tag over the header already stops a silent downgrade, but a floor here means a
     * hostile container naming 4 GiB of Argon2 memory is refused before the allocation, not after
     * the process has been evicted.
     */
    fun validate(): ContainerParams {
        val sane = memoryKib in 8 * 1024..4 * 1024 * 1024 &&
            timeCost in 1..16 &&
            lanes in 1..16
        if (!sane) throw AccountError.kdfOutOfRange()
        return this
    }
}

/**
 * The decrypted container payload: everything a new device needs to become the account again.
 *
 * Deliberately small. The root is the account; metadata exists so a future reader can tell what
 * it is holding. Wallet addresses and device lists are NOT here — they are functions of the root
 * or live on the server, and duplicating them into the backup would create a second copy that
 * can drift from the first.
 *
 * The serialized form is a byte contract with the Rust reference: compact JSON, fields in this
 * order, no spaces — the same bytes serde_json emits — which is what lets a container sealed on
 * one platform open on another without a format negotiation. The vectors pin the exact bytes.
 */
@Serializable
data class AccountFile(
    /** The account payload format version. */
    val version: Int,
    /** When this container was sealed, Unix seconds. Display material, not security material. */
    @SerialName("created_at") val createdAt: Long,
    /** The root secret, hex-encoded: 64 characters. The only secret in the file. */
    val root: String,
) {
    companion object {
        /** Builds a payload for `root`, stamped `now` (Unix seconds). */
        fun new(root: MigoRoot, now: Long): AccountFile =
            AccountFile(version = FORMAT_VERSION, createdAt = now, root = hexOf(root.asBytes()))
    }

    /** The root secret. */
    fun root(): MigoRoot {
        val decoded = unhex(root) ?: throw AccountError.badLength("container root", MigoRoot.LEN, root.length / 2)
        return MigoRoot.fromBytes(decoded)
    }
}

/**
 * The exact JSON codec for the container payload.
 *
 * No configuration beyond ignoring unknown keys on read (the AEAD tag has already authenticated
 * the bytes; a field this build does not know is a future build's, not an attacker's). Anything
 * else — pretty printing, default skipping — would change the sealed bytes and break the
 * cross-port contract.
 */
private val containerJson = Json { ignoreUnknownKeys = true }

/**
 * Seals an account into container bytes with a fresh salt and nonce.
 *
 * A container copied to two clouds cannot be correlated by bytes, and an attacker cannot learn
 * anything from comparing two backups of the same account.
 */
fun sealContainer(credential: String, file: AccountFile): ByteArray =
    sealContainerWith(credential, file, ContainerParams.CURRENT, Csprng.bytes(SALT_LEN), Csprng.bytes(NONCE_LEN))

/**
 * Seals with caller-supplied salt and nonce: the deterministic form the conformance vectors use.
 */
fun sealContainerWith(
    credential: String,
    file: AccountFile,
    params: ContainerParams,
    salt: ByteArray,
    nonce: ByteArray,
): ByteArray {
    checkCredential(credential)
    params.validate()
    if (salt.size != SALT_LEN) throw AccountError.badLength("container salt", SALT_LEN, salt.size)
    if (nonce.size != NONCE_LEN) throw AccountError.badLength("container nonce", NONCE_LEN, nonce.size)

    val header = concatBytes(
        CONTAINER_MAGIC.toByteArray(Charsets.US_ASCII),
        u16be(FORMAT_VERSION),
        u16be(CRYPTO_VERSION),
        byteArrayOf(KDF_ARGON2ID.toByte()),
        u32be(params.memoryKib),
        u32be(params.timeCost),
        u32be(params.lanes),
        salt,
        nonce,
    )
    check(header.size == HEADER_LEN) { "the header layout drifted from HEADER_LEN" }

    val key = containerKey(credential, salt, params)
    val plaintext = containerJson.encodeToString(AccountFile.serializer(), file).toByteArray(Charsets.UTF_8)
    // sealWithNonce returns nonce || ciphertext || tag, which is the body this format stores:
    // readers hand the whole body to Aead.open.
    val body = Aead.sealWithNonce(key, nonce, header, plaintext)
    plaintext.fill(0)
    key.destroy()
    return concatBytes(header, body)
}

/**
 * Opens a container: verifies the header, derives the key, decrypts, returns the account.
 *
 * The only distinct errors are the ones that name a remedy; a wrong credential, a tampered byte,
 * and a truncated file are all [AccountErrorKind.OpenFailed] — the reader cannot distinguish
 * them, so it must not tell the caller which happened.
 */
fun openContainer(credential: String, bytes: ByteArray): AccountFile {
    checkCredential(credential)
    if (bytes.size < HEADER_LEN) throw AccountError.notAContainer()
    if (!String(bytes, 0, CONTAINER_MAGIC.length, Charsets.US_ASCII).equals(CONTAINER_MAGIC)) {
        throw AccountError.notAContainer()
    }
    val formatVersion = readU16(bytes, 9)
    val cryptoVersion = readU16(bytes, 11)
    if (formatVersion != FORMAT_VERSION || cryptoVersion != CRYPTO_VERSION) {
        // A container from a future build: refuse rather than guess at what its fields mean.
        throw AccountError.unsupportedVersion(maxOf(formatVersion, cryptoVersion), FORMAT_VERSION)
    }
    val kdfId = bytes[13].toInt() and 0xff
    if (kdfId != KDF_ARGON2ID) throw AccountError.unknownKdf(kdfId)
    val params = ContainerParams(
        memoryKib = readU32(bytes, 14),
        timeCost = readU32(bytes, 18),
        lanes = readU32(bytes, 22),
    ).validate()
    val salt = bytes.copyOfRange(26, 26 + SALT_LEN)
    // The header's nonce is advisory for readers that parse it field by field; the body carries
    // the authoritative copy as its prefix, and the two must agree or the tag fails — swapping a
    // header between files is the attack that arrangement closes.
    val header = bytes.copyOfRange(0, HEADER_LEN)
    val body = bytes.copyOfRange(HEADER_LEN, bytes.size)

    val key = containerKey(credential, salt, params)
    val file = try {
        val plaintext = Aead.open(key, header, body)
        val parsed = try {
            containerJson.decodeFromString(AccountFile.serializer(), plaintext.toString(Charsets.UTF_8))
        } catch (_: Exception) {
            throw AccountError.openFailed()
        }
        plaintext.fill(0)
        parsed
    } catch (_: CryptoError) {
        throw AccountError.openFailed()
    } finally {
        key.destroy()
    }
    file.root()
    return file
}

/** Argon2id at the header's parameters, then HKDF under the backup domain label. */
private fun containerKey(credential: String, salt: ByteArray, params: ContainerParams): SymmetricKey {
    val stretched = Argon2.derive(
        credential.toByteArray(Charsets.UTF_8),
        salt,
        params.memoryKib.toInt(),
        params.timeCost.toInt(),
        params.lanes.toInt(),
        32,
    )
    val derived = Kdf.derive(stretched, null, AccountDomains.BACKUP, 32)
    stretched.fill(0)
    return SymmetricKey.fromBytes(derived)
}

private fun checkCredential(credential: String) {
    val length = credential.toByteArray(Charsets.UTF_8).size
    if (length !in MIN_CREDENTIAL_BYTES..MAX_CREDENTIAL_BYTES) {
        throw AccountError.badLength("recovery credential", MIN_CREDENTIAL_BYTES, length)
    }
}

private fun u16be(value: Int): ByteArray = byteArrayOf((value ushr 8).toByte(), value.toByte())

private fun u32be(value: Long): ByteArray = byteArrayOf(
    (value ushr 24).toByte(),
    (value ushr 16).toByte(),
    (value ushr 8).toByte(),
    value.toByte(),
)

private fun readU16(bytes: ByteArray, offset: Int): Int =
    ((bytes[offset].toInt() and 0xff) shl 8) or (bytes[offset + 1].toInt() and 0xff)

private fun readU32(bytes: ByteArray, offset: Int): Long =
    ((bytes[offset].toInt() and 0xff).toLong() shl 24) or
        ((bytes[offset + 1].toInt() and 0xff).toLong() shl 16) or
        ((bytes[offset + 2].toInt() and 0xff).toLong() shl 8) or
        (bytes[offset + 3].toInt() and 0xff).toLong()

/**
 * Hex decode that rejects odd lengths and non-hex characters — a container payload is either
 * exactly right or it is not an account.
 */
private fun unhex(text: String): ByteArray? {
    if (text.length % 2 != 0) return null
    val out = ByteArray(text.length / 2)
    for (i in out.indices) {
        val high = Character.digit(text[i * 2], 16)
        val low = Character.digit(text[i * 2 + 1], 16)
        if (high < 0 || low < 0) return null
        out[i] = ((high shl 4) or low).toByte()
    }
    return out
}
