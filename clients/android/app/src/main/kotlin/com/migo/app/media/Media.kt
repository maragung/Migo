package com.migo.app.media

import android.graphics.BitmapFactory
import com.migo.core.crypto.AEAD_KEY_LEN
import com.migo.core.crypto.AEAD_NONCE_LEN
import com.migo.core.crypto.Aead
import com.migo.core.crypto.Content
import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.SymmetricKey
import com.migo.core.domain.MediaDomain
import com.migo.core.domain.MediaKinds
import com.migo.core.wire.Id

/**
 * The send and open halves of attachment media: a port of the web client's `lib/migo/media.ts`
 * and `lib/migo/voice.ts`, and it has to stay a port -- a message sealed here is opened by the web
 * client and the desktop client, so any divergence in *which* layer seals, under what domain
 * label, or what the message claims would show up as a message the other client cannot render.
 *
 * # The rule, and the one exception
 *
 * Media is ciphertext at rest: [sealMedia] mints a fresh symmetric key per upload, seals the
 * bytes with the house AEAD (XChaCha20-Poly1305, the same `Aead` the call signaling uses), and
 * uploads only the sealed bytes. The key and nonce ride to the conversation's devices inside the
 * *message* -- which is itself sealed -- so the server that stores the object cannot read it.
 *
 * The exception is a server-readable room: a room conversation is Transport-encrypted by design,
 * so sealing an upload for it would hand the recipients a key through a channel the room does not
 * have. There the legacy plaintext path applies -- the bytes are stored as uploaded and the
 * message's key and nonce slots carry the zero-filled placeholders of an older build (web's
 * `LEGACY_PLAINTEXT_SLOTS`), which the receiver detects with [isLegacyPlaintext] and passes
 * through unopened.
 *
 * # The claims, stated honestly
 *
 * A sealed upload declares itself `application/octet-stream`, because that is what the stored
 * bytes are; the *message* carries the plaintext's MIME type and size, the claim the renderer
 * labels the opened blob with. A room's plaintext upload claims the file's real type, which the
 * server's sniffer then re-judges at commit (brief section 122). Documents are a private-and-group
 * feature -- always sealed, capped at [DOCUMENT_MAX_BYTES]; a room's composer offers no attach
 * button at all, mirroring the web client's gating.
 */

/** The associated-data domain an image or document is sealed under. */
val MEDIA_SEAL_DOMAIN: ByteArray = "migo-media".toByteArray()

/** The associated-data domain a voice note is sealed under. */
val VOICE_SEAL_DOMAIN: ByteArray = "migo-voice".toByteArray()

/**
 * The ceiling a document attachment may not exceed, matching the server's `Document` media kind.
 * Checked before any upload call so an oversized file fails with an honest sentence instead of a
 * server round trip that was always going to refuse.
 */
const val DOCUMENT_MAX_BYTES: Long = 32L * 1024L * 1024L

/** The server's cap on a voice note; a longer recording is refused at upload. */
const val VOICE_NOTE_MAX_MS: Long = 300_000L

/** One refusal a person can act on: a file too large, a recording too long. */
class AttachmentRefusal(message: String) : Exception(message)

/**
 * A fresh per-object seal: the travelling key and nonce for the message's slots, and the
 * `nonce || ciphertext || tag` bytes to upload.
 *
 * The key is drawn inside [sealMedia] and never accepted from a caller, for the same reason the
 * web module refuses it: per-object sealing only means anything if no two objects can ever share
 * key material by accident.
 */
class SealedMedia internal constructor(
    val key: ByteArray,
    val nonce: ByteArray,
    val sealed: ByteArray,
)

/** Seals [plaintext] under [domain] with a fresh random key. */
fun sealMedia(plaintext: ByteArray, domain: ByteArray): SealedMedia {
    val key = SymmetricKey.generate()
    val sealed = Aead.seal(key, domain, plaintext)
    // The travelling copy is made before the key's own buffer is destroyed: what leaves this
    // function is the copy alone, exactly the discipline the web module keeps.
    val travelling = key.expose().copyOf()
    key.destroy()
    return SealedMedia(travelling, sealed.copyOfRange(0, AEAD_NONCE_LEN), sealed)
}

