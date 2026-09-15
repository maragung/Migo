'use client';

/**
 * The receiver-local half of a voice note: how fast it plays, and whether this receiver has
 * listened to it (section 179).
 *
 * Both halves are deliberately *not* protocol state. The playback speeds are the client-side
 * rates section 167 names — applied to an `Audio` element already holding the decrypted bytes, so
 * a switch never re-fetches media — and the listened marks are the "keadaan lokal penerima": the
 * receiver's own memory of which notes they have heard. Nothing here crosses the wire, nothing
 * here is a receipt, and marking a note unlistened cancels no receipt that was already sent —
 * `ReceiptKind` has no played variant and gains none, so the sender's view of these marks is and
 * stays nothing. The only surfaces that read this state are the receiver's own player and the
 * receiver's own transcript rows.
 *
 * # Persistence
 *
 * Both choices persist in `localStorage` the way the navigation mode does (`nav-mode.ts`): a
 * namespaced `migo:` key holding a plain string or JSON — never key material, so the audit rule
 * that keeps secrets out of `localStorage` is not touched — written first and then announced with
 * a `window` event, because the writer (a bubble, a hover action) and the readers (other bubbles,
 * other rows) are different components. Readers re-read the store on the announcement rather than
 * trusting the event, so a refused write in a locked-down embedder honestly reads back as the
 * previous value. Access to `localStorage` can itself throw; every read falls back to the default
 * (1×, unlistened), so a private window simply loses the memory, never the player.
 *
 * The speed is one choice per user — one browser profile, every conversation — because a listener
 * who prefers 2× wants it everywhere; per-conversation memory would be a setting to forget
 * rather than a preference.
 *
 * The listened marks are a list of `[messageId, listened]` pairs in last-updated order, capped at
 * {@link MAX_TRACKED}. Absent means unlistened, which is the default for a received note, so the
 * store only ever grows by explicit marks (a play that reached the threshold, a hand toggle). The
 * cap prunes from the front — the least recently updated marks — which in the worst case lets an
 * ancient note read as unheard again; that is the honest failure of a bounded local store, and
 * two thousand marks are years of listening.
 */

import { useCallback, useEffect, useState } from 'react';

/** The three client-side playback rates section 167 allows, in cycle order. */
export const PLAYBACK_SPEEDS: readonly [1, 1.5, 2] = [1, 1.5, 2];

/** One of the three allowed rates. */
export type PlaybackSpeed = (typeof PLAYBACK_SPEEDS)[number];

/** Where the speed choice persists; namespaced like the rest of the client's local state. */
const SPEED_KEY = 'migo:voiceSpeed';

/** The event a speed write dispatches on `window`, so mounted players re-read the store. */
const SPEED_EVENT = 'migo:voicespeed';

/** Where the listened marks persist; versioned because the payload is JSON, not a plain string. */
const HEARD_KEY = 'migo:voiceHeard:v1';

/** The event a listened write dispatches on `window`, so mounted rows and bubbles re-read. */
const HEARD_EVENT = 'migo:voiceheard';

/** How many listened marks the store keeps before the least recently updated are dropped. */
const MAX_TRACKED = 2_000;

/**
 * How much of a note must play before it counts as listened: 90% of the duration or the end
 * event itself (whichever comes first). Near-the-end is the threshold the brief asks for rather
 * than strict completion, so a listener who skips the last second of trailing silence still gets
 * the mark — and a hand-toggled "unlistened" stays unlistened until the note is played again.
 */
export const LISTENED_THRESHOLD = 0.9;

// --- the speed ---

/**
 * The stored speed this browser last chose, or the 1× default.
 *
 * Anything that is not one of the three rates — an unset key, a value written by a future build,
 * a corrupted string — reads as 1× rather than as a rate this build cannot name.
 */
export function getVoicePlaybackSpeed(): PlaybackSpeed {
  if (typeof window === 'undefined') {
    return 1;
  }
  try {
    const stored = window.localStorage.getItem(SPEED_KEY);
    const value = stored === null ? NaN : Number(stored);
    return PLAYBACK_SPEEDS.includes(value as PlaybackSpeed) ? (value as PlaybackSpeed) : 1;
  } catch {
    return 1;
  }
}

/** Applies one speed choice: persisted first, then announced to every mounted reader. */
export function setVoicePlaybackSpeed(speed: PlaybackSpeed): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.localStorage.setItem(SPEED_KEY, String(speed));
  } catch {
    // A refused write is announced like any other: the re-read then reports the previous rate,
    // which is the honest answer when nothing persisted.
  }
  window.dispatchEvent(new Event(SPEED_EVENT));
}

/** The next rate in the 1× → 1.5× → 2× → 1× cycle. */
export function nextPlaybackSpeed(speed: PlaybackSpeed): PlaybackSpeed {
  const index = PLAYBACK_SPEEDS.indexOf(speed);
  return PLAYBACK_SPEEDS[(index + 1) % PLAYBACK_SPEEDS.length] ?? 1;
}

/** A rate as the compact button label: `1×`, `1.5×`, `2×`. */
export function formatPlaybackSpeed(speed: PlaybackSpeed): string {
  return `${speed}×`;
}

/**
 * The playback speed as React state: the stored choice, and a setter that persists it.
 *
 * Players apply the stored rate when they build their `Audio` element and again whenever this
 * state changes under them — setting `playbackRate` on a live element keeps `currentTime`, so a
 * mid-playback switch is instantaneous and position-true with no second fetch of the bytes.
 */
