package com.migo.core.account

import com.migo.core.crypto.Keccak
import com.migo.core.crypto.hexOf
import java.math.BigInteger
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec
import org.bouncycastle.crypto.ec.CustomNamedCurves
import org.bouncycastle.math.ec.ECPoint

/** BIP-44 coin type for Ethereum and the EVM family. */
const val EIP155_COIN_TYPE = 60

/** The account path this build derives, as documentation: the code walks the levels. */
const val EVM_BIP44_PATH = "m/44'/60'/0'/0"

/** The BIP-32 hardened-bit. */
private const val BIP32_HARDENED = 0x8000_0000L

/** secp256k1, as BouncyCastle's named curve: the group, the order, and the generator. */
private val SECP256K1 = CustomNamedCurves.getByName("secp256k1")

/** The secp256k1 group order `n`. */
private val SECP256K1_N: BigInteger = SECP256K1.n

/** The secp256k1 generator. */
private val SECP256K1_G: ECPoint = SECP256K1.g

/**
 * A derived EVM wallet: one BIP-44 account of the root's EVM domain.
 *
 * The wallet is the one domain where an established hierarchical standard exists, so this is
 * that standard rather than a Migo-shaped cousin of it: the `MIGO/EVM/V1` domain seed becomes a
 * BIP-32 master seed, accounts are `m/44'/60'/0'/0/i`, the curve is secp256k1, and the address
 * is the last 20 bytes of Keccak-256 over the 64-byte uncompressed public key, checksummed per
 * EIP-55. A wallet recovered from a container is the address any standards-conformant Ethereum
 * tool derives from the same seed — the conformance vectors, written by an independent Python
 * implementation, are what prove it.
 *
 * The private key never leaves the device. [toString] renders the checksummed address, useful in
 * a log line without being dangerous.
 */
class EvmWallet private constructor(
    private val privateKey: BigInteger,
    val chainCode: ByteArray,
    private val addressBytes: ByteArray,
) {
    companion object {
        /** Derives wallet [index] of the root's EVM domain. */
        fun fromRoot(root: MigoRoot, index: Int): EvmWallet =
            derive(root.domainSeed(AccountDomains.EVM), index)

        /**
         * Derives wallet [index] from an explicit EVM domain seed — the form the conformance
         * vectors and a container restore use.
         */
        fun derive(domainSeed: ByteArray, index: Int): EvmWallet {
            // BIP-32 master key generation: I = HMAC-SHA512(key = "Bitcoin seed", data = seed).
            // The label is the standard's, not a Migo one — that is the point of this domain.
            val master = hmacSha512("Bitcoin seed".toByteArray(Charsets.UTF_8), domainSeed)
            var secret = BigInteger(1, master.copyOfRange(0, 32))
            var code = master.copyOfRange(32, 64)

            // m/44'/60'/0'/0/i, walked level by level exactly as BIP-44 prescribes for coin
            // type 60: three hardened levels, the change level 0, and the requested index.
            val levels = longArrayOf(
                44L + BIP32_HARDENED,
                EIP155_COIN_TYPE.toLong() + BIP32_HARDENED,
                BIP32_HARDENED,
                0L,
                index.toLong(),
            )
            for (level in levels) {
                val (childSecret, childCode) = ckdPriv(secret, code, level)
                secret = childSecret
                code = childCode
            }
            if (secret.signum() == 0 || secret >= SECP256K1_N) {
                throw AccountError.invalidDerivation()
            }
            return EvmWallet(secret, code, addressOf(secret))
        }
    }

    /** The 20-byte address. */
    fun address(): ByteArray = addressBytes.copyOf()

    /**
     * The EIP-55 checksummed address, the only form that should ever be shown to a user — a
     * mistyped checksummed address is rejected by every tool that receives it.
     */
    fun addressChecksummed(): String = eip55(addressBytes)

    /** The BIP-32 chain code after the full path, for container metadata. */
    fun chainCodeBytes(): ByteArray = chainCode.copyOf()

    /**
     * The private key bytes, for signing inside the device. The only accessor that exposes
     * secret material; whatever consumes this wallet next is a local operation by definition.
     */
    fun privateKeyBytes(): ByteArray = toBytes32(privateKey)

    override fun toString(): String = "EvmWallet(${eip55(addressBytes)})"
}