/**
 * Whether a message's key slot is the zero-filled placeholder of a legacy plaintext upload: bytes
 * stored as uploaded, no sealing, the renderer must not attempt to open them.
 *
 * Sound because a sealed upload's key is 256 random bits -- a zero key cannot be one.
 */
fun isLegacyPlaintext(key: ByteArray): Boolean =
    key.size == AEAD_KEY_LEN && key.all { it == 0.toByte() }

/**
 * Opens a stored object with the key and nonce as they arrived in the message's slots.
 *
 * The sealed blob embeds its own nonce, and the slot carries the same 24 bytes; the two are
 * compared rather than trusting either alone, so a message whose slots were spliced from a
 * different object than its bytes fails here as a decryption failure -- the same refusal for
 * every cause, telling a wrong key from edited bytes apart is a fact the caller must never learn.
 */
fun openMedia(key: ByteArray, nonce: ByteArray, domain: ByteArray, stored: ByteArray): ByteArray {
    if (stored.size < AEAD_NONCE_LEN) {
        throw CryptoError.badLength("sealed content", AEAD_NONCE_LEN, stored.size)
    }
    for (i in 0 until AEAD_NONCE_LEN) {
        if (stored[i] != nonce[i]) {
            throw CryptoError.decryptionFailed()
        }
    }
    return Aead.open(SymmetricKey.fromBytes(key), domain, stored)
}

/**
 * The pixel bounds of an image, read from the bytes' header alone -- [BitmapFactory] with
 * `inJustDecodeBounds` allocates nothing, which is the whole reason the claim can be made on the
 * send path without paying for a decode.
 */
fun imageDimensions(bytes: ByteArray): Pair<Long, Long>? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    return bounds.outWidth.toLong() to bounds.outHeight.toLong()
}

/**
 * Normalises a recorded MIME type to its container claim: the type before any `;` parameters.
 *
 * A recorder reports e.g. `audio/mp4; codecs=...` -- the parameter is true but noise in a claim
 * the receiver labels the opened blob with. An empty input stays empty; the uploader substitutes
 * the neutral claim.
 */
fun normalizeVoiceMime(mimeType: String): String = mimeType.split(';', limit = 2).first().trim()

/**
 * A recording or playback length as `M:SS` — `0:34`, `1:00` — the shape the recording timer and a
 * bubble's duration label share. Rounds down and collapses a negative input to `0:00`.
 */
fun formatDuration(durationMs: Long): String {
    val safe = if (durationMs > 0) durationMs else 0L
    val totalSeconds = safe / 1000
    return "${totalSeconds / 60}:${(totalSeconds % 60).toString().padStart(2, '0')}"
}

/** A byte count as a person reads it: `412 KB`, `1.8 MB` — the document row's size line. */
fun formatBytes(sizeBytes: Long): String {
    if (sizeBytes < 1024) return "$sizeBytes B"
    val kb = sizeBytes / 1024.0
    if (kb < 1024) return String.format("%.0f KB", kb)
    return String.format("%.1f MB", kb / 1024.0)
}

/** How many bars a recorded waveform folds into, and what a bubble renders at most. */
const val WAVEFORM_BARS: Int = 50

/**
 * One sampled amplitude as the 0–255 bar byte a waveform carries: [MediaRecorder.getMaxAmplitude]
 * tops out at 32767, and the fold below and the receiver's renderer both speak bytes.
 */
fun amplitudeToBar(amplitude: Int): Int {
    val clamped = if (amplitude in 0..32767) amplitude else 0
    return clamped * 255 / 32767
}