export function useVoicePlaybackSpeed(): readonly [PlaybackSpeed, (speed: PlaybackSpeed) => void] {
  const [speed, setSpeed] = useState<PlaybackSpeed>(getVoicePlaybackSpeed);

  useEffect(() => {
    const resync = (): void => setSpeed(getVoicePlaybackSpeed());
    window.addEventListener(SPEED_EVENT, resync);
    return () => {
      window.removeEventListener(SPEED_EVENT, resync);
    };
  }, []);

  const pick = useCallback((next: PlaybackSpeed): void => {
    setVoicePlaybackSpeed(next);
  }, []);

  return [speed, pick] as const;
}

// --- the listened marks ---

/** One persisted mark: which message, and whether this receiver has listened to it. */
export type ListenMark = readonly [messageId: string, listened: boolean];

/**
 * Parses the persisted marks, degrading to none rather than throwing.
 *
 * The payload is this receiver's own past writes, but a corrupted or hand-edited value must not
 * take the transcript down: entries that are not a `[string, boolean]` pair are dropped, and a
 * value that is not an array at all reads as an empty store (every note unlistened — the default,
 * so the failure is a lost memory, not a broken player). Later duplicates of one message id are
 * ignored in favour of the first, because the writer never produces them.
 */
export function parseListenMarks(raw: string | null): ListenMark[] {
  if (raw === null) {
    return [];
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) {
    return [];
  }
  const seen = new Set<string>();
  const marks: ListenMark[] = [];
  for (const entry of parsed) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      continue;
    }
    const [id, listened] = entry as [unknown, unknown];
    if (typeof id !== 'string' || id.length === 0 || typeof listened !== 'boolean') {
      continue;
    }
    if (seen.has(id)) {
      continue;
    }
    seen.add(id);
    marks.push([id, listened] as const);
  }
  return marks;
}

/**
 * Reads one message's mark out of a parsed store: `true` when listened, `false` when marked
 * unlistened or never marked at all. Absent is the unlistened default, so a note this receiver
 * has not touched yet — the common case for a just-received note — reads as unheard.
 */
export function lookupListened(marks: readonly ListenMark[], messageId: string): boolean {
  for (const [id, listened] of marks) {
    if (id === messageId) {
      return listened;
    }
  }
  return false;
}

/**
 * Applies one mark to a store: an existing entry for the message moves to the end (the list is in
 * last-updated order), a new one is appended, and the store is capped at {@link MAX_TRACKED} by
 * dropping the least recently updated entries from the front.
 */
export function applyListenMark(
  marks: readonly ListenMark[],
  messageId: string,
  listened: boolean,
): ListenMark[] {
  const kept = marks.filter(([id]) => id !== messageId);
  kept.push([messageId, listened] as const);
  return kept.length > MAX_TRACKED ? kept.slice(kept.length - MAX_TRACKED) : kept;
}

/** The persisted marks, or none when the store is unreadable. */
function readMarks(): ListenMark[] {
  if (typeof window === 'undefined') {
    return [];
  }
  try {
    return parseListenMarks(window.localStorage.getItem(HEARD_KEY));
  } catch {
    return [];
  }
}

/**
 * Whether this receiver has listened to one voice note. Receiver-local only: the sender has no
 * way to ask, and this function is never consulted for anything the sender sees.
 */
export function isVoiceNoteListened(messageId: string): boolean {
  return lookupListened(readMarks(), messageId);
}

/** Marks one voice note listened or unlistened: persisted first, then announced to readers. */
export function markVoiceNoteListened(messageId: string, listened: boolean): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.localStorage.setItem(
      HEARD_KEY,
      JSON.stringify(applyListenMark(readMarks(), messageId, listened)),
    );
  } catch {
    // As with the speed: a refused write is announced anyway, and the re-read honestly reports
    // the previous mark. The session keeps working; only the next visit loses the change.
  }
  window.dispatchEvent(new Event(HEARD_EVENT));
}

/**
 * One voice note's listened state as React state, for a note this receiver tracks.
 *
 * Returns `null` when `messageId` is absent — the caller's signal that this note is not a
 * trackable received note (our own recording, say) and should render with neither the heard
 * indicator nor the row toggle. The setter persists the flip; the returned state re-reads the
 * store on every announcement, so a play that reaches the threshold and a hover-action toggle
 * made elsewhere in the same window agree.
 */
export function useVoiceNoteListened(
  messageId: string | undefined,
): readonly [boolean, (listened: boolean) => void] | null {
  const [listened, setListenedState] = useState(() =>
    messageId === undefined ? false : isVoiceNoteListened(messageId),
  );

  useEffect(() => {
    if (messageId === undefined) {
      return;
    }
    const resync = (): void => setListenedState(isVoiceNoteListened(messageId));
    resync();
    window.addEventListener(HEARD_EVENT, resync);
    return () => {
      window.removeEventListener(HEARD_EVENT, resync);
    };
  }, [messageId]);

  const mark = useCallback(
    (next: boolean): void => {
      if (messageId !== undefined) {
        markVoiceNoteListened(messageId, next);
      }
    },
    [messageId],
  );

  return messageId === undefined ? null : ([listened, mark] as const);
}
