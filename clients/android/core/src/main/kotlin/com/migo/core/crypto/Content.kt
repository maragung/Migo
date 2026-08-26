package com.migo.core.crypto

import com.migo.core.wire.Id
import com.migo.core.wire.Reader
import com.migo.core.wire.WireError
import com.migo.core.wire.Writer

/**
 * The kind of a decrypted message body.
 *
 * A distinct byte space from the protocol's `MessageKind`: that one travels in cleartext so the
 * server can route and count by coarse kind, this one lives inside the ciphertext and names the
 * exact struct that follows. They version separately, which is why this vocabulary lives here
 * rather than in the wire schema. Mirrors `clients/desktop/src/crypto/content.rs` and
 * `packages/sdk/src/content.ts`.
 */
enum class ContentType(
    /** The `content_type` byte this variant is written as -- the first byte of the plaintext. */
    val value: Int,
) {
    /** A written message, with optional inline mentions. */
    Text(1),

    /** A reference to an encrypted media object in storage, with the key to open it. */
    MediaRef(2),

    /** A reference to an encrypted voice note, with its waveform and duration. */
    VoiceNoteRef(3),

    /** An emoji reaction to another message, or the removal of one. */
    Reaction(4),

    /** An out-of-band control signal (edits, key-exchange payloads, ephemeral markers). */
    ControlEvent(5),
    ;

    companion object {
        /**
         * The type for [value], or null for a byte this build does not know.
         *
         * A null is not corruption: it is a message from a newer peer, which [Content.decode] parks
         * as [Content.Unsupported] so the interface can render "unsupported" rather than crash the
         * conversation.
         */
        fun fromByte(value: Int): ContentType? = when (value) {
            Text.value -> Text
            MediaRef.value -> MediaRef
            VoiceNoteRef.value -> VoiceNoteRef
            Reaction.value -> Reaction
            ControlEvent.value -> ControlEvent
            else -> null
        }
    }
}

/**
 * A decrypted message body, and the codec for the section 11 inner plaintext.
 *
 * The plaintext layout is `content_type || MSE body || zero padding`, and the body carries no
 * length prefix on purpose: every MSE field is self-delimiting, so [decode] consumes exactly the
 * body and leaves the padding untouched. This is a client-to-client contract -- the server never
 * sees any of it -- and it mirrors `clients/desktop/src/crypto/content.rs` and
 * `packages/sdk/src/content.ts` field for field, so a body composed here opens on the web and
 * desktop clients and the reverse holds.
 *
 * The variants are plain classes rather than `data class`es for the same reason the ratchet's
 * message types are: several hold a [ByteArray], whose value equality a generated `equals` would
 * get wrong by comparing references. Bodies are carriers, not map keys.
 */
sealed class Content {
    /**
     * The `content_type` byte this body is written under, and the byte [decode] switches on.
     *
     * A property rather than a stored field on each variant so it cannot drift out of step with the
     * type: the five known variants map to their [ContentType], and [Unsupported] carries the
     * byte it was decoded from.
     */
    abstract val contentType: Int

    /**
     * Written text, with the users referenced inline for client-side highlighting.
     *
     * [mentions] is a plain list rather than a nullable one, matching the Rust `Vec<Id>`: the
     * encoder writes the optional mentions block only when the list is non-empty, so an empty list
     * and "no mentions" are the same bytes.
     */
    class Text(
        /** The message text. */
        val text: String,
        /** Users referenced inline; empty when there are none. */
        val mentions: List<Id> = emptyList(),
    ) : Content() {
        override val contentType: Int get() = ContentType.Text.value
    }

