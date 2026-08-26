package com.migo.core.domain

import com.migo.core.protocol.Op
import com.migo.core.protocol.SyncRequest
import com.migo.core.protocol.SyncResponse
import com.migo.core.wire.Id

/**
 * Fetching conversation history this client is missing.
 *
 * A port of `packages/sdk/src/domains/sync.ts`. A client holds the highest contiguous sequence number
 * it has per conversation; when it reconnects, or when a live event arrives with a sequence past that
 * watermark, it asks for the range in between.
 *
 * # Sync moves ciphertext
 *
 * What comes back is a list of sealed `MessageEvent`s -- the same shape a live delivery has, still
 * encrypted -- because the server has no plaintext to return. The caller replays each through
 * [MessagingDomain.ingest], which opens and routes it exactly as if it had just arrived. That is the
 * whole reason this domain is thin: there is no second decryption path to keep in step with the
 * first.
 *
 * # Two things a caller owes
 *
 * Replay in **ascending sequence order**. A sender-key distribution has a lower sequence than the
 * content it unlocks, so replaying forward hands the messaging layer each key before the messages
 * that need it, and the pending buffer stays a fallback rather than the normal path.
 *
 * **De-duplicate.** Both crypto layers refuse a second decrypt of the same message, which is replay
 * protection working correctly; a caller that replayed an overlapping page twice would see history
 * that will not open.
 *
 * # The truncation boundary
 *
 * The server may cap how far back it serves. A [com.migo.core.protocol.SyncStatus.Truncated] response
 * means older history exists and was not returned, and a client should draw a visible boundary for
 * it. Rendering a truncated response as continuous is how a user comes to believe a conversation
 * started later than it did.
 */
class SyncDomain(private val rpc: Rpc) {
    /**
     * Fetches up to [limit] messages for a conversation.
     *
     * [haveSeq] is the highest contiguous sequence already held; the server returns what follows it,
     * or with [backwards] what precedes it. On success the caller replays [SyncResponse.messages] in
     * order and then advances its watermark to [SyncResponse.toSeq]; a [SyncResponse.more] of true
     * means another page is waiting, with that same `toSeq` as the next [haveSeq].
     *
     * [toSeq] bounds the far end, which is what fills a *detected gap* rather than tailing the
     * latest: a client that saw sequence 40 arrive while holding 30 asks for 31 to 39 and nothing
     * else, instead of re-fetching a page it will mostly discard.
     */
    suspend fun fetch(
        conversationId: Id,
        haveSeq: Long,
        limit: Long,
        toSeq: Long? = null,
        backwards: Boolean? = null,
    ): SyncResponse {
        val request = SyncRequest(conversationId, haveSeq, limit, toSeq, backwards)
        return rpc.call(Op.SYNC, { w -> request.encode(w) }, { r -> SyncResponse.decode(r) })
    }
}
