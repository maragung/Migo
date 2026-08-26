package com.migo.core.crypto

import com.migo.core.wire.ByteAccumulator
import com.migo.core.wire.Varint
import com.migo.core.wire.WireError

/**
 * The bytes that go in the opaque `envelope` field of a `MESSAGE_SEND` (brief section 11).
 *
 * The server never reads this. It sees a byte string of some length, routes it, and stores it. Both
 * ends encode it identically, which is the whole point: a message sealed by this client opens on
 * the web client and on the desktop client, and the reverse holds. This mirrors the references
 * `clients/desktop/src/crypto/envelope.rs` and `packages/sdk/src/session-crypto.ts`, field for
 * field.
 *
 * ```text
 * u8      envelope_version       always ENVELOPE_VERSION
 * u8      scheme                 decides which fields follow
 * varint  sender_key_id          0 for 1:1; the field exists for the group layout
 * -- X3DH preamble, present only for SCHEME_DOUBLE_RATCHET_PREKEY --
 * 64      initiator_identity     IdentityPublic.toBytes(); lets the responder run X3DH
 * 32      ephemeral_key          the initiator's X3DH ephemeral public key
 * varint  signed_prekey_id       which of the responder's signed prekeys was used
 * u8      has_one_time_prekey    1 if a one-time prekey was used, else 0
 * varint  one_time_prekey_id     present only when has_one_time_prekey is 1
 * -- Double Ratchet header and body --
 * 32      ratchet_public_key     the sender's current ratchet public key
 * varint  message_counter        index within the sender's current chain
 * varint  previous_chain_length  messages the sender sent in its previous chain
 * bytes   ciphertext             to the end; the trailing 16 bytes are the AEAD tag
 * ```
 *
 * # No field names, and no JSON
 *
 * Section 11 forbids JSON inside the envelope. Field names would cost bytes on every message and
 * leak structure through length, and the layout is fixed on both ends anyway, so there is nothing
 * for a name to disambiguate. Everything is positional; the `scheme` byte is what varies the shape.
 *
 * # A separate scheme rather than a flag for the first message
 *
 * [SCHEME_DOUBLE_RATCHET_PREKEY] changes which fields are *present*, not just how one is
 * interpreted. That is what a scheme is for; a boolean flag whose value silently adds a hundred
 * bytes to the layout is how parsers end up disagreeing about where the ciphertext starts.
 *
 * # Where the associated data is not
 *
 * There is deliberately no accessor here for AEAD associated data. The reference `envelope.rs`
 * exposes none, and the tag does not authenticate these header bytes: the ratchet layer binds both
 * device identities (the X3DH associated data) and the ratchet header's own canonical encoding,
 * [RatchetHeader.toBytes], which is a different byte string from this envelope's varint header.
 * Exposing a "header prefix" here would invite a caller to authenticate the wrong bytes.
 */

/** The only envelope version this build writes, and the only one it reads. */
const val ENVELOPE_VERSION = 1

/** An established 1:1 Double Ratchet message -- no X3DH preamble. */
const val SCHEME_DOUBLE_RATCHET = 1

/** A 1:1 first message: the same ratchet body, preceded by the X3DH material the peer needs. */
const val SCHEME_DOUBLE_RATCHET_PREKEY = 2

/** A group (sender-key) message. Belongs to the group layer, not this one. */
const val SCHEME_SENDER_KEY = 3

/**
 * The X3DH material a first message carries so the responder can derive the same secret.
 *
 * All of it is public. The responder needs the identity and the ephemeral key to reconstruct the
 * Diffie-Hellman outputs, and the prekey ids so it knows which of its own keys to use. Modelled on
 * [InitialMessage]: the ephemeral key is validated on construction and handed back only as a copy.
 */
