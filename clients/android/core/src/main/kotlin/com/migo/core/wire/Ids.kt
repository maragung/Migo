package com.migo.core.wire

import java.math.BigInteger

/**
 * Identifiers: 128 bits on the wire, 26 Crockford base32 characters in memory.
 *
 * The first six bytes are big-endian Unix milliseconds, so ids sort chronologically as both
 * bytes and text and a database index on them stays append-mostly. The alphabet excludes I,
 * L, O and U: the first three because they are misread as 1, 1 and 0 when a human copies an
 * id out of a support ticket, and U because excluding it keeps accidental profanity out of
 * generated identifiers.
 *
 * [Id] wraps a `String` in a zero-overhead value class — the Kotlin counterpart of the
 * TypeScript branded string. Two ids are equal when their text is equal, which is what makes
 * them safe as `Map`/`Set` keys.
 */
@JvmInline
value class Id(val value: String) {
    override fun toString(): String = value
}

/** Crockford base32, minus I, L, O and U. */
private const val ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

/** Characters in the text form. */
const val ID_TEXT_LEN = 26

/** Bytes in the wire form. */
const val ID_BYTE_LEN = 16

/** The all-zero id. Means "absent" in a required field, and is never generated. */
val NIL_ID = Id("0".repeat(ID_TEXT_LEN))

/**
 * Reverse lookup, built once. Lenient in the three ways the Rust and TypeScript decoders
 * are: lowercase is accepted, `I`/`l` read as `1`, and `O` reads as `0`. An id pasted from a
 * support ticket must either parse on every side or fail on every side; a validator stricter
 * in one client than the server produces "it works for them" bug reports.
 */
private val DECODE: Map<Char, Int> = buildMap {
    ALPHABET.forEachIndexed { index, ch ->
        put(ch, index)
        put(ch.lowercaseChar(), index)
    }
    for (ch in "IiLl") put(ch, 1)
    for (ch in "Oo") put(ch, 0)
}

private val MASK_5 = BigInteger.valueOf(0x1F)
private val MASK_8 = BigInteger.valueOf(0xFF)

/** What was wrong with a candidate identifier. */
sealed interface IdParseFailure {
    data class Length(val actual: Int) : IdParseFailure
    data class Character(val position: Int) : IdParseFailure
    data object Overflow : IdParseFailure
}

/** The outcome of [tryParseId]. */
sealed interface IdParseResult {
    data class Ok(val id: Id) : IdParseResult
    data class Fail(val why: IdParseFailure) : IdParseResult
}

/** Converts 16 wire bytes to an id. */
fun idFromBytes(bytes: ByteArray): Id {
    if (bytes.size != ID_BYTE_LEN) throw WireError.fieldOverflow("id")
    return render(BigInteger(1, bytes))
}

/** Converts an id to its 16 wire bytes. */
fun idToBytes(id: Id): ByteArray {
    var remaining = parseToBigInt(id.value)
    val out = ByteArray(ID_BYTE_LEN)
    for (i in ID_BYTE_LEN - 1 downTo 0) {
        out[i] = remaining.and(MASK_8).toInt().toByte()
        remaining = remaining.shiftRight(8)
    }
    return out
}

/**
 * Validates text as an id, returning the failure instead of throwing. The id that comes back
 * is canonical: uppercase, with Crockford's confusable characters folded, so `==` and hashing
 * stay correct.
 */
fun tryParseId(text: String): IdParseResult =
    when (val scanned = scan(text)) {
        is Scan.Ok -> IdParseResult.Ok(render(scanned.value))
        is Scan.Fail -> IdParseResult.Fail(scanned.why)
    }

/** Validates text as an id, throwing on anything malformed. */
fun parseId(text: String): Id =
    when (val result = tryParseId(text)) {
        is IdParseResult.Ok -> result.id
        // The failure reason describes the shape of the input, not its content, and an id is
        // not secret material — so it is safe to state.
        is IdParseResult.Fail -> throw IllegalArgumentException("not a Migo id (${describe(result.why)}): length ${text.length}")
    }

/**
 * True when [value] is a well-formed id in canonical form. A lenient spelling — lowercase, or
 * an `O` for a zero — is parseable but is not itself an id, because two spellings of one
 * identifier would not compare equal; run such a string through [parseId] instead.
 */
fun isId(value: String): Boolean =
    when (val result = tryParseId(value)) {
        is IdParseResult.Ok -> result.id.value == value
        is IdParseResult.Fail -> false
    }

/** Unix milliseconds from the id's time prefix. */
fun idUnixMs(id: Id): Long {
    val bytes = idToBytes(id)
    var ms = 0L
    for (i in 0 until 6) {
        ms = ms * 256 + (bytes[i].toInt() and 0xFF)
    }
    return ms
}

/** Renders 128 bits as the canonical 26-character text form. */
private fun render(n: BigInteger): Id {
    val sb = StringBuilder(ID_TEXT_LEN)
    for (i in 0 until ID_TEXT_LEN) {
        val shift = 125 - i * 5
        val index = n.shiftRight(shift).and(MASK_5).toInt()
        sb.append(ALPHABET[index])
    }
    return Id(sb.toString())
}

private sealed interface Scan {
    data class Ok(val value: BigInteger) : Scan
    data class Fail(val why: IdParseFailure) : Scan
}

private fun scan(text: String): Scan {
    if (text.length != ID_TEXT_LEN) return Scan.Fail(IdParseFailure.Length(text.length))
    var n = BigInteger.ZERO
    for (position in text.indices) {
        val value = DECODE[text[position]] ?: return Scan.Fail(IdParseFailure.Character(position))
        // 26 characters carry 130 bits but an id is 128, so the leading character may only use
        // its low three. Rejecting the rest keeps the text form injective.
        if (position == 0 && value > 7) return Scan.Fail(IdParseFailure.Overflow)
        n = n.shiftLeft(5).or(BigInteger.valueOf(value.toLong()))
    }
    return Scan.Ok(n)
}

private fun parseToBigInt(text: String): BigInteger =
    when (val scanned = scan(text)) {
        is Scan.Ok -> scanned.value
        is Scan.Fail -> throw IllegalArgumentException("not a Migo id (${describe(scanned.why)}): length ${text.length}")
    }

private fun describe(why: IdParseFailure): String = when (why) {
    is IdParseFailure.Length -> "must be $ID_TEXT_LEN characters, got ${why.actual}"
    is IdParseFailure.Character -> "invalid character at position ${why.position}"
    IdParseFailure.Overflow -> "leading character encodes bits above 128"
}