/**
 * Folds a stream of sampled amplitudes into the fixed-width bar chart a bubble renders — the same
 * fold the web client's `downsampleWaveform` keeps, so a note recorded on either client carries
 * the same shape of preview.
 *
 * Each output bar is the *maximum* sample in its slice, because a peak — not an average — is what
 * a waveform bar is drawn from: a syllable landing inside a bucket must show, and averaging would
 * flatten it into the silence around it. The output is always exactly `barCount` bytes: an input
 * shorter than the bar count pads with silence at the tail, an empty input is all silence, and
 * values are clamped to the 0–255 byte. The fold also runs over hostile receiver-supplied
 * waveforms at render time, so it degrades gracefully rather than throwing.
 */
fun downsampleWaveform(samples: IntArray, barCount: Int = WAVEFORM_BARS): ByteArray {
    val bars = ByteArray(barCount)
    if (samples.isEmpty() || barCount <= 0) {
        return bars
    }
    val bucketSize = maxOf(1, (samples.size + barCount - 1) / barCount)
    for (i in samples.indices) {
        val value = samples[i]
        val clamped = if (value in 0..255) value else 0
        val barIndex = minOf(barCount - 1, i / bucketSize)
        // The stored bar reads back signed — a 200 is a -56 as a Byte — so the comparison
        // unwraps it to its unsigned value first; compared raw, the tail's quiet samples would
        // overwrite every peak above 127 the fold had already kept.
        if (clamped > (bars[barIndex].toInt() and 0xFF)) {
            bars[barIndex] = clamped.toByte()
        }
    }
    return bars
}

/** The neutral claim an upload whose real type is unknown falls back to. */
private fun claimMime(mimeType: String): String = mimeType.ifBlank { "application/octet-stream" }

/** The legacy plaintext path's zero-filled key and nonce slots, matching the web client's. */
private val LEGACY_PLAINTEXT_KEY = ByteArray(AEAD_KEY_LEN)
private val LEGACY_PLAINTEXT_NONCE = ByteArray(AEAD_NONCE_LEN)

/**
 * Uploads a picked image and returns the message body that references it.
 *
 * In an end-to-end conversation (every direct conversation and group) the bytes are sealed before
 * anything crosses the wire; a server-readable room keeps the legacy plaintext path. The image's
 * own dimensions, when they can be read, ride both the upload and the message, so a receiver can
 * lay out before downloading.
 */
suspend fun uploadImageAttachment(
    media: MediaDomain,
    conversationId: Id,
    bytes: ByteArray,
    mimeType: String,
    endToEnd: Boolean,
): Content.MediaRef {
    val mime = claimMime(mimeType)
    val dimensions = imageDimensions(bytes)

    if (!endToEnd) {
        val mediaId = media.upload(
            kind = MediaKinds.IMAGE,
            contentType = mime,
            bytes = bytes,
            conversationId = conversationId,
            width = dimensions?.first,
            height = dimensions?.second,
        )
        return imageContent(
            mediaId, mime, bytes.size.toLong(), dimensions,
            LEGACY_PLAINTEXT_KEY, LEGACY_PLAINTEXT_NONCE,
        )
    }

    val sealed = sealMedia(bytes, MEDIA_SEAL_DOMAIN)
    // The stored object is opaque ciphertext; the honest claim for it is the neutral one. The
    // message's own claim describes the *content*: the plaintext's type and size.
    val mediaId = media.upload(
        kind = MediaKinds.IMAGE,
        contentType = "application/octet-stream",
        bytes = sealed.sealed,
        conversationId = conversationId,
        width = dimensions?.first,
        height = dimensions?.second,
    )
    return imageContent(
        mediaId, mime, bytes.size.toLong(), dimensions,
        sealed.key, sealed.nonce,
    )
}

/**
 * The message body for an uploaded image, in the sender's claim of type and size, with the key
 * material that opens the sealed object. A pure function of its inputs, so the content shape is
 * one a test can pin.
 */
private fun imageContent(
    mediaId: Id,
    mimeType: String,
    sizeBytes: Long,
    dimensions: Pair<Long, Long>?,
    key: ByteArray,
    nonce: ByteArray,
): Content.MediaRef = Content.MediaRef(
    mediaId = mediaId,
    mimeType = mimeType,
    sizeBytes = sizeBytes,
    key = key,
    nonce = nonce,
    width = dimensions?.first,
    height = dimensions?.second,
)