class Preamble(
    /** The initiator's long-term identity. */
    val identity: IdentityPublic,
    ephemeralKey: ByteArray,
    /** Which of the responder's signed prekeys was used. */
    val signedPrekeyId: Long,
    /** Which one-time prekey was used, or null when the bundle had none left. */
    val oneTimePrekeyId: Long?,
) {
    private val ephemeralBytes: ByteArray

    init {
        requireU32(signedPrekeyId, "signed prekey id")
        oneTimePrekeyId?.let { requireU32(it, "one-time prekey id") }
        if (ephemeralKey.size != PUBLIC_KEY_LEN) {
            throw CryptoError.badLength("ephemeral key", PUBLIC_KEY_LEN, ephemeralKey.size)
        }
        ephemeralBytes = ephemeralKey.copyOf()
    }

    /** The initiator's ephemeral public key, returned as a copy so a holder cannot rewrite it. */
    val ephemeralKey: ByteArray get() = ephemeralBytes.copyOf()

    /** Public fields only; contains no secret material. */
    override fun toString(): String =
        "Preamble(identity: $identity, ephemeral_key: ${hexOf(ephemeralBytes)}, " +
            "signed_prekey_id: $signedPrekeyId, one_time_prekey_id: ${oneTimePrekeyId ?: "none"})"
}

/**
 * A parsed or about-to-be-written envelope.
 *
 * The constructor is positional and mirrors the reference struct's public fields, so [decode] can
 * build any shape it parses. [established] and [initial] are the two constructors a sender actually
 * uses, and they keep [scheme] and [preamble] consistent so [encode] never has to reject its own
 * output. Like the ratchet's [RatchetMessage], the [ciphertext] is held directly rather than
 * copied: it is the sealed AEAD output, not key material.
 */
class Envelope(
    /** Which of the `SCHEME_*` constants this is. */
    val scheme: Int,
    /** `0` for 1:1. Named in the layout because the group layout needs it in the same position. */
    val senderKeyId: Long,
    /** Present exactly when [scheme] is [SCHEME_DOUBLE_RATCHET_PREKEY]. */
    val preamble: Preamble?,
    /** The ratchet header: the sender's ratchet key, counter, and previous chain length. */
    val header: RatchetHeader,
    /** The AEAD output, tag included. */
    val ciphertext: ByteArray,
) {
    init {
        requireU32(senderKeyId, "sender key id")
    }

    /**
     * Serialises the envelope.
     *
     * Rejects the two states the sender-facing constructors cannot produce but the positional
     * constructor can: a prekey scheme without a preamble, and an established scheme with one. Both
     * are [CryptoError.malformedHeader], because a mismatched scheme and body is exactly the
     * kind of header confusion the scheme byte exists to prevent.
     */
    fun encode(): ByteArray {
        if (scheme == SCHEME_DOUBLE_RATCHET_PREKEY && preamble == null) {
            throw CryptoError.malformedHeader()
        }
        if (scheme == SCHEME_DOUBLE_RATCHET && preamble != null) {
            throw CryptoError.malformedHeader()
        }

        val out = ByteAccumulator(
            2 + 5 + IDENTITY_PUBLIC_LEN + PUBLIC_KEY_LEN + 16 +
                RatchetHeader.ENCODED_LEN + ciphertext.size,
        )
        out.push(ENVELOPE_VERSION)
        out.push(scheme)
        Varint.encodeU64(senderKeyId, out)

        preamble?.let { p ->
            out.append(p.identity.toBytes())
            out.append(p.ephemeralKey)
            Varint.encodeU64(p.signedPrekeyId, out)
            val otp = p.oneTimePrekeyId
            if (otp != null) {
                out.push(1)
                Varint.encodeU64(otp, out)
            } else {
                out.push(0)
            }
        }

        out.append(header.ratchetKey)
        Varint.encodeU64(header.messageNumber, out)
        Varint.encodeU64(header.previousChainLength, out)
        out.append(ciphertext)
        return out.toByteArray()
    }

    /** Public fields only; the ciphertext is shown as a length, never as bytes. */
    override fun toString(): String =
        "Envelope(scheme: $scheme, sender_key_id: $senderKeyId, " +
            "preamble: ${preamble ?: "none"}, header: $header, " +
            "ciphertext_len: ${ciphertext.size})"

    companion object {
        /** An envelope for a message in an established session -- no X3DH preamble. */
        fun established(header: RatchetHeader, ciphertext: ByteArray): Envelope =
            Envelope(SCHEME_DOUBLE_RATCHET, 0L, null, header, ciphertext)

        /** An envelope for the first message of a session, carrying the X3DH preamble. */
        fun initial(preamble: Preamble, header: RatchetHeader, ciphertext: ByteArray): Envelope =
            Envelope(SCHEME_DOUBLE_RATCHET_PREKEY, 0L, preamble, header, ciphertext)

        /**
         * Parses an envelope from attacker-supplied bytes.
         *
         * Every failure is [CryptoError.malformedHeader]. The reference answers every envelope
         * problem with one error reason, and these bytes end up in logs, so no failure here
         * carries a ciphertext or key byte (brief section 174). An unsupported version, an unknown
         * or out-of-place scheme, a truncated field, a non-canonical one-time-prekey flag, and a
         * ciphertext too short to hold an AEAD tag are all the same word to the caller: drop it.
         */
        fun decode(bytes: ByteArray): Envelope {
            val cursor = Cursor(bytes)

            val version = cursor.u8()
            if (version != ENVELOPE_VERSION) throw CryptoError.malformedHeader()
            val scheme = cursor.u8()
            val senderKeyId = cursor.varintU32()

            val preamble = when (scheme) {
                SCHEME_DOUBLE_RATCHET -> null
                SCHEME_DOUBLE_RATCHET_PREKEY -> {
                    val identityBytes = cursor.take(IDENTITY_PUBLIC_LEN)
                    val identity = try {
                        IdentityPublic.parse(identityBytes)
                    } catch (_: CryptoError) {
                        throw CryptoError.malformedHeader()
                    }
                    val ephemeralKey = cursor.take(PUBLIC_KEY_LEN)
                    val signedPrekeyId = cursor.varintU32()
                    val oneTimePrekeyId = when (cursor.u8()) {
                        0 -> null
                        1 -> cursor.varintU32()
                        // Canonical or rejected: `2` is not "true with spare bits", it is a
                        // sender this parser does not agree with, and guessing is how a parsing
                        // bug becomes a security bug.
                        else -> throw CryptoError.malformedHeader()
                    }
                    Preamble(identity, ephemeralKey, signedPrekeyId, oneTimePrekeyId)
                }
                // A sender-key envelope on the 1:1 path, or a scheme this build does not know.
                else -> throw CryptoError.malformedHeader()
            }

            val ratchetKey = cursor.take(PUBLIC_KEY_LEN)
            val messageNumber = cursor.varintU32()
            val previousChainLength = cursor.varintU32()
            val ciphertext = cursor.rest()
            if (ciphertext.size < AEAD_TAG_LEN) throw CryptoError.malformedHeader()

            return Envelope(
                scheme,
                senderKeyId,
                preamble,
                RatchetHeader.of(ratchetKey, previousChainLength, messageNumber),
                ciphertext,
            )
        }
    }
}

