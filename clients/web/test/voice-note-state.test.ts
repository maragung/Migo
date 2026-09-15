/**
 * What the receiver-local voice note state is allowed to be (section 179).
 *
 * The two halves of `voice-note-state.ts` — the client-side playback speed and the listened
 * marks — are storage contracts like the navigation mode's, and they are pinned the same way:
 *
 *   1. **The speed.** An unset, corrupted, or future-value key reads as the 1× default, a write
 *      persists under the namespaced key and reads back, the cycle offers exactly 1× → 1.5× →
 *      2×, and a write announces itself so mounted players re-read the store.
 *   2. **The listened marks.** Absent means unlistened (the default for a just-received note),
 *      a mark persists and reads back in both directions, and the payload degrades honestly on
 *      hostile or corrupted input — entries that are not `[id, boolean]` pairs are dropped
 *      rather than thrown, and the bounded store prunes the least recently updated marks.
 *
 * Nothing here is protocol state: there is no receipt, no wire opcode, and no surface the sender
 * reads — which is exactly why every test talks to `localStorage` and the `window` event bus,
 * never to a client or a connection.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import {
  PLAYBACK_SPEEDS,
  applyListenMark,
  formatPlaybackSpeed,
  getVoicePlaybackSpeed,
  isVoiceNoteListened,
  lookupListened,
  markVoiceNoteListened,
  nextPlaybackSpeed,
  parseListenMarks,
  setVoicePlaybackSpeed,
} from '../src/lib/migo/voice-note-state.js';

/** The namespaced keys, stated here so a rename cannot silently orphan a user's stored choice. */
const SPEED_KEY = 'migo:voiceSpeed';
const HEARD_KEY = 'migo:voiceHeard:v1';

/** The window double: a map-backed localStorage and a real EventTarget for the announcement. */
let store: Map<string, string>;
let restoreWindow: () => void;

beforeEach(() => {
  store = new Map<string, string>();
  const target = new EventTarget();
  const win = {
    localStorage: {
      getItem: (key: string): string | null => (store.has(key) ? store.get(key)! : null),
      setItem: (key: string, value: string): void => {
        store.set(key, value);
      },
    },
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    dispatchEvent: target.dispatchEvent.bind(target),
  };
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', { configurable: true, value: win });
  restoreWindow = (): void => {
    if (previous) {
      Object.defineProperty(globalThis, 'window', previous);
    } else {
      Reflect.deleteProperty(globalThis, 'window');
    }
  };
});

afterEach(() => {
  restoreWindow();
});

// --- the speed ---

test('without a window the speed is the 1× default', () => {
  restoreWindow();
  assert.equal(getVoicePlaybackSpeed(), 1);
});

test('an unset key reads as the 1× default', () => {
  assert.equal(getVoicePlaybackSpeed(), 1);
});

test('a write persists under the namespaced key and reads back', () => {
  setVoicePlaybackSpeed(1.5);
  assert.equal(store.get(SPEED_KEY), '1.5');
  assert.equal(getVoicePlaybackSpeed(), 1.5);
  setVoicePlaybackSpeed(2);
  assert.equal(store.get(SPEED_KEY), '2');
  assert.equal(getVoicePlaybackSpeed(), 2);
});

test('a rate this build cannot name reads as the 1× default', () => {
  for (const hostile of ['0.5', '3', 'fast', '', 'null']) {
    store.set(SPEED_KEY, hostile);
    assert.equal(getVoicePlaybackSpeed(), 1, `the stored value ${hostile} must not be a rate`);
  }
});

test('the cycle offers exactly 1× → 1.5× → 2× → 1×', () => {
  assert.equal(nextPlaybackSpeed(1), 1.5);
  assert.equal(nextPlaybackSpeed(1.5), 2);
  assert.equal(nextPlaybackSpeed(2), 1);
  assert.deepEqual(
    [...PLAYBACK_SPEEDS, ...PLAYBACK_SPEEDS].map(nextPlaybackSpeed),
    [1.5, 2, 1, 1.5, 2, 1],
  );
});

