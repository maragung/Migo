/**
 * How the chats display choice persists, and what it reads as when it cannot.
 *
 * The choice is a fact about a person, not a session, so it lives in `localStorage` as a plain
 * string — never key material, so the audit rule that keeps secrets out of localStorage is not
 * touched by it. Three rules are pinned against a writable storage double:
 *
 *   1. **Right tabs is the default, and only the two names are modes.** An unset key, a
 *      corrupted string, or a locked-down storage all read as `'right'` — never as some mode
 *      this build cannot honour.
 *   2. **A stored choice round-trips.** What `setChatTabsMode` writes is what the next read
 *      returns, for both modes.
 *   3. **A failed write costs a default next session, never a wrong screen now.** The setter
 *      swallows a throwing storage rather than surfacing an error mid-render.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { getChatTabsMode, setChatTabsMode } from '../src/lib/chat-tabs-mode.js';

/** A writable `localStorage` double backed by a plain map. */
function fakeLocalStorage(): Storage {
  const store = new Map<string, string>();
  return {
    getItem: (key: string) => (store.has(key) ? (store.get(key) as string) : null),
    setItem: (key: string, value: string) => void store.set(key, value),
    removeItem: (key: string) => void store.delete(key),
    clear: () => void store.clear(),
    key: () => null,
    get length() {
      return store.size;
    },
  };
}

/** Installs a `window` double the mode module can round-trip through. */
function installModeDom(store: Storage): () => void {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { localStorage: store },
  });
  return () => {
    if (previousWindow) {
      Object.defineProperty(globalThis, 'window', previousWindow);
    } else {
      Reflect.deleteProperty(globalThis, 'window');
    }
  };
}

test('an unset or unreadable store reads as the right-tabs default', () => {
  const restore = installModeDom(fakeLocalStorage());
  try {
    assert.equal(getChatTabsMode(), 'right', 'a first visit must get the right-tabs default');

    // Only the two names are modes; anything else a store might hold reads as the default.
    window.localStorage.setItem('migo:chat-tabs-mode', 'left');
    assert.equal(getChatTabsMode(), 'right', 'a corrupted value must not pose as a mode');
    window.localStorage.setItem('migo:chat-tabs-mode', 'list');
    assert.equal(getChatTabsMode(), 'list', 'a stored list choice must be honoured');
  } finally {
    restore();
  }
});

test('a stored choice round-trips for both modes', () => {
  const restore = installModeDom(fakeLocalStorage());
  try {
    setChatTabsMode('list');
    assert.equal(
      window.localStorage.getItem('migo:chat-tabs-mode'),
      'list',
      'the choice must persist for the next session',
    );
    assert.equal(getChatTabsMode(), 'list');

    setChatTabsMode('right');
    assert.equal(window.localStorage.getItem('migo:chat-tabs-mode'), 'right');
    assert.equal(getChatTabsMode(), 'right');
  } finally {
    restore();
  }
});

test('a locked-down storage reads as the default and swallows its writes', () => {
  // A storage whose every access throws — the locked-down embedder's whole offer.
  const refusing: Storage = {
    getItem: () => {
      throw new Error('denied');
    },
    setItem: () => {
      throw new Error('denied');
    },
    removeItem: () => {
      throw new Error('denied');
    },
    clear: () => {
      throw new Error('denied');
    },
    key: () => null,
    length: 0,
  };
  const restore = installModeDom(refusing);
  try {
    assert.equal(getChatTabsMode(), 'right', 'a refused read must read as the default');
    // A refused write costs the default next session — it must not become an error now.
    setChatTabsMode('list');
  } finally {
    restore();
  }
});