    /**
     * A pointer to an encrypted blob in object storage.
     *
     * The server stores and serves the ciphertext by [mediaId] but cannot read it: the symmetric
     * [key] and [nonce] that open it travel only here, inside this message's own ciphertext.
     * [mimeType] and the dimensions are the sender's claim and must be re-validated after
     * decryption (brief section 122).
     */
    class MediaRef(
        /** The storage id of the encrypted blob. */
        val mediaId: Id,
        /** The sender's claimed MIME type -- re-validate after decryption. */
        val mimeType: String,
        /** The blob's length in bytes, held as an unsigned 64-bit value. */
        val sizeBytes: Long,
        /** The symmetric key that opens the blob. */
        val key: ByteArray,
        /** The nonce that opens the blob. */
        val nonce: ByteArray,
        /** Pixel width, if the sender supplied it. */
        val width: Long? = null,
        /** Pixel height, if the sender supplied it. */
        val height: Long? = null,
        /** A blurhash preview string, if supplied. */
        val blurhash: String? = null,
        /** A caption, if supplied. */
        val caption: String? = null,
    ) : Content() {
        override val contentType: Int get() = ContentType.MediaRef.value
    }

    /**
     * A pointer to an encrypted voice note.
     *
     * [waveform] is a coarse amplitude preview for the UI. As with [MediaRef], [key] and [nonce]
     * travel only inside this ciphertext, so the server that stores the note cannot play it.
     */
    class VoiceNoteRef(
        /** The storage id of the encrypted voice note. */
        val mediaId: Id,
        /** The sender's claimed MIME type -- re-validate after decryption. */
        val mimeType: String,
        /** The note's length in bytes, held as an unsigned 64-bit value. */
        val sizeBytes: Long,
        /** Playback duration in milliseconds, held as an unsigned 32-bit value. */
        val durationMs: Long,
        /** The symmetric key that opens the note. */
        val key: ByteArray,
        /** The nonce that opens the note. */
        val nonce: ByteArray,
        /** A coarse amplitude preview, if supplied. */
        val waveform: ByteArray? = null,
    ) : Content() {
        override val contentType: Int get() = ContentType.VoiceNoteRef.value
    }

    /**
     * An emoji reaction to another message.
     *
     * [remove] retracts a reaction the sender placed earlier rather than adding one, so the same
     * message type carries both the tap and the untap.
     */
    class Reaction(
        /** The message being reacted to. */
        val targetMessageId: Id,
        /** The emoji, as text. */
        val emoji: String,
        /** True to retract a reaction rather than add one. */
        val remove: Boolean,
    ) : Content() {
        override val contentType: Int get() = ContentType.Reaction.value
    }

    /**
     * An out-of-band signal that is not itself a chat message.
     *
     * [event] names the signal (`"edit"`, `"sender-key"`, `"revoke"`); [data] is an opaque body the
     * handler for that event interprets. The sender-key distribution the group layer sends rides
     * here, which is why this layer treats [data] as bytes and does not look inside.
     */
    class ControlEvent(
        /** The signal name. */
        val event: String,
        /** The opaque body, if the signal carries one. */
        val data: ByteArray? = null,
    ) : Content() {
        override val contentType: Int get() = ContentType.ControlEvent.value
    }

    /**
     * A type byte this build does not know.
     *
     * Only ever produced by [decode], never encoded: it exists so the interface can say
     * "unsupported message" instead of treating a newer peer's body as corruption. [encode] refuses
     * it, because re-serialising a body this build never parsed would be inventing bytes.
     */
    class Unsupported(
        /** The unknown content-type byte, preserved so the interface can report it. */
        override val contentType: Int,
    ) : Content()

    /**
     * Encodes this body to the section 11 inner plaintext: the type byte, the MSE body, and, unless
     * [pad] is false, zero padding up to the next bucket.
     *
     * The padding bytes are zero. They are never read back -- [decode] stops at the end of the MSE
     * struct -- so their value is immaterial, and zero keeps the sealed ciphertext free of extra
     * entropy that might otherwise hint at where the real body ended. The bucket is chosen from the
     * unpadded length alone and never from the content type, so a padded body is indistinguishable
     * in length from any other body that lands in the same bucket -- which is the whole security
     * claim.
     *
     * Refuses an [Unsupported] body with [WireError.fieldOverflow]: the `content_type` is the field
     * that does not accept it.
     */
    fun encode(pad: Boolean = true): ByteArray {
        val w = Writer()
        encodeBody(w, this)
        val body = w.finish()

        val unpadded = 1 + body.size
        val total = if (pad) bucketFor(unpadded) else unpadded
        val out = ByteArray(total)
        out[0] = contentType.toByte()
        System.arraycopy(body, 0, out, 1, body.size)
        return out
    }

