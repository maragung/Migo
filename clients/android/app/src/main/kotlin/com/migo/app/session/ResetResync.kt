package com.migo.app.session

import com.migo.core.wire.Id

/**
 * What a fresh-session reset owes the shell, as one run (section 158).
 *
 * The core half is done before the shell ever hears about it: the SDK re-sent every tracked
 * topic subscription, so the live streams are flowing again by the time this runs. What the
 * core cannot do is re-read the application's own surfaces, so the shell owes two things, in
 * this order:
 *
 *  1. **The conversation list re-reads.** The outage may have moved rows -- a conversation
 *     created, a direct chat started, a room left from another device -- and the list is the
 *     one surface every other surface reads from. A reload here is never optional, because a
 *     resumed-looking session with a stale list is indistinguishable from a correct one until
 *     the row the reader taps is not there.
 *  2. **The held conversations catch up from their watermarks, never from the top.** The
 *     conversation on screen goes first -- section 158's "sync visible conversations first",
 *     and the one the reader is watching is the visible one -- then every other conversation
 *     this shell holds a cursor for. A full resync here would wipe-and-refetch a held
 *     conversation -- the exact thing section 158 forbids, because a client that drops and
 *     refetches a whole held conversation is indistinguishable, on the wire, from one that had
 *     nothing. A conversation with no held cursor (nothing delivered this session, no cached
 *     transcript) has no gap to sync: its first open fetches fresh, and the reload above is
 *     the whole debt.
 *
 * The actions are injected so the phone-free suite can pin the shape the way the web suite pins
 * its providers' projections: what is tested is the ordering and the watermark rule, not the
 * network calls themselves.
 */
class ResetResync(
    /** Re-reads the conversation list from the server. */
    private val reloadConversations: () -> Unit,
    /** Fetches a conversation's gap, from the highest sequence this shell already holds. */
    private val catchUpConversation: (conversationId: Id, haveSeq: Long) -> Unit,
) {
    /**
     * Runs the resync for the state the shell is in right now.
     *
     * @param openConversationId the chat on screen, or null when no window is open.
     * @param heldSeq the highest sequence this shell holds for a conversation, or null when it
     * holds nothing -- in which case the open chat's catch-up starts from the top, the same cold
     * open a first visit performs, and a background conversation is skipped entirely.
     * @param conversations the conversation ids the shell is currently listing, in list order.
     * The open chat's own entry is not double-caught-up.
     */
    fun run(
        openConversationId: Id?,
        heldSeq: (Id) -> Long?,
        conversations: () -> List<Id>,
    ) {
        reloadConversations()
        val open = openConversationId
        if (open != null) {
            catchUpConversation(open, heldSeq(open) ?: FROM_THE_TOP)
        }
        for (conversationId in conversations()) {
            if (conversationId == open) continue
            val haveSeq = heldSeq(conversationId) ?: continue
            catchUpConversation(conversationId, haveSeq)
        }
    }

    companion object {
        /** Where a catch-up starts when the shell holds nothing: the conversation's first sequence. */
        const val FROM_THE_TOP = 0L
    }
}
