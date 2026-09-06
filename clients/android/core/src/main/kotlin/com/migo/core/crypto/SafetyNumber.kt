package com.migo.core.crypto

import com.migo.core.wire.Id

/** Length of an identity fingerprint: the input [safetyNumber] groups for display. */
const val FINGERPRINT_LEN = 32

/**
 * The salt for the pair safety number. The same salt the contact fingerprint uses, so both live
 * in one KDF family; the label below is what keeps them from ever meaning the same thing.
 */
private val PAIR_SALT = "migo-fingerprint".toByteArray(Charsets.UTF_8)

/** The HKDF label for the pair safety number. */
private const val PAIR_LABEL = "migo-safety-number-v1"

/**
 * Groups a 32-byte identity fingerprint into the readable safety-number form: five-digit blocks
 * joined by single spaces.
 *
 * This is a line-for-line port of the desktop client's `model::safety_number`, because the whole
 * point of the form is that two people can read their numbers to each other aloud and compare —
 * which only works if every client renders the same fingerprint as the same string. Four bytes
 * become one big-endian `u32`, the value is taken modulo 100,000, and the remainder is padded to
 * five decimal digits: thirty-two bytes, eight blocks, forty digits.
 *
 * One discrepancy with the desktop source is worth recording: its doc comment claims "five-digit
 * blocks, twelve of them" and "sixty digits", while its code — and therefore every number it has
 * ever shown — produces eight blocks and forty digits. This port matches the code, not the
 * comment, because the code is what a person might already have read aloud off a desktop
 * machine, and a safety number that quietly changes between clients is worse than no safety
 * number at all.
 */
fun safetyNumber(fingerprint: ByteArray): String {
    if (fingerprint.size != FINGERPRINT_LEN) {
        throw CryptoError.badLength("fingerprint", FINGERPRINT_LEN, fingerprint.size)
    }
    val blocks = ArrayList<String>(FINGERPRINT_LEN / 4)
    var index = 0
    while (index < FINGERPRINT_LEN) {
        var value = 0L
        for (offset in 0 until 4) {
            value = (value shl 8) or (fingerprint[index + offset].toLong() and 0xff)
        }
        blocks.add(value.rem(100_000L).toString().padStart(5, '0'))
        index += 4
    }
    return blocks.joinToString(" ")
}

/**
 * The 32-byte fingerprint of a *pair* — this device's identity and one peer device's, in one
 * value neither side can influence alone.
 *
 * Each party's own fingerprint is fed in the same order on both sides — the two fingerprints are
 * sorted, so the number is symmetric: both people read the same string off their own screens,
 * which is the property that makes an aloud comparison meaningful. The order is the plain
 * lexicographic order of the fingerprint bytes, unsigned; sorting the *identity keys* themselves
 * would work too, but sorting the fingerprints keeps this function about bytes it was handed and
 * lets it stay agnostic of where they came from.
 *
 * # Android-first, and said so
 *
 * No other client derives a pair number: the desktop client renders only an account's *own*
 * fingerprint in its settings, and the web client renders nothing. So this label is defined here
 * first, and a safety number this build shows is not yet a number another client can be asked
 * for by name. The single-fingerprint [safetyNumber] above is the cross-client form; this pair
 * form is the Android definition until the others adopt it, and any adoption elsewhere must
 * reproduce the salt, the label, the sort, and the `own || peer` input order exactly.
 */
fun pairFingerprint(own: ByteArray, peer: ByteArray): ByteArray {
    if (own.size != FINGERPRINT_LEN) {
        throw CryptoError.badLength("fingerprint", FINGERPRINT_LEN, own.size)
    }
    if (peer.size != FINGERPRINT_LEN) {
        throw CryptoError.badLength("fingerprint", FINGERPRINT_LEN, peer.size)
    }
    val input = if (precedes(peer, own)) peer + own else own + peer
    return Kdf.derive(input, PAIR_SALT, PAIR_LABEL, FINGERPRINT_LEN)
}

/** The pair safety number: [pairFingerprint] rendered in the shared grouping. */
fun pairSafetyNumber(own: ByteArray, peer: ByteArray): String =
    safetyNumber(pairFingerprint(own, peer))

/**
 * One peer device's safety number, as a conversation's verification surface shows it.
 *
 * A Migo identity belongs to a *device*, not an account — a peer signed in on a phone and a
 * laptop publishes two of them — so a conversation's report is one of these per device, and
 * [changed] is per device: it says this device's fingerprint differs from the last one this
 * conversation observed, which is the moment brief section 164 says must not pass silently.
 */
class PeerSafetyNumber(
    /** The peer device the number was derived with. */
    val deviceId: Id,
    /** The number itself, already rendered. */
    val number: String,
    /** True when the device's identity changed since this conversation last saw it. */
    val changed: Boolean,
) {
    /** Public material only; safe to log. */
    override fun toString(): String =
        "PeerSafetyNumber(device_id: $deviceId, changed: $changed)"
}

/**
 * Whether [left] sorts before [right] by unsigned byte order, with equality resolving to false.
 *
 * Byte comparison in Kotlin is signed, and a fingerprint whose first differing byte is above
 * `0x7f` would sort the wrong way around half the time — which would not break the derivation
 * (both sides hold the same two values) but would break it *differently on the two sides* only if
 * the comparison itself disagreed, which is the one thing it must never do. Masking to unsigned
 * makes the order the order a person reading hex would expect.
 */
private fun precedes(left: ByteArray, right: ByteArray): Boolean {
    for (index in left.indices) {
        val lhs = left[index].toInt() and 0xff
        val rhs = right[index].toInt() and 0xff
        if (lhs != rhs) return lhs < rhs
    }
    return false
}
