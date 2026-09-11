package com.migo.core.domain

/**
 * The in-thread message search, as pure functions — the same contract the web client's chat window
 * already ships, so all three clients filter a conversation the same way.
 *
 * The search is a filter over the messages this device has already decrypted and holds, not a
 * server query: nothing leaves the device, and the honest scope is "loaded messages" — the field's
 * own placeholder says so rather than promising the whole history. Matching is a case-insensitive
 * substring on TEXT bodies only: a media message's caption and a voice note's label are not
 * text bodies, so they never match, exactly as the web client's `ContentType.Text` check does.
 */

/**
 * Whether one message body matches the live query.
 *
 * Blank — empty or whitespace-only — matches nothing, because the web client skips the filter
 * entirely while the trimmed query is empty; a matcher that returned true there would turn
 * "the field is empty" into "everything matches", which the caller's skip already means and
 * which this function never gets to decide. Case-insensitive by lowercasing both sides, the
 * locale-blind `lowercase()` the web client's `toLowerCase()` approximates, so a query typed
 * without regard to case finds what was sent without regard to case.
 */
fun chatSearchMatches(query: String, text: String): Boolean {
    val needle = query.trim().lowercase()
    if (needle.isEmpty()) return false
    return text.lowercase().contains(needle)
}

/**
 * The conversation's messages as the search would show them.
 *
 * A blank query returns the original list untouched — same reference, so the caller's `remember`
 * keys stay stable and the thread draws exactly as it did before the field was opened. Otherwise
 * the matching subset, in the thread's own order, because a result list that reordered messages
 * would answer a different question than "which of these say it".
 *
 * [textOf] is the text-body rule as a function: it returns the matchable text for one message,
 * or null when the message has no text body (an attachment, or a body this build cannot render)
 * — and null never matches, so media captions and voice labels stay unfindable here as they are
 * on the web.
 */
fun <T> filterChatSearch(messages: List<T>, query: String, textOf: (T) -> String?): List<T> {
    if (query.trim().isEmpty()) return messages
    return messages.filter { message -> textOf(message)?.let { chatSearchMatches(query, it) } == true }
}
