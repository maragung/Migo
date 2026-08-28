package com.migo.app.model

import com.migo.core.ConnectionState
import com.migo.core.protocol.ConversationKind
import com.migo.core.store.ServerEndpoint
import com.migo.core.wire.Id

/**
 * Everything the interface draws, as one immutable value.
 *
 * The screens in this module are functions of this type and nothing else: no screen reads a
 * `MigoClient`, holds a coroutine, or keeps a `remember`ed copy of anything that also lives here.
 * That is the same discipline the desktop client follows (a read-only context plus a command buffer),
 * and it is worth the extra type because the alternative is state that exists in two places -- the
 * ratchet advanced but the bubble still says "sending", or a conversation list that disagrees with
 * the conversation open on top of it.
 *
 * It also means the whole interface can be driven from a test or a preview by constructing a value,
 * with no connection and no key store anywhere.
 */
sealed interface AppState {
    /**
     * The vault is being opened and a stored sign-in resumed.
     *
     * A distinct state rather than a flag on [SignedOut], because showing the sign-in form during it
     * would flash a form at a signed-in user on every cold start.
     */
    data object Starting : AppState

    /** Nobody is signed in on this device, or the last session could not be resumed. */
    data class SignedOut(
        /**
         * The server to talk to, as the structured record the user picked.
         *
         * The form is initialised with the persisted choice (or the dev default on a
         * fresh install) and re-emits a new record on every "Use this server" click
         * through [com.migo.app.AppViewModel.setServerEndpoint]. The form holds the
         * typed text in its own local state, so this field is always a valid
         * [ServerEndpoint] -- a partial host (one that does not satisfy
         * [ServerEndpoint.init]) never reaches here.
         */
        val serverEndpoint: ServerEndpoint,
        /** Username or email, kept across a failed attempt so it does not have to be retyped. */
        val identifier: String = "",
        /** True while a register or sign-in call is in flight; the form is disabled. */
        val busy: Boolean = false,
        /** What went wrong last time, already reduced to something worth showing a person. */
        val failure: String? = null,
    ) : AppState

    /** Signed in. The conversation list is always present; [open] is the chat on top of it. */
    data class SignedIn(
        val username: String,
        val accountId: Id,
        val connection: ConnectionState,
        val conversations: List<ConversationRow> = emptyList(),
        /** True while the first page of conversations is loading. */
        val loading: Boolean = false,
        /** The conversation the user is reading, or null when the list is on top. */
        val open: ChatState? = null,
        /** A transient failure banner: a send that did not go, a page that did not load. */
        val failure: String? = null,
    ) : AppState
}

/** One row of the conversation list. */
data class ConversationRow(
    val conversationId: Id,
    /**
     * What to show as the name.
     *
     * Resolved when the row is built rather than at draw time: a direct conversation has no title of
     * its own and has to borrow the peer's, which is a lookup, and a lookup inside a list item is a
     * lookup that runs on every scroll frame.
     */
    val title: String,
    val kind: ConversationKind,
    /** The last message, as a short line. Null when the conversation has no readable message yet. */
    val preview: String? = null,
    /** `lastSeq - readSeq`, floored at zero. */
    val unread: Long = 0,
    /** Unix milliseconds of the last activity, for ordering and for the timestamp column. */
    val updatedAt: Long = 0,
)

/** One open conversation. */
data class ChatState(
    val conversationId: Id,
    val title: String,
    /** Oldest first: the order they are drawn in, and the order history must be replayed in. */
    val messages: List<ChatMessage> = emptyList(),
    /** True while history is being fetched and decrypted. */
    val loading: Boolean = false,
    /** True from the moment Send is pressed until the server accepts or rejects. */
    val sending: Boolean = false,
    /** Ids of accounts currently typing, other than this one. */
    val typing: Set<Id> = emptySet(),
    /** The text in the composer. Held here so a rotation does not lose a half-written message. */
    val draft: String = "",
)

/**
 * One message bubble.
 *
 * [text] is already the display string. A bubble never holds a [com.migo.core.crypto.Content], so
 * there is no path by which a media key or a control payload reaches a `Text` composable -- the
 * mapping happens once, where the content is decoded.
 */
data class ChatMessage(
    val messageId: Id,
    /** The server sequence number, or 0 for a message this device has sent but not yet had accepted. */
    val seq: Long,
    /** True when this device sent it, which is the only thing that decides which side it sits on. */
    val mine: Boolean,
    /** Who sent it, as a display string. Empty for own messages, which need no label. */
    val author: String,
    val text: String,
    val at: Long,
    /** True for a message sitting in the list before the server has accepted it. */
    val pending: Boolean = false,
    /**
     * True for a body this build cannot render: a content type from a newer peer, or a decode this
     * client refused. The bubble says so rather than showing an empty line, because a message that
     * silently renders as nothing looks like a delivery failure.
     */
    val unsupported: Boolean = false,
)
