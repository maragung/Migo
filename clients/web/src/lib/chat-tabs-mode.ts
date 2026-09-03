/**
 * How chats open, owned by a plain localStorage string.
 *
 * The shell has two ways to hold a conversation, and which one a person prefers is a fact about
 * them, not about a session: {@link ChatTabsMode.RightTabs} docks every open chat as a closable
 * tab in the right pane (the default, and the client's only mode for its first year, so a stored
 * nothing must read as it); {@link ChatTabsMode.List} drops the right-pane chat tabs, keeps the
 * Chats list among the side tabs, and opens a chat as one full window at a time — back returns to
 * the list.
 *
 * The choice persists under {@link STORAGE_KEY} as a plain string, never key material, so the
 * audit rule that keeps secrets out of localStorage is not touched by this. A value this build
 * cannot name — unset, written by a future build, corrupted — reads as the default rather than
 * as some mode this build cannot honour.
 */

/** The two ways the shell can hold an open chat. */
export type ChatTabsMode = 'right' | 'list';

/** Where the choice persists; namespaced like the rest of the client's local state. */
const STORAGE_KEY = 'migo:chat-tabs-mode';

/**
 * The choice this browser last made, or the right-tabs default.
 *
 * Access to `localStorage` can itself throw in locked-down embedders; that too reads as the
 * default.
 */
export function getChatTabsMode(): ChatTabsMode {
  if (typeof window === 'undefined') {
    return 'right';
  }
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    return stored === 'right' || stored === 'list' ? stored : 'right';
  } catch {
    return 'right';
  }
}

/** Persists a choice for the next session; a failed write costs a default next time, never a wrong screen now. */
export function setChatTabsMode(mode: ChatTabsMode): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // A locked-down embedder keeps the choice for this session only.
  }
}