/**
 * One BIP-32 CKDpriv step.
 *
 * Hardened levels hash the parent secret; non-hardened levels hash the parent's compressed
 * public key. The distinction is the whole privacy property of BIP-32, so it is decided here, in
 * one place, from the index bit rather than by a caller remembering to pick a function.
 */
private fun ckdPriv(parentSecret: BigInteger, parentCode: ByteArray, index: Long): Pair<BigInteger, ByteArray> {
    val mac = Mac.getInstance("HmacSHA512")
    mac.init(SecretKeySpec(parentCode, "HmacSHA512"))
    if (index >= BIP32_HARDENED) {
        mac.update(0)
        mac.update(toBytes32(parentSecret))
    } else {
        // Compressed serialization, 33 bytes: the standard digest input for a non-hardened step.
        mac.update(compressedPublicKey(parentSecret))
    }
    mac.update(
        byteArrayOf(
            (index ushr 24).toByte(),
            (index ushr 16).toByte(),
            (index ushr 8).toByte(),
            index.toByte(),
        ),
    )
    val digest = mac.doFinal()

    // parse256(IL): a value at or above the curve order invalidates the step.
    val tweak = BigInteger(1, digest.copyOfRange(0, 32))
    if (tweak >= SECP256K1_N) {
        throw AccountError.invalidDerivation()
    }
    // kchild = IL + kpar (mod n); a zero result is the other invalid case.
    val child = parentSecret.add(tweak).mod(SECP256K1_N)
    if (child.signum() == 0) {
        throw AccountError.invalidDerivation()
    }
    return Pair(child, digest.copyOfRange(32, 64))
}

/**
 * The compressed secp256k1 public key of a secret scalar.
 *
 * The point is computed through the curve's own multiply rather than by importing a public-key
 * type: BIP-32 needs nothing but the encoded bytes and the scalar arithmetic, and going through
 * key classes would add a format layer this file has no use for. `ECPoint` here is
 * BouncyCastle's, which normalizes its own coordinates.
 */
private fun compressedPublicKey(secret: BigInteger): ByteArray {
    val point = SECP256K1_G.multiply(secret.mod(SECP256K1_N)).normalize()
    return point.getEncoded(true)
}

/** The 20-byte Ethereum address of a secret scalar: Keccak-256 of X || Y, last 20 bytes. */
private fun addressOf(secret: BigInteger): ByteArray {
    val point = SECP256K1_G.multiply(secret.mod(SECP256K1_N)).normalize()
    // 65 bytes with the 0x04 prefix; the digest input is the 64 bytes after it — including the
    // prefix is the classic way to derive a valid-looking wrong address.
    val uncompressed = point.getEncoded(false)
    val digest = Keccak.digest256(uncompressed.copyOfRange(1, uncompressed.size))
    return digest.copyOfRange(12, 32)
}

/** Renders a 20-byte address in EIP-55 form. */
fun eip55(address: ByteArray): String {
    val lowercase = hexOf(address)
    val digest = Keccak.digest256(lowercase.toByteArray(Charsets.US_ASCII))
    val out = StringBuilder(42)
    out.append("0x")
    for (i in lowercase.indices) {
        // Digits are never cased; letters follow the digest nibble. EIP-55 indexes the digest by
        // the hex character position, which for a 40-character string is the nibble index.
        val nibble = if (i % 2 == 0) digest[i / 2].toInt() ushr 4 else digest[i / 2].toInt() and 0x0f
        val ch = lowercase[i]
        out.append(if (ch in '0'..'9' || nibble < 8) ch else ch.uppercaseChar())
    }
    return out.toString()
}

/** HMAC-SHA512 in one call, the only hash BIP-32 defines. */
private fun hmacSha512(key: ByteArray, data: ByteArray): ByteArray {
    val mac = Mac.getInstance("HmacSHA512")
    mac.init(SecretKeySpec(key, "HmacSHA512"))
    return mac.doFinal(data)
}

/** A non-negative scalar as exactly 32 big-endian bytes, left-padded. */
private fun toBytes32(value: BigInteger): ByteArray {
    val raw = value.toByteArray()
    if (raw.size == 32) return raw.copyOf()
    val out = ByteArray(32)
    if (raw.size < 32) {
        System.arraycopy(raw, 0, out, 32 - raw.size, raw.size)
    } else {
        // A 33-byte signed encoding with a zero sign byte; the value is under n < 2^256, so this
        // strips the sign byte without losing magnitude.
        System.arraycopy(raw, raw.size - 32, out, 0, 32)
    }
    return out
}