test('a rate labels as itself, compactly', () => {
  assert.equal(formatPlaybackSpeed(1), '1×');
  assert.equal(formatPlaybackSpeed(1.5), '1.5×');
  assert.equal(formatPlaybackSpeed(2), '2×');
});

test('a speed write announces itself on the window bus', () => {
  const seen: number[] = [];
  const onChange = (): void => {
    seen.push(getVoicePlaybackSpeed());
  };
  window.addEventListener('migo:voicespeed', onChange);
  setVoicePlaybackSpeed(2);
  window.removeEventListener('migo:voicespeed', onChange);
  assert.deepEqual(seen, [2]);
});

// --- the listened marks ---

test('without a window every note reads as unlistened and a mark is a no-op', () => {
  restoreWindow();
  assert.equal(isVoiceNoteListened('msg_1'), false);
  markVoiceNoteListened('msg_1', true);
  assert.equal(isVoiceNoteListened('msg_1'), false);
});

test('absent means unlistened — the default for a just-received note', () => {
  assert.equal(isVoiceNoteListened('msg_never_touched'), false);
});

test('a mark persists under the namespaced key and reads back, in both directions', () => {
  markVoiceNoteListened('msg_1', true);
  assert.equal(isVoiceNoteListened('msg_1'), true);
  assert.equal(isVoiceNoteListened('msg_2'), false);
  // Unlistened is a real state, not the absence of one: an explicit false overwrites a true.
  markVoiceNoteListened('msg_1', false);
  assert.equal(isVoiceNoteListened('msg_1'), false);
});

test('a mark write announces itself on the window bus', () => {
  const seen: boolean[] = [];
  const onChange = (): void => {
    seen.push(isVoiceNoteListened('msg_1'));
  };
  window.addEventListener('migo:voiceheard', onChange);
  markVoiceNoteListened('msg_1', true);
  window.removeEventListener('migo:voiceheard', onChange);
  assert.deepEqual(seen, [true]);
});

test('a corrupted payload degrades to an empty store, never a throw', () => {
  for (const hostile of ['{not json', '"a string"', '42', 'null']) {
    store.set(HEARD_KEY, hostile);
    assert.equal(isVoiceNoteListened('msg_1'), false, `${hostile} must read as unlistened`);
  }
});

test('entries that are not [id, boolean] pairs are dropped, not trusted', () => {
  store.set(
    HEARD_KEY,
    JSON.stringify([
      ['msg_ok', true],
      ['msg_bad_type', 'yes'],
      [7, true],
      ['', true],
      ['msg_extra', true, 'surprise'],
      'not-a-pair',
      null,
    ]),
  );
  assert.equal(isVoiceNoteListened('msg_ok'), true);
  for (const id of ['msg_bad_type', 'msg_extra']) {
    assert.equal(isVoiceNoteListened(id), false, `${id} must not survive the parse`);
  }
});

test('a duplicate id keeps the first occurrence — the writer never produces them', () => {
  const marks = parseListenMarks(
    JSON.stringify([
      ['msg_1', true],
      ['msg_1', false],
    ]),
  );
  assert.deepEqual(marks, [['msg_1', true]]);
  assert.equal(lookupListened(marks, 'msg_1'), true);
});

test('an updated mark moves to the end; the store is in last-updated order', () => {
  let marks = applyListenMark([], 'msg_1', true);
  marks = applyListenMark(marks, 'msg_2', true);
  marks = applyListenMark(marks, 'msg_1', false);
  assert.deepEqual(marks, [
    ['msg_2', true],
    ['msg_1', false],
  ]);
});

test('the bounded store prunes the least recently updated marks from the front', () => {
  let marks: ReturnType<typeof applyListenMark> = [];
  for (let i = 0; i < 2_500; i += 1) {
    marks = applyListenMark(marks, `msg_${i}`, true);
  }
  assert.equal(marks.length, 2_000);
  // The oldest 500 are gone; the newest and everything after index 500 survive.
  assert.equal(lookupListened(marks, 'msg_0'), false);
  assert.equal(lookupListened(marks, 'msg_499'), false);
  assert.equal(lookupListened(marks, 'msg_500'), true);
  assert.equal(lookupListened(marks, 'msg_2499'), true);
});
