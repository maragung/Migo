package com.migo.core.domain

import com.migo.core.protocol.ConversationCreateRequest
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.ConversationListRequest
import com.migo.core.protocol.ConversationListResponse
import com.migo.core.protocol.ConversationSummary
import com.migo.core.protocol.Op
import com.migo.core.wire.Id

/**
 * Listing the conversations this account is in, and creating new ones.
 *
 * A port of `packages/sdk/src/domains/conversations.ts`. Request and response only -- no crypto, no
 * event stream -- which is why it holds nothing but its [Rpc].
 *
 * # The property worth knowing
 *
 * [ConversationsDomain.create] is idempotent for a [ConversationKind.Direct] chat. The server derives
 * the conversation id deterministically from the sorted member ids, so "the conversation with Alice"
 * resolves to the same conversation every time it is asked for. That is what makes it safe to call
 * before a first send rather than tracking whether one has been created, and it is why there is no
 * `ensureConversation` helper here: create already is one.
 *
 * # What the encryption mode in a summary does and does not say
 *
 * [ConversationSummary.encryption] is a claim about the conversation's transport policy, and a client
 * may show it. It is not the end-to-end guarantee: content is sealed by the crypto layers on the way
 * out regardless of what a summary advertises, so a summary that came back with an unexpected mode
 * cannot cause plaintext to be sent. Treating the field as the source of truth for "is this
 * encrypted" would put a security decision in the hands of the party the encryption protects
 * against.
 */
class ConversationsDomain(private val rpc: Rpc) {
    /**
     * Lists the account's conversations, most recent activity first.
     *
     * Pass the [ConversationListResponse.nextCursor] of a previous page as [cursor] for the next one;
     * a response without a cursor is the last page. [limit] bounds one page, and the server may
     * return fewer.
     */
    suspend fun list(limit: Long, cursor: String? = null): ConversationListResponse {
        val request = ConversationListRequest(limit, cursor)
        return rpc.call(
            Op.CONVERSATION_LIST,
            { w -> request.encode(w) },
            { r -> ConversationListResponse.decode(r) },
        )
    }

    /**
     * Creates a conversation, or returns the existing one for a direct chat.
     *
     * [members] is the *other* participants: the server adds the caller, and a client that included
     * itself would be asking for a two-person conversation with one person in it. [title] is
     * meaningful for a group and ignored for a direct chat, where the name shown is the other
     * person's.
     */
    suspend fun create(
        kind: ConversationKind,
        members: List<Id>,
        title: String? = null,
    ): ConversationSummary {
        val request = ConversationCreateRequest(kind, members, title)
        return rpc.call(
            Op.CONVERSATION_CREATE,
            { w -> request.encode(w) },
            { r -> ConversationSummary.decode(r) },
        )
    }
}
