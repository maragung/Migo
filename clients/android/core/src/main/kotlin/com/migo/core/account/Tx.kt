package com.migo.core.account

import com.migo.core.crypto.Keccak
import com.migo.core.crypto.hexOf
import java.math.BigInteger
import org.bouncycastle.crypto.digests.SHA256Digest
import org.bouncycastle.crypto.ec.CustomNamedCurves
import org.bouncycastle.crypto.params.ECDomainParameters
import org.bouncycastle.crypto.params.ECPrivateKeyParameters
import org.bouncycastle.crypto.signers.ECDSASigner
import org.bouncycastle.crypto.signers.HMacDSAKCalculator
import org.bouncycastle.math.ec.ECPoint

/** The secp256k1 parameters, shared with the BIP-32 walk in EvmWallet. */
private val SECP256K1 = CustomNamedCurves.getByName("secp256k1")
private val SECP256K1_N: BigInteger = SECP256K1.n
private val SECP256K1_G = SECP256K1.g
private val SECP256K1_PARAMS = ECDomainParameters(SECP256K1.curve, SECP256K1.g, SECP256K1.n)
private val HALF_N: BigInteger = SECP256K1_N.shiftRight(1)

/** The EIP-1559 type byte — first byte of the signing hash input and of the raw transaction. */
private const val EIP1559_TYPE = 0x02

/** The 20-byte address length. */
private const val ADDRESS_LEN = 20

// --- networks -----------------------------------------------------------------

/**
 * An EVM network this build can name: a chain id and a pinned RPC.
 *
 * The RPC URL is a documented constant, not a configuration knob, because the user picks a network
 * and never a URL — a self-supplied RPC is the classic way a wallet gets shown a fake chain.
 */
data class Network(val name: String, val chainId: Long, val rpcUrl: String)

/** Avalanche C-Chain mainnet: chain id 43114. */
val AVALANCHE_MAINNET = Network("Avalanche C-Chain", 43114, "https://api.avax.network/ext/bc/C/rpc")

/** Avalanche Fuji testnet: chain id 43113. The verification network — never mainnet. */
val FUJI_TESTNET = Network("Avalanche Fuji", 43113, "https://api.avax-test.network/ext/bc/C/rpc")

/**
 * Verifies an RPC-observed chain id against [network]. Called with the answer to `eth_chainId`
 * before a transaction is built; a mismatch is the chain-confusion case and must close the session.
 */
fun checkChainId(network: Network, observed: Long) {
    if (observed != network.chainId) {
        throw AccountError.chainMismatch(network.chainId, observed)
    }
}

// --- RLP ----------------------------------------------------------------------

/**
 * One RLP item: a byte string or a list of items. RLP has exactly these two shapes; the danger is
 * never the tree, it is the length-prefix rules, which live in one place below.
 */
sealed class RlpItem {
    /** A byte string. */
    class String(val bytes: ByteArray) : RlpItem()

    /** A list of items. */
    class List(val items: kotlin.collections.List<RlpItem>) : RlpItem()
}

/**
 * The RLP encoding of one item. Canonical: a one-byte string below 0x80 is the byte itself, zero
 * encodes as the empty string, and every length is minimal.
 */
fun rlpEncode(item: RlpItem): ByteArray {
    val out = ArrayList<Byte>(64)
    encodeInto(item, out)
    return out.toByteArray()
}

private fun encodeInto(item: RlpItem, out: MutableList<Byte>) {
    when (item) {
        is RlpItem.String -> {
            // The rule the decoder enforces and the encoder must honor, or their round trip is
            // not the identity.
            if (item.bytes.size == 1 && item.bytes[0].toInt() and 0xff < 0x80) {
                out.add(item.bytes[0])
                return
            }
            encodeLength(out, item.bytes.size, 0x80)
            out.addAll(item.bytes.toList())
        }
        is RlpItem.List -> {
            val payload = ArrayList<Byte>(64)
            for (child in item.items) {
                encodeInto(child, payload)
            }
            encodeLength(out, payload.size, 0xc0)
            out.addAll(payload)
        }
    }
}

