package com.migo.core.crypto

import com.migo.core.wire.Id
import com.migo.core.wire.idFromBytes
import com.migo.core.wire.idToBytes

/**
 * What an envelope's tag authenticates besides its contents.
 *
 * The AEAD in [Aead] authenticates a ciphertext against an *associated data* string. That string is
 * where a protocol binds the metadata it is not willing to encrypt but is not willing to leave
 * forgeable either, and until envelope version 2, Migo's bound less than the specification asks for.
 *
 * # What version 1 binds, and the hole that leaves
 *
 * A v1 associated data is the X3DH associated data followed by the ratchet header: the two device
 * identities, through X3DH's initiator and responder identity keys, and the ratchet public key with
 * its two counters. The identity binding is the anti-unknown-key-share protection brief section 11
 * cares about most, and it holds. What v1 does not bind is *context*: which conversation the envelope
 * belongs to, which device sent it, which message it is, and which version and scheme the envelope
 * claims.
 *
 * Section 11 names the consequence: without it, E2E protects the content but not the context. A
 * server that can read the conversation id field of a message frame can rewrite it, and the tag will
 * still verify, because the receiver computes the same associated data either way. For an ordinary
 * message that is a denial of service at worst. For a sender-key distribution, which is the pairwise
 * envelope this layer exists to carry, it is worse: the distribution installs a chain key, and moving
 * one into a conversation it was not meant for installs a chain there that the sender never
 * authorised.
 *
 * # What version 2 binds, and why the layout is what it is
 *
 * The v2 associated data is the v1 one followed by a context naming the five fields of section 11:
 *
 * ```text
 * migo-envelope-aad     17 bytes   purpose label, so these bytes are not any other structure's
 * envelope_version      u8         must equal the envelope's own version byte
 * scheme                u8         must equal the envelope's own scheme byte
 * sender_device         16 bytes   the device the frame claims to be from
 * conversation_id       16 bytes   the conversation the frame claims to belong to
 * flags                 u8         bit 0: a message id follows
 * message_id            16 bytes   present only when flags bit 0 is set
 * ```
 *
 * Every field but the message id is fixed width, so no length prefix is needed and no two different
 * field assignments can encode to the same bytes. The message id gets a flag byte rather than a
 * length prefix because it is the only optional field: one byte states everything the receiver
 * needs, and a length that could disagree with the flag is a length that will.
 *
 * The context is built from the metadata the *frame claims*, not from anything the receiver
 * independently knows. That is the whole mechanism: the receiver recomputes the context from what it
 * was told, the tag only verifies if the sender bound exactly those values, and a server that edits
 * any of them turns a readable message into a failed authentication instead of a silent relocation.
 *
 * # Ids are typed here
 *
 * [Aad.context] takes [Id] values rather than raw byte arrays, which is the reference crate's choice
 * and unlike `packages/crypto`, where the same function takes bytes and checks their length. The
 * difference is what each package is allowed to depend on: that one is auditable without the framing
 * layer, and this one already reads [Id] in the content, call-key and safety-number modules for
 * exactly this reason. An [Id] is 16 bytes by construction, so there is no length to check and no
 * way to pass a 15-byte one.
 *
 * # The version-1 rule
 *
 * [Aad.context] returns empty bytes for [EnvelopeVersion.V1], whatever metadata the caller passes.
 * This is not a convenience: reading the version byte before the tag has been verified is only safe
 * while a v1 envelope's associated data does not depend on the metadata, and a receiver must be able
 * to compute the right associated data for both versions from the byte it sees. If a v1 context ever
 * became non-empty, every stored v1 message would fail to open, and the failure would look like
 * corruption rather than a version mistake.
 *
 * The cross-language vectors in `shared/protocol/vectors/crypto/aad-context.json` pin both the layout
 * and that rule, and [CryptoVectorsTest] runs them against this file.
 */

