/**
 * What the navigation mode is allowed to be.
 *
 * The mode is the one setting that reroutes the whole conversation surface (Settings →
 * Navigation, Chat List Mode), so its storage contract needs pinning the same way the theme's
 * is: an unset or corrupted key reads as the tabbed default — the layout the client has always
 * had, and the one an account that never opens Settings must keep seeing — a write persists
 * under the namespaced key and announces itself so a mounted shell obeys mid-session, and a
 * storage that refuses the write (a full or blocked `localStorage`) still announces, though
 * nothing persisted means the announced re-read reports the previous choice.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import { getNavMode, setNavMode, subscribeNavMode } from '../src/lib/migo/nav-mode.js';

/** The namespaced key the choice persists under, stated here so a rename cannot silently orphan old choices. */
const STORAGE_KEY = 'migo:navMode';

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

test('without a window the choice is the tabbed default', () => {
  restoreWindow();
  assert.equal(getNavMode(), 'tabbed');
});

test('an unset key reads as the tabbed default', () => {
  assert.equal(getNavMode(), 'tabbed');
});

test('a write persists under the namespaced key and reads back', () => {
  setNavMode('chatlist');
  assert.equal(store.get(STORAGE_KEY), 'chatlist');
  assert.equal(getNavMode(), 'chatlist');
  setNavMode('tabbed');
  assert.equal(store.get(STORAGE_KEY), 'tabbed');
  assert.equal(getNavMode(), 'tabbed');
});

test('a value this build cannot name reads as the tabbed default', () => {
  store.set(STORAGE_KEY, 'sidebars');
  assert.equal(getNavMode(), 'tabbed');
  store.set(STORAGE_KEY, '');
  assert.equal(getNavMode(), 'tabbed');
});

test('a write announces itself, and the subscription re-reads the store', () => {
  const seen: string[] = [];
  const off = subscribeNavMode(() => {
    seen.push(getNavMode());
  });
  setNavMode('chatlist');
  assert.deepEqual(seen, ['chatlist']);
  setNavMode('tabbed');
  assert.deepEqual(seen, ['chatlist', 'tabbed']);
  off();
  setNavMode('chatlist');
  assert.deepEqual(seen, ['chatlist', 'tabbed']);
});

test('a storage that refuses the write still announces the switch', () => {
  const win = globalThis.window as unknown as { localStorage: Storage };
  Object.defineProperty(win, 'localStorage', {
    configurable: true,
    value: {
      getItem: (): string | null => null,
      setItem: (): void => {
        throw new Error('storage is full');
      },
    },
  });
  let notified = 0;
  const off = subscribeNavMode(() => {
    notified += 1;
  });
  setNavMode('chatlist');
  assert.equal(notified, 1);
  // Nothing persisted, so the announced re-read reports the previous choice — the session keeps
  // the layout it had, which is the honest answer when the store refused the write.
  assert.equal(getNavMode(), 'tabbed');
  off();
});

test('a subscription without a window is a no-op that still unsubscribes', () => {
  restoreWindow();
  const off = subscribeNavMode(() => {
    assert.fail('a windowless subscription must never fire');
  });
  off();
});