private fun encodeLength(out: MutableList<Byte>, length: Int, offset: Int) {
    if (length <= 55) {
        out.add((offset + length).toByte())
        return
    }
    val bytes = ArrayList<Byte>(8)
    var value = length
    while (value > 0) {
        bytes.add((value and 0xff).toByte())
        value = value ushr 8
    }
    out.add((offset + 55 + bytes.size).toByte())
    for (i in bytes.indices.reversed()) {
        out.add(bytes[i])
    }
}

/**
 * Decodes exactly one RLP item from [bytes], rejecting trailing data. Strict on purpose: this
 * parses raw transactions that arrived over a network, where a tolerant decoder is a differential
 * oracle at best and a memory-exhaustion primitive at worst.
 */
fun rlpDecode(bytes: ByteArray): RlpItem {
    val decoded = decodeItem(bytes, 0)
    if (decoded.next != bytes.size) {
        throw AccountError.malformedRlp("trailing bytes after a complete item")
    }
    return decoded.item
}

/** One decoded item and the offset just past it. */
private class Decoded(val item: RlpItem, val next: Int)

private fun decodeItem(data: ByteArray, offset: Int): Decoded {
    if (offset >= data.size) {
        throw AccountError.malformedRlp("input ends where an item was expected")
    }
    val first = data[offset].toInt() and 0xff
    if (first < 0x80) {
        return Decoded(RlpItem.String(data.copyOfRange(offset, offset + 1)), offset + 1)
    }
    val length: Int
    val start: Int
    when {
        first <= 0xb7 -> {
            length = first - 0x80
            start = offset + 1
        }
        first <= 0xbf -> {
            val long = decodeLength(data, offset + 1, first - 0xb7)
            length = long.length
            start = long.start
        }
        first <= 0xf7 -> {
            length = first - 0xc0
            start = offset + 1
        }
        else -> {
            val long = decodeLength(data, offset + 1, first - 0xf7)
            length = long.length
            start = long.start
        }
    }
    val end = start + length
    if (end > data.size) {
        throw AccountError.malformedRlp("input ends inside an item")
    }
    if (first <= 0xbf) {
        // A single byte below 0x80 inside a string prefix was never written by a canonical encoder.
        if (length == 1 && data[start].toInt() and 0xff < 0x80) {
            throw AccountError.malformedRlp("single byte below 0x80 must encode as itself")
        }
        return Decoded(RlpItem.String(data.copyOfRange(start, end)), end)
    }
    val items = ArrayList<RlpItem>()
    var at = start
    while (at < end) {
        val child = decodeItem(data, at)
        items.add(child.item)
        at = child.next
    }
    return Decoded(RlpItem.List(items), end)
}

private class LongForm(val length: Int, val start: Int)

/** Reads a long-form length, refusing a leading zero byte and a short payload in long form. */
private fun decodeLength(data: ByteArray, at: Int, lengthOfLength: Int): LongForm {
    if (at + lengthOfLength > data.size) {
        throw AccountError.malformedRlp("input ends inside a length prefix")
    }
    if (data[at].toInt() == 0) {
        throw AccountError.malformedRlp("length has a leading zero byte")
    }
    var length = 0
    for (i in 0 until lengthOfLength) {
        length = (length shl 8) or (data[at + i].toInt() and 0xff)
    }
    if (length <= 55) {
        throw AccountError.malformedRlp("length written in long form for a short-form payload")
    }
    return LongForm(length, at + lengthOfLength)
}

/**
 * The minimal big-endian byte encoding of an integer; zero is the empty string — the one integer
 * rule most hand-rolled encoders get wrong.
 */
fun rlpUint(value: BigInteger): ByteArray {
    if (value.signum() == 0) return ByteArray(0)
    if (value.signum() < 0) throw AccountError.malformedRlp("negative integer")
    return value.toByteArray().let { raw ->
        // BigInteger.toByteArray is minimal and two's-complement signed: strip a leading zero
        // sign byte when the high bit is set, which is the only case it appears for a positive
        // value.
        if (raw.size > 1 && raw[0].toInt() == 0) raw.copyOfRange(1, raw.size) else raw
    }
}