/**
 * An envelope version, as a value that cannot be constructed from a number the protocol does not
 * define.
 *
 * Named the way the reference enum is, so a reader moving between the implementations reads the same
 * code. [fromWire] refuses every other byte rather than defaulting, because a default would mean
 * computing an associated data for a layout the sender did not choose.
 */
enum class EnvelopeVersion(
    /** The version byte as it travels. */
    val wire: Int,
) {
    /** Binds identities and the ratchet header, and nothing else. */
    V1(1),

    /** Binds the five context fields of section 11 as well. */
    V2(2),
    ;

    /** Whether this version's associated data carries a context. */
    val bindsContext: Boolean get() = this == V2

    companion object {
        /** Every version a reader must accept, oldest first. */
        val ACCEPTED: List<EnvelopeVersion> = listOf(V1, V2)

        /**
         * The version this build writes.
         *
         * A name for the choice rather than a second constant beside it: two constants that must
         * agree is one constant too many, and the envelope's own default reads this one.
         */
        val WRITTEN: EnvelopeVersion = V2

        /** Reads a version byte, or null for a value no build writes. */
        fun fromWire(value: Int): EnvelopeVersion? = ACCEPTED.firstOrNull { it.wire == value }
    }
}

/** The fields a context carries, as [Aad.parseContext] recovered them. */
class ParsedAadContext(
    /** The envelope version the context binds. */
    val version: EnvelopeVersion,
    /** The scheme byte the context binds. */
    val scheme: Int,
    /** The sending device the context binds. */
    val senderDevice: Id,
    /** The conversation the context binds. */
    val conversationId: Id,
    /** The message id the context binds, or null when it carries none. */
    val messageId: Id?,
) {
    /** Public fields only; a context holds no key material and no ciphertext. */
    override fun toString(): String =
        "ParsedAadContext(version: ${version.wire}, scheme: $scheme, " +
            "sender_device: $senderDevice, conversation_id: $conversationId, " +
            "message_id: ${messageId ?: "none"})"
}

/**
 * The context builder, the assembler and the parser.
 *
 * An object rather than four top-level functions so the domain label has one owner: the layout is a
 * cross-client contract, and a second spelling of the label elsewhere in this package would be a
 * silent disagreement on the wire.
 */
object Aad {
    /** Bit 0 of the flags byte: a message id follows. */
    const val FLAG_MESSAGE_ID = 0x01

    /**
     * Flags no build writes.
     *
     * A receiver refuses a context carrying one, because a bit whose meaning is undefined is a bit an
     * attacker chooses the meaning of.
     */
    const val KNOWN_FLAGS = FLAG_MESSAGE_ID

    /** Bytes in an identifier, matching the wire codec's own constant. */
    const val ID_BYTE_LEN = 16

    private val DOMAIN_BYTES = "migo-envelope-aad".toByteArray()

    /**
     * The context a version-1 envelope binds: none.
     *
     * Named rather than written as a literal at each call site, because a bare empty array reads as a
     * value somebody forgot to fill in, and this one is the value version 1 requires. Every call site
     * passes it today, which is what makes this change purely additive: a version-1 envelope's
     * associated data is byte for byte what it was before contexts existed.
     */
    val NO_CONTEXT = ByteArray(0)

    /** The purpose label every v2 context starts with, as a fresh copy. */
    val domain: ByteArray get() = DOMAIN_BYTES.copyOf()