    companion object {
        /**
         * The length buckets an unpadded plaintext is rounded up to when padding is on.
         *
         * Fine at the low end so a one-word reply and a sentence look identical, coarse at the high
         * end so a large body is not fingerprinted to the byte. Past the largest bucket, lengths
         * round up to the next multiple of it.
         */
        private val BUCKETS = intArrayOf(64, 256, 1024, 4096, 16384)

        /**
         * Decodes the section 11 inner plaintext, ignoring any trailing padding.
         *
         * Reads the type byte, decodes the struct for that type, and never asserts the reader was
         * fully consumed, so the padding after the struct is harmlessly left unread. An empty
         * plaintext is [WireError.unexpectedEnd] -- there is not even a type byte -- and an unknown
         * type byte becomes [Unsupported] rather than an error, so a newer peer's message survives
         * as something the interface can label.
         */
        fun decode(plaintext: ByteArray): Content {
            if (plaintext.isEmpty()) throw WireError.unexpectedEnd(0, 1)
            val tag = plaintext[0].toInt() and 0xff
            val type = ContentType.fromByte(tag) ?: return Unsupported(tag)
            // The reader spans the body and any padding; the struct decode consumes only the body,
            // and no finish() call means the trailing padding is simply never read.
            val reader = Reader(plaintext.copyOfRange(1, plaintext.size))
            return decodeBody(type, reader)
        }

        /** The padded length for an unpadded plaintext of [length] bytes. */
        private fun bucketFor(length: Int): Int {
            for (bucket in BUCKETS) {
                if (length <= bucket) return bucket
            }
            val largest = BUCKETS[BUCKETS.size - 1]
            // Integer div-ceil, matching Rust's `div_ceil` and the TypeScript `Math.ceil`.
            return ((length + largest - 1) / largest) * largest
        }

        /** Writes the MSE body for a content struct. */
        private fun encodeBody(w: Writer, content: Content) {
            when (content) {
                is Text -> {
                    w.enter()
                    w.str(content.text)
                    w.u32(if (content.mentions.isNotEmpty()) 1 else 0)
                    if (content.mentions.isNotEmpty()) {
                        w.optional(1) { sub ->
                            sub.listLen(content.mentions.size)
                            for (id in content.mentions) sub.id(id)
                        }
                    }
                    w.leave()
                }
                is MediaRef -> {
                    w.enter()
                    w.id(content.mediaId)
                    w.str(content.mimeType)
                    w.u64(content.sizeBytes)
                    w.bytes(content.key)
                    w.bytes(content.nonce)
                    var present = 0
                    if (content.width != null) present += 1
                    if (content.height != null) present += 1
                    if (content.blurhash != null) present += 1
                    if (content.caption != null) present += 1
                    w.u32(present)
                    content.width?.let { v -> w.optional(1) { sub -> sub.u32(v) } }
                    content.height?.let { v -> w.optional(2) { sub -> sub.u32(v) } }
                    content.blurhash?.let { v -> w.optional(3) { sub -> sub.str(v) } }
                    content.caption?.let { v -> w.optional(4) { sub -> sub.str(v) } }
                    w.leave()
                }
                is VoiceNoteRef -> {
                    w.enter()
                    w.id(content.mediaId)
                    w.str(content.mimeType)
                    w.u64(content.sizeBytes)
                    w.u32(content.durationMs)
                    w.bytes(content.key)
                    w.bytes(content.nonce)
                    w.u32(if (content.waveform != null) 1 else 0)
                    content.waveform?.let { v -> w.optional(1) { sub -> sub.bytes(v) } }
                    w.leave()
                }
                is Reaction -> {
                    w.enter()
                    w.id(content.targetMessageId)
                    w.str(content.emoji)
                    w.bool(content.remove)
                    w.u32(0)
                    w.leave()
                }
                is ControlEvent -> {
                    w.enter()
                    w.str(content.event)
                    w.u32(if (content.data != null) 1 else 0)
                    content.data?.let { v -> w.optional(1) { sub -> sub.bytes(v) } }
                    w.leave()
                }
                is Unsupported -> {
                    // Never written. This variant only ever comes out of decode, where a type byte
                    // this build does not know is parked so the interface can render it honestly.
                    // Round-tripping it would mean re-sending a body that was never parsed, so it
                    // is refused: fieldOverflow is the codec's "this value does not belong in that
                    // field", and content_type is the field.
                    throw WireError.fieldOverflow("content_type")
                }
            }
        }

        /** Reads the MSE body for a content struct of the given type. */
        private fun decodeBody(type: ContentType, r: Reader): Content = when (type) {
            ContentType.Text -> {
                r.enter()
                val text = r.str()
                var mentions: List<Id> = emptyList()
                var optionalCount = r.u32()
                while (optionalCount > 0) {
                    val (fieldId, sub) = r.optional()
                    if (fieldId == 1L) {
                        val count = sub.listLen()
                        val ids = ArrayList<Id>(count)
                        repeat(count) { ids.add(sub.id()) }
                        mentions = ids
                    }
                    optionalCount -= 1
                }
                r.leave()
                Text(text, mentions)
            }
            ContentType.MediaRef -> {
                r.enter()
                val mediaId = r.id()
                val mimeType = r.str()
                val sizeBytes = r.u64()
                val key = r.bytes()
                val nonce = r.bytes()
                var width: Long? = null
                var height: Long? = null
                var blurhash: String? = null
                var caption: String? = null
                var optionalCount = r.u32()
                while (optionalCount > 0) {
                    val (fieldId, sub) = r.optional()
                    when (fieldId) {
                        1L -> width = sub.u32()
                        2L -> height = sub.u32()
                        3L -> blurhash = sub.str()
                        4L -> caption = sub.str()
                        else -> {}
                    }
                    optionalCount -= 1
                }
                r.leave()
                MediaRef(mediaId, mimeType, sizeBytes, key, nonce, width, height, blurhash, caption)
            }
            ContentType.VoiceNoteRef -> {
                r.enter()
                val mediaId = r.id()
                val mimeType = r.str()
                val sizeBytes = r.u64()
                val durationMs = r.u32()
                val key = r.bytes()
                val nonce = r.bytes()
                var waveform: ByteArray? = null
                var optionalCount = r.u32()
                while (optionalCount > 0) {
                    val (fieldId, sub) = r.optional()
                    if (fieldId == 1L) waveform = sub.bytes()
                    optionalCount -= 1
                }
                r.leave()
                VoiceNoteRef(mediaId, mimeType, sizeBytes, durationMs, key, nonce, waveform)
            }
            ContentType.Reaction -> {
                r.enter()
                val targetMessageId = r.id()
                val emoji = r.str()
                val remove = r.bool()
                var optionalCount = r.u32()
                while (optionalCount > 0) {
                    r.optional()
                    optionalCount -= 1
                }
                r.leave()
                Reaction(targetMessageId, emoji, remove)
            }
            ContentType.ControlEvent -> {
                r.enter()
                val event = r.str()
                var data: ByteArray? = null
                var optionalCount = r.u32()
                while (optionalCount > 0) {
                    val (fieldId, sub) = r.optional()
                    if (fieldId == 1L) data = sub.bytes()
                    optionalCount -= 1
                }
                r.leave()
                ControlEvent(event, data)
            }
        }
    }
}