/**
 * Reads an RLP byte string as the integer an encoder wrote with [rlpUint]: the empty string is
 * zero, and a zero-leading multi-byte form or a lone `0x00` is non-minimal and refused.
 */
fun rlpAsUint(item: RlpItem): BigInteger {
    val bytes = stringItem(item)
    if (bytes.isEmpty()) return BigInteger.ZERO
    if (bytes.size > 1 && bytes[0].toInt() == 0) {
        throw AccountError.malformedRlp("integer has a non-minimal (zero-leading) encoding")
    }
    if (bytes.size == 1 && bytes[0].toInt() == 0) {
        throw AccountError.malformedRlp("integer zero must encode as the empty string")
    }
    return BigInteger(1, bytes)
}

/** A body field that must be a byte string, not a list. */
private fun stringItem(item: RlpItem): ByteArray {
    if (item !is RlpItem.String) throw AccountError.notATransaction()
    return item.bytes
}

// --- the transaction ----------------------------------------------------------

/**
 * An EIP-1559 transaction body: everything the user confirmed, exactly as it will be signed.
 *
 * `data` is empty for a native AVAX transfer, and the access list is always empty in this build.
 * Wei fields are [BigInteger] by construction — a Long or Double at a call site is a silent
 * precision bug the type prevents.
 */
class Eip1559Tx(
    val chainId: Long,
    val nonce: Long,
    val maxPriorityFeePerGas: BigInteger,
    val maxFeePerGas: BigInteger,
    val gasLimit: Long,
    val to: ByteArray,
    val value: BigInteger,
    val data: ByteArray,
) {
    /** The nine signed fields as RLP items, with the always-empty access list. */
    private fun bodyItems(): List<RlpItem> = listOf(
        RlpItem.String(rlpUint(BigInteger.valueOf(chainId))),
        RlpItem.String(rlpUint(BigInteger.valueOf(nonce))),
        RlpItem.String(rlpUint(maxPriorityFeePerGas)),
        RlpItem.String(rlpUint(maxFeePerGas)),
        RlpItem.String(rlpUint(BigInteger.valueOf(gasLimit))),
        RlpItem.String(to),
        RlpItem.String(rlpUint(value)),
        RlpItem.String(data),
        RlpItem.List(emptyList()),
    )

    /** The nine signed fields as an RLP list. */
    fun bodyRlp(): ByteArray = rlpEncode(RlpItem.List(bodyItems()))

    /**
     * The hash signed: `Keccak-256(0x02 || RLP(fields))`. This — not the raw transaction, not the
     * receipt — is what the user's confirmation and the signature must agree on.
     */
    fun signingHash(): ByteArray =
        Keccak.digest256(byteArrayOf(EIP1559_TYPE.toByte()) + bodyRlp())

    /**
     * Signs this transaction with [wallet], returning the raw transaction ready for
     * `eth_sendRawTransaction`.
     *
     * The nonce is RFC 6979 deterministic and the signature is normalized to low-s; the parity bit
     * is recovered by matching the wallet's own public key, and flipping `s` to `n - s` mirrors
     * the recovered point, so the parity bit flips with it — the two must move together or
     * recovery lands on the wrong point.
     */
    fun sign(wallet: EvmWallet): SignedTx {
        val digest = signingHash()
        val signer = ECDSASigner(HMacDSAKCalculator(SHA256Digest()))
        signer.init(true, ECPrivateKeyParameters(BigInteger(1, wallet.privateKeyBytes()), SECP256K1_PARAMS))
        val signature = signer.generateSignature(digest)
        var r = signature[0]
        var s = signature[1]
        if (s > HALF_N) {
            s = SECP256K1_N.subtract(s)
        }

        // The recovery id is found by trying each parity against the wallet's public point: two
        // candidates cover every signature with r < n (all of them in practice), and the loop's
        // tail is the x-overflow pair, refused if it ever matched nothing.
        val publicPoint = SECP256K1_G.multiply(BigInteger(1, wallet.privateKeyBytes()).mod(SECP256K1_N)).normalize()
        var parity = -1
        for (candidate in intArrayOf(0, 1, 2, 3)) {
            val recovered = recoverPoint(digest, r, s, candidate) ?: continue
            if (recovered == publicPoint) {
                parity = candidate and 1
                break
            }
        }
        if (parity < 0) {
            throw AccountError.badSignature()
        }

        val envelope = rlpEncode(
            RlpItem.List(
                bodyItems() +
                    listOf(
                        RlpItem.String(rlpUint(BigInteger.valueOf(parity.toLong()))),
                        RlpItem.String(toBytes32(r)),
                        RlpItem.String(toBytes32(s)),
                    ),
            ),
        )
        val raw = byteArrayOf(EIP1559_TYPE.toByte()) + envelope
        return SignedTx(raw, Keccak.digest256(raw))
    }
}