    /**
     * Builds the context an envelope of [version] binds.
     *
     * Returns an empty array for [EnvelopeVersion.V1], which is the compatibility rule described on
     * this file rather than an optimisation.
     *
     * [messageId] is null for an envelope that is not a message: a sender-key distribution rides
     * inside the message that carries it, so at seal time it has no id of its own to bind, and
     * pretending otherwise by binding the carrier's id would bind a value the receiver does not have.
     */
    fun context(
        version: EnvelopeVersion,
        scheme: Int,
        senderDevice: Id,
        conversationId: Id,
        messageId: Id? = null,
    ): ByteArray {
        if (!version.bindsContext) return ByteArray(0)

        val out = ByteArray(
            DOMAIN_BYTES.size + 3 + ID_BYTE_LEN * (if (messageId == null) 2 else 3),
        )
        var at = 0
        System.arraycopy(DOMAIN_BYTES, 0, out, at, DOMAIN_BYTES.size)
        at += DOMAIN_BYTES.size
        out[at] = version.wire.toByte()
        out[at + 1] = scheme.toByte()
        at += 2
        System.arraycopy(idToBytes(senderDevice), 0, out, at, ID_BYTE_LEN)
        at += ID_BYTE_LEN
        System.arraycopy(idToBytes(conversationId), 0, out, at, ID_BYTE_LEN)
        at += ID_BYTE_LEN
        out[at] = if (messageId == null) 0 else FLAG_MESSAGE_ID.toByte()
        at += 1
        if (messageId != null) {
            System.arraycopy(idToBytes(messageId), 0, out, at, ID_BYTE_LEN)
        }
        return out
    }

    /**
     * Assembles the associated data a tag is computed over: the v1 prefix followed by the context.
     *
     * The one place the two halves are joined, so a reader and a writer of the same version cannot
     * disagree about the order.
     */
    fun assemble(
        associatedData: ByteArray,
        header: ByteArray,
        context: ByteArray,
    ): ByteArray {
        val out = ByteArray(associatedData.size + header.size + context.size)
        System.arraycopy(associatedData, 0, out, 0, associatedData.size)
        System.arraycopy(header, 0, out, associatedData.size, header.size)
        System.arraycopy(context, 0, out, associatedData.size + header.size, context.size)
        return out
    }

    /**
     * Reads the fields a context claims back out of its bytes.
     *
     * A receiver does not need this: it builds the context from the metadata it was handed and lets
     * the tag decide. It exists for the conformance vectors and for diagnostics, so it validates
     * rather than assumes. A buffer that is too short is [CryptoError.badLength]; a wrong label, a
     * version no build writes, a flag no build writes, and trailing bytes are all
     * [CryptoError.malformedHeader], because each of them means the bytes are not a context rather
     * than a context of the wrong size.
     */
    fun parseContext(bytes: ByteArray): ParsedAadContext {
        val fixed = DOMAIN_BYTES.size + 3 + ID_BYTE_LEN * 2
        if (bytes.size < fixed) {
            throw CryptoError.badLength("envelope aad context", fixed, bytes.size)
        }
        for (i in DOMAIN_BYTES.indices) {
            if (bytes[i] != DOMAIN_BYTES[i]) throw CryptoError.malformedHeader()
        }

        var at = DOMAIN_BYTES.size
        val version = EnvelopeVersion.fromWire(bytes[at].toInt() and 0xff)
            ?: throw CryptoError.malformedHeader()
        at += 1
        val scheme = bytes[at].toInt() and 0xff
        at += 1
        val senderDevice = idFromBytes(bytes.copyOfRange(at, at + ID_BYTE_LEN))
        at += ID_BYTE_LEN
        val conversationId = idFromBytes(bytes.copyOfRange(at, at + ID_BYTE_LEN))
        at += ID_BYTE_LEN
        val flags = bytes[at].toInt() and 0xff
        at += 1
        if (flags and KNOWN_FLAGS.inv() != 0) throw CryptoError.malformedHeader()

        var messageId: Id? = null
        if (flags and FLAG_MESSAGE_ID != 0) {
            if (bytes.size < at + ID_BYTE_LEN) {
                throw CryptoError.badLength(
                    "envelope aad context message id",
                    at + ID_BYTE_LEN,
                    bytes.size,
                )
            }
            messageId = idFromBytes(bytes.copyOfRange(at, at + ID_BYTE_LEN))
            at += ID_BYTE_LEN
        }
        if (at != bytes.size) {
            throw CryptoError.badLength("envelope aad context", at, bytes.size)
        }
        return ParsedAadContext(version, scheme, senderDevice, conversationId, messageId)
    }
}