/**
 * Uploads a picked document and returns the message body that references it.
 *
 * Documents are a private-and-group feature, and every direct conversation and group is
 * end-to-end, so there is no plaintext branch to keep: the bytes are always sealed. The file's
 * name rides in the `MediaRef`'s caption slot -- the only free-text field the content shape has --
 * and the receiver's renderer tells a document from an image by the claimed MIME type.
 */
suspend fun uploadDocumentAttachment(
    media: MediaDomain,
    conversationId: Id,
    bytes: ByteArray,
    mimeType: String,
    fileName: String,
): Content.MediaRef {
    if (bytes.size.toLong() > DOCUMENT_MAX_BYTES) {
        throw AttachmentRefusal("That file is too large to send (over 32 MB).")
    }
    val mime = claimMime(mimeType)
    val sealed = sealMedia(bytes, MEDIA_SEAL_DOMAIN)
    val mediaId = media.upload(
        kind = MediaKinds.DOCUMENT,
        contentType = "application/octet-stream",
        bytes = sealed.sealed,
        conversationId = conversationId,
    )
    return Content.MediaRef(
        mediaId = mediaId,
        mimeType = mime,
        sizeBytes = bytes.size.toLong(),
        key = sealed.key,
        nonce = sealed.nonce,
        caption = fileName,
    )
}

/**
 * Uploads a finished voice recording and returns the message body that references it.
 *
 * The five-minute cap is enforced before any bytes cross the wire — the server would refuse the
 * object at commit anyway, and a failed five-minute upload is the worst possible place to learn
 * that. The duration rides both the upload and the message, so the receiver can size the player
 * before downloading anything. In an end-to-end conversation the bytes are sealed; a
 * server-readable room keeps the legacy plaintext path.
 */
suspend fun uploadVoiceNote(
    media: MediaDomain,
    conversationId: Id,
    bytes: ByteArray,
    containerMime: String,
    durationMs: Long,
    endToEnd: Boolean,
    waveform: ByteArray? = null,
): Content.VoiceNoteRef {
    if (durationMs > VOICE_NOTE_MAX_MS) {
        throw AttachmentRefusal("Voice notes are capped at 5 minutes.")
    }
    val container = normalizeVoiceMime(containerMime)
    val claim = container.ifBlank { "application/octet-stream" }

    if (!endToEnd) {
        val mediaId = media.upload(
            kind = MediaKinds.VOICE_NOTE,
            contentType = claim,
            bytes = bytes,
            conversationId = conversationId,
            durationMs = durationMs,
        )
        return voiceContent(
            mediaId, claim, bytes.size.toLong(), durationMs,
            LEGACY_PLAINTEXT_KEY, LEGACY_PLAINTEXT_NONCE, waveform,
        )
    }

    val sealed = sealMedia(bytes, VOICE_SEAL_DOMAIN)
    // The stored object is opaque ciphertext; the message's own claim — the real container — is
    // what the player labels the opened blob with.
    val mediaId = media.upload(
        kind = MediaKinds.VOICE_NOTE,
        contentType = "application/octet-stream",
        bytes = sealed.sealed,
        conversationId = conversationId,
        durationMs = durationMs,
    )
    return voiceContent(
        mediaId, claim, bytes.size.toLong(), durationMs,
        sealed.key, sealed.nonce, waveform,
    )
}

/** The message body for an uploaded voice note, in the sender's claim of type and duration. */
private fun voiceContent(
    mediaId: Id,
    mimeType: String,
    sizeBytes: Long,
    durationMs: Long,
    key: ByteArray,
    nonce: ByteArray,
    waveform: ByteArray? = null,
): Content.VoiceNoteRef = Content.VoiceNoteRef(
    mediaId = mediaId,
    mimeType = mimeType,
    sizeBytes = sizeBytes,
    durationMs = durationMs,
    key = key,
    nonce = nonce,
    waveform = waveform,
)