/**
 * A signed transaction: the raw bytes and the hash the chain will know it by, computed once here
 * rather than re-derived by every consumer.
 */
class SignedTx(val raw: ByteArray, val txHash: ByteArray) {
    /** The hash as lowercase hex with a `0x` prefix, the form every RPC method takes. */
    fun txHashHex(): String = "0x" + hexOf(txHash)
}

/**
 * Recovers the sender's 20-byte address from a raw type-2 transaction: the mirror of
 * [Eip1559Tx.sign] and the proof the ports' signing paths agree. Signature bytes are deliberately
 * not pinned by the vectors — each port signs with its own library and proves validity by
 * recovering the sender from its own raw transaction.
 *
 * Strictness matches the other ports: a flat twelve-item envelope, chain id non-zero, parity 0 or
 * 1, `r` and `s` exactly 32 bytes, `s` at most n/2, and the body re-encoded exactly as it arrived.
 */
fun recoverSender(raw: ByteArray): ByteArray {
    if (raw.isEmpty() || raw[0].toInt() != EIP1559_TYPE) {
        throw AccountError.notATransaction()
    }
    val envelope = try {
        rlpDecode(raw.copyOfRange(1, raw.size))
    } catch (error: AccountError) {
        if (error.kind == AccountErrorKind.MalformedRlp) throw AccountError.notATransaction() else throw error
    }
    if (envelope !is RlpItem.List || envelope.items.size != 12) {
        throw AccountError.notATransaction()
    }
    val body = envelope.items.subList(0, 9)

    val chainId = try {
        rlpAsUint(body[0])
    } catch (error: AccountError) {
        if (error.kind == AccountErrorKind.MalformedRlp) throw AccountError.notATransaction() else throw error
    }
    if (chainId.signum() == 0 || chainId.bitLength() > 64) {
        throw AccountError.notATransaction()
    }

    // Re-encode the body exactly as it arrived and hash it: recovery must run over the bytes the
    // signature was made over, byte for byte.
    val bodyBytes = rlpEncode(RlpItem.List(body))
    val digest = Keccak.digest256(byteArrayOf(EIP1559_TYPE.toByte()) + bodyBytes)

    val parity = try {
        rlpAsUint(envelope.items[9])
    } catch (error: AccountError) {
        if (error.kind == AccountErrorKind.MalformedRlp) throw AccountError.notATransaction() else throw error
    }
    if (parity > BigInteger.ONE) {
        throw AccountError.badSignature()
    }
    val rBytes = stringItem(envelope.items[10])
    val sBytes = stringItem(envelope.items[11])
    if (rBytes.size != 32 || sBytes.size != 32) {
        throw AccountError.badSignature()
    }
    val s = BigInteger(1, sBytes)
    if (s > HALF_N) {
        throw AccountError.badSignature()
    }

    val point = recoverPoint(digest, BigInteger(1, rBytes), s, parity.toInt())
        ?: throw AccountError.badSignature()
    val uncompressed = point.getEncoded(false)
    return Keccak.digest256(uncompressed.copyOfRange(1, uncompressed.size)).copyOfRange(12, 32)
}

/**
 * Recovers the public point from a signature over [digest], or null if the recovery id does not
 * name a point on the curve. `u1 = -z/r`, `u2 = s/r`, `Q = u1·G + u2·R` — the same construction
 * the independent Python generator self-checks against the EIP-712 worked example.
 */