/**
 * A forward-only reader over the envelope bytes.
 *
 * Its own small type rather than [com.migo.core.wire.Reader] because the envelope is not MSE: it
 * is a fixed byte layout with raw varints and one run of bytes that continues to the end.
 * Borrowing the struct reader would mean pretending the envelope has a struct header it does not
 * have. Every failure is [CryptoError.malformedHeader], and none carry a byte of the input.
 *
 * `internal` rather than private because the group layer's envelope is a different layout of the
 * same kind, read the same way and failing with the same one reason. A second copy of this class
 * over there would be a second bounds check to keep correct.
 */
internal class Cursor(private val bytes: ByteArray) {
    private var offset = 0

    /** Reads one byte, or fails if the buffer ended. */
    fun u8(): Int {
        if (offset >= bytes.size) throw CryptoError.malformedHeader()
        val byte = bytes[offset].toInt() and 0xff
        offset += 1
        return byte
    }

    /** Reads exactly [len] bytes as a fresh array, or fails if fewer remain. */
    fun take(len: Int): ByteArray {
        val end = offset + len
        if (len < 0 || end > bytes.size) throw CryptoError.malformedHeader()
        val slice = bytes.copyOfRange(offset, end)
        offset = end
        return slice
    }

    /** Reads a varint and narrows it to `u32`, folding any wire error into the envelope error. */
    fun varintU32(): Long {
        val decoded = try {
            Varint.decodeU32(bytes, offset)
        } catch (_: WireError) {
            throw CryptoError.malformedHeader()
        }
        offset += decoded.used
        return decoded.value
    }

    /** Takes everything left as the ciphertext, and leaves the cursor at the end. */
    fun rest(): ByteArray {
        val start = minOf(offset, bytes.size)
        val slice = bytes.copyOfRange(start, bytes.size)
        offset = bytes.size
        return slice
    }
}
