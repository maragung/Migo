package com.migo.core.domain

import com.migo.core.net.Rest
import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.MediaAbort
import com.migo.core.protocol.MediaBegin
import com.migo.core.protocol.MediaCommit
import com.migo.core.protocol.MediaTicket
import com.migo.core.protocol.Op
import com.migo.core.wire.Id
import java.security.MessageDigest
import kotlinx.coroutines.CancellationException

/**
 * The media object plane: uploads and download URLs, the byte side of the wire.
 *
 * A port of `packages/sdk/src/domains/media.ts`. An upload is three moves —
 * [begin] mints a ticket from the socket, [uploadBytes] PUTs the bytes to the ticket's signed URL
 * over plain HTTP, [commit] finalises the object with the bytes' SHA-256 — and a caller that only
 * wants "these bytes, uploaded" uses [upload], which drives the three and aborts the ticket on any
 * failure so the server never holds a half-written object.
 *
 * The data plane is not on the socket, which is why this domain holds a [Rest] handle where its
 * neighbours hold only an [Rpc]: the signed URL names its own destination and its own authority,
 * so the PUT is an ordinary HTTP request rather than a protocol frame.
 */
class MediaDomain(
    private val rpc: Rpc,
    private val rest: Rest,
) {
    /**
     * Opens an upload ticket: the id that claims the object, and the signed URL the bytes go to.
     *
     * The claim is the caller's belief — the MIME type it thinks the bytes are, and their size; the
     * server is the authority and re-judges the bytes at commit, so a wrong claim is refused there
     * rather than here. [kind] is the media domain's own numbering (an avatar is 0); a
     * [conversationId] scopes a conversation object, and is null for profile-scoped media like an
     * avatar, whose audience is whoever may see the profile, not a conversation's members.
     */
    suspend fun begin(
        kind: Long,
        contentType: String,
        size: Long,
        conversationId: Id? = null,
    ): MediaTicket {
        val request = MediaBegin(
            kind = kind,
            contentType = contentType,
            size = size,
            conversationId = conversationId,
        )
        return rpc.call(
            Op.MEDIA_UPLOAD_BEGIN,
            { w -> request.encode(w) },
            { r -> MediaTicket.decode(r) },
        )
    }

    /**
     * PUTs the raw bytes to a ticket's upload URL.
     *
     * One request, the whole object. The content type is always `application/octet-stream` — the
     * object is an opaque blob to the HTTP layer, and its real type is the claim made at [begin].
     */
    suspend fun uploadBytes(url: String, bytes: ByteArray) {
        rest.putUploadBytes(url, bytes)
    }

    /**
     * Finalises an upload, making the object referenceable.
     *
     * The digest is the SHA-256 of the uploaded bytes — [sha256] computes the one this method
     * expects — so a truncated or corrupted PUT is caught here rather than served as damaged
     * media. The reply carries only `ok`; the caller already knows the id.
     */
    suspend fun commit(uploadId: Id, digest: ByteArray): Acknowledged {
        val request = MediaCommit(uploadId = uploadId, digest = digest)
        return rpc.call(
            Op.MEDIA_UPLOAD_COMMIT,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }

    /**
     * Abandons an upload, telling the server to drop whatever bytes it holds for the ticket.
     *
     * Call it when a caller gives up mid-upload. An abort of an unknown or already-committed
     * ticket is an error the caller can ignore — the cleanup is best-effort, never recovery.
     */
    suspend fun abort(uploadId: Id): Acknowledged {
        val request = MediaAbort(uploadId = uploadId)
        return rpc.call(
            Op.MEDIA_UPLOAD_ABORT,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }

    /**
     * "These bytes, uploaded": begin, PUT, commit — one call.
     *
     * Any failure aborts the ticket (best-effort, so the abort's own failure never masks the
     * real one) and rethrows, leaving the server holding nothing half-written. The media id the
     * caller commits to is the ticket's upload id — the object's id from the moment it exists.
     */
    suspend fun upload(
        kind: Long,
        contentType: String,
        bytes: ByteArray,
        conversationId: Id? = null,
    ): Id {
        val ticket = begin(kind, contentType, bytes.size.toLong(), conversationId)
        try {
            uploadBytes(ticket.uploadUrl, bytes)
            commit(ticket.uploadId, sha256(bytes))
        } catch (cause: Throwable) {
            if (cause is CancellationException) throw cause
            try {
                abort(ticket.uploadId)
            } catch (_: Exception) {
                // The abort is cleanup, not recovery: failing it must not mask the real error.
            }
            throw cause
        }
        return ticket.uploadId
    }
}

/**
 * The SHA-256 of `bytes`, the digest [MediaDomain.commit] expects.
 *
 * Computed with the platform's [MessageDigest] — every Android device ships one — rather than a
 * second hand-rolled implementation, because a digest this client computes wrong is a digest the
 * server will refuse at commit, and the platform's is the one already on the audit path.
 */
fun sha256(bytes: ByteArray): ByteArray =
    MessageDigest.getInstance("SHA-256").digest(bytes)