private fun recoverPoint(digest: ByteArray, r: BigInteger, s: BigInteger, recid: Int): ECPoint? {
    val x = r.add(if (recid >= 2) SECP256K1_N else BigInteger.ZERO)
    if (x >= SECP256K1.curve.field.characteristic) return null
    val prefix = if (recid and 1 == 1) 0x03 else 0x02
    val encoded = byteArrayOf(prefix.toByte()) + toBytes32(x)
    val rPoint = try {
        SECP256K1.curve.decodePoint(encoded).normalize()
    } catch (error: IllegalArgumentException) {
        return null
    }
    val z = BigInteger(1, digest)
    val rInv = r.modInverse(SECP256K1_N)
    val u1 = z.negate().multiply(rInv).mod(SECP256K1_N)
    val u2 = s.multiply(rInv).mod(SECP256K1_N)
    val q = SECP256K1_G.multiply(u1).add(rPoint.multiply(u2)).normalize()
    // The recovery is only meaningful if the point is on the curve the signature claims; decodePoint
    // already enforces that, so the sanity check below is a guard against a degenerate infinity.
    return if (q.isInfinity) null else q
}

// --- EIP-712 ------------------------------------------------------------------

/**
 * Builds an EIP-712 `encodeType` string: the primary struct's declaration, followed by every
 * referenced struct's declaration, sorted by name. The appendix is the part of EIP-712 every
 * hand-rolled implementation gets wrong — a struct that references other structs does not hash to
 * `keccak("Name(...)")` alone; the referenced declarations ride along.
 */
fun eip712EncodeType(primary: String, referenced: List<String>): String =
    primary + referenced.sorted().joinToString("")

/** `Keccak-256(encodeType)` for the primary struct being signed. */
fun eip712TypeHash(primary: String, referenced: List<String>): ByteArray =
    Keccak.digest256(eip712EncodeType(primary, referenced).toByteArray(Charsets.UTF_8))

/**
 * A typed value in the EIP-712 model this module hashes. Structs compose by hash: a struct field's
 * contribution to its parent's encoding is the child's own `hashStruct` output, supplied as a
 * [Eip712Value.Bytes32].
 */
sealed class Eip712Value {
    /** A 20-byte address. */
    class Address(val value: ByteArray) : Eip712Value()

    /** Exactly 32 bytes. */
    class Bytes32(val value: ByteArray) : Eip712Value()

    /** A 256-bit unsigned integer. */
    class Uint256(val value: BigInteger) : Eip712Value()

    /** A UTF-8 string, hashed. */
    class Text(val value: String) : Eip712Value()

    /** Arbitrary bytes, hashed. */
    class Data(val value: ByteArray) : Eip712Value()

    /** An array of values, each encoded and the concatenation hashed. */
    class ArrayValue(val values: List<Eip712Value>) : Eip712Value()
}

/** The 32-byte abi encoding of one typed value inside a hashStruct. */
fun eip712EncodeValue(value: Eip712Value): ByteArray = when (value) {
    is Eip712Value.Address -> {
        if (value.value.size != ADDRESS_LEN) throw AccountError.badLength("eip712 address", ADDRESS_LEN, value.value.size)
        ByteArray(32).also { System.arraycopy(value.value, 0, it, 12, ADDRESS_LEN) }
    }
    is Eip712Value.Bytes32 -> {
        if (value.value.size != 32) throw AccountError.badLength("eip712 bytes32", 32, value.value.size)
        value.value.copyOf()
    }
    is Eip712Value.Uint256 -> toBytes32(value.value)
    is Eip712Value.Text -> Keccak.digest256(value.value.toByteArray(Charsets.UTF_8))
    is Eip712Value.Data -> Keccak.digest256(value.value)
    is Eip712Value.ArrayValue -> {
        var concatenated = ByteArray(0)
        for (item in value.values) {
            concatenated += eip712EncodeValue(item)
        }
        Keccak.digest256(concatenated)
    }
}

/**
 * The EIP-712 domain of a signing request. Field presence matters: the domain separator's type
 * hash is built from exactly the fields that are set, in the EIP's fixed order, because a
 * separator computed over different fields than the dApp displayed is the primary EIP-712
 * phishing shape.
 */
