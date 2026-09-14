'use client';

/**
 * The navigation mode: how the client presents its conversations.
 *
 * `tabbed` is the layout the client has always had — every conversation a window of its own on
 * the desk, a full-screen thread on the phone — and it is the default, so an account that never
 * opens Settings sees nothing change. `chatlist` keeps every list and panel exactly where it is
 * and changes only the conversation route: the phone's home gains a Main tab that holds the
 * conversation list, and the desk replaces the per-conversation windows with one split view —
 * the list on the left, a single thread pane on the right.
 *
 * The choice persists in `localStorage` under {@link STORAGE_KEY} — a plain string, never key
 * material, so the audit rule that keeps secrets out of `localStorage` is not touched. Like the
 * theme, the choice is read at mount and re-read on a custom event, because the settings panel
 * that writes it and the shell that obeys it are different components in different windows: a
 * dispatched event is the one bus both can see without the shell threading a callback through
 * every panel that might someday hold the control.
 */

import { useCallback, useEffect, useState } from 'react';

/** The two layouts the client ships. */
export type NavMode = 'tabbed' | 'chatlist';

/** Where the choice persists; namespaced like the rest of the client's local state. */
const STORAGE_KEY = 'migo:navMode';

/** The event a write dispatches on `window`, so mounted readers re-read the store. */
const CHANGE_EVENT = 'migo:navmode';

/**
 * The choice this browser last made, or the tabbed default.
 *
 * Anything that is not one of the two names — an unset key, a value written by a future build,
 * a corrupted string — reads as tabbed rather than as some choice this build cannot name.
 * Access to `localStorage` can itself throw in locked-down embedders; that too reads as the
 * default, so a private window simply keeps the classic layout.
 */
export function getNavMode(): NavMode {
  if (typeof window === 'undefined') {
    return 'tabbed';
  }
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    return stored === 'chatlist' ? 'chatlist' : 'tabbed';
  } catch {
    return 'tabbed';
  }
}

/**
 * Applies one choice: persisted first, then announced to every mounted reader.
 *
 * Readers re-read the store on the announcement, so persistence is the whole truth: if the write
 * is refused (a full or blocked `localStorage`), the announced read is still the previous choice
 * and the session keeps the layout it had — honest, and only the next visit is affected anyway.
 */
export function setNavMode(mode: NavMode): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // A refused write is announced like any other: the re-read then reports the previous
    // choice, which is the honest answer when nothing persisted.
  }
  // A plain Event, not a CustomEvent carrying the mode: listeners re-read the store (see
  // subscribeNavMode), so the announcement needs no payload a synthetic dispatch could fake.
  window.dispatchEvent(new Event(CHANGE_EVENT));
}

/**
 * Subscribes to later writes. Returns the unwatch function.
 *
 * Listeners re-read the store rather than trusting the event's detail, so a stale or synthetic
 * event cannot paint a mode the store does not hold.
 */
export function subscribeNavMode(onChange: () => void): () => void {
  if (typeof window === 'undefined') {
    return () => {};
  }
  window.addEventListener(CHANGE_EVENT, onChange);
  return () => {
    window.removeEventListener(CHANGE_EVENT, onChange);
  };
}

/**
 * The navigation mode as React state: the stored choice, and a setter that persists it.
 *
 * The initial read happens in the state initializer, so the very first render after mount already
 * carries the stored choice (the shell renders nothing before its mount guard lifts anyway);
 * later writes arrive through the subscription, whichever window made them.
 */
export function useNavMode(): readonly [NavMode, (mode: NavMode) => void] {
  const [mode, setMode] = useState<NavMode>(getNavMode);

  useEffect(() => subscribeNavMode(() => setMode(getNavMode())), []);

  const pick = useCallback((next: NavMode): void => {
    setNavMode(next);
  }, []);

  return [mode, pick] as const;
}