class Eip712Domain(
    val name: String? = null,
    val version: String? = null,
    val chainId: Long? = null,
    val verifyingContract: ByteArray? = null,
    val salt: ByteArray? = null,
) {
    /** `Keccak-256("EIP712Domain(" + joined types + ")")` over exactly the present fields. */
    fun typeHash(): ByteArray {
        val types = ArrayList<String>()
        if (name != null) types.add("string name")
        if (version != null) types.add("string version")
        if (chainId != null) types.add("uint256 chainId")
        if (verifyingContract != null) types.add("address verifyingContract")
        if (salt != null) types.add("bytes32 salt")
        return Keccak.digest256("EIP712Domain(${types.joinToString(",")})".toByteArray(Charsets.UTF_8))
    }

    /** The domain separator: `Keccak-256(typeHash || encodeData(domain values))`. */
    fun separator(): ByteArray {
        var parts = typeHash()
        if (name != null) parts += eip712EncodeValue(Eip712Value.Text(name))
        if (version != null) parts += eip712EncodeValue(Eip712Value.Text(version))
        if (chainId != null) parts += eip712EncodeValue(Eip712Value.Uint256(BigInteger.valueOf(chainId)))
        if (verifyingContract != null) parts += eip712EncodeValue(Eip712Value.Address(verifyingContract))
        if (salt != null) parts += eip712EncodeValue(Eip712Value.Bytes32(salt))
        return Keccak.digest256(parts)
    }
}

/** `hashStruct`: `Keccak-256(typeHash || encodeData(values))`, the message half of the digest. */
fun eip712HashStruct(typeHash: ByteArray, values: List<Eip712Value>): ByteArray {
    var parts = typeHash
    for (value in values) {
        parts += eip712EncodeValue(value)
    }
    return Keccak.digest256(parts)
}

/** The final digest a wallet signs: `Keccak-256(0x1901 || domainSeparator || hashStruct)`. */
fun eip712Digest(domainSeparator: ByteArray, structHash: ByteArray): ByteArray =
    Keccak.digest256(byteArrayOf(0x19.toByte(), 0x01.toByte()) + domainSeparator + structHash)

// --- address input ------------------------------------------------------------

/**
 * Parses an address string for the send flow: `0x` optional, exactly 40 hex characters.
 * All-lowercase and all-uppercase are accepted as unchecked; mixed case is accepted only when its
 * EIP-55 checksum matches — a typo in a checksummed recipient is the last line of defense before
 * funds move, and it must fail here rather than on the chain.
 */
fun parseAddress(text: String): ByteArray {
    val stripped = if (text.startsWith("0x")) text.substring(2) else text
    if (stripped.length != 40 || !stripped.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) {
        throw AccountError.badAddress()
    }
    val hasLower = stripped.any { it in 'a'..'f' }
    val hasUpper = stripped.any { it in 'A'..'F' }
    val bytes = stripped.lowercase().hexBytes()
    // eip55() answers with the 0x prefix; the input may or may not carry one, so both sides of the
    // comparison are spelled with it.
    if (hasLower && hasUpper && eip55(bytes) != "0x$stripped") {
        throw AccountError.addressChecksumFailed()
    }
    return bytes
}

/** A non-negative scalar as exactly 32 big-endian bytes, left-padded. */
private fun toBytes32(value: BigInteger): ByteArray {
    if (value.signum() < 0) throw AccountError.badLength("eip712 uint256", 32, -1)
    val out = ByteArray(32)
    val raw = value.toByteArray()
    if (raw.size > 33 || (raw.size == 33 && raw[0].toInt() != 0)) {
        throw AccountError.badLength("eip712 uint256", 32, raw.size)
    }
    val magnitude = if (raw.isNotEmpty() && raw[0].toInt() == 0) raw.copyOfRange(1, raw.size) else raw
    if (magnitude.size > 32) {
        throw AccountError.badLength("eip712 uint256", 32, magnitude.size)
    }
    System.arraycopy(magnitude, 0, out, 32 - magnitude.size, magnitude.size)
    return out
}

/** Hex decode for a known-even-length, known-hex string. */
private fun String.hexBytes(): ByteArray =
    ByteArray(length / 2) { i -> substring(i * 2, i * 2 + 2).toInt(16).toByte() }
