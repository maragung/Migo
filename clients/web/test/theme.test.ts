/**
 * The theme's persistence and its pre-paint restore.
 *
 * Three rules would silently regress under a "helpful" refactor, so they are pinned here against
 * a writable storage double and a recording `<html>`:
 *
 *   1. **The attribute and the store move together.** `setTheme` is the single write path: the
 *      `<html data-theme>` attribute (what the stylesheet switches on) and the `localStorage`
 *      entry (what the next visit restores) must never disagree, or a reload flips skins.
 *   2. **Dark is the default, and only the two names are themes.** An unset key, a corrupted
 *      string, or a locked-down storage all read as dark — never as some theme this build
 *      cannot name.
 *   3. **The init script restores before paint.** It must read the same key, apply the same
 *      attribute, and fall back to dark — it is the thing that stops a light-theme visitor from
 *      seeing one dark frame on every load.
 *
 * The restore also exists as a file of its own — public/theme-init.js, loaded by the root layout —
 * because the Content-Security-Policy admits no inline script. The last test runs the module's
 * string and that file through the same harness on every path a visitor can take, so the two
 * copies cannot disagree about anything a first paint depends on.
 */

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

import {
  getChoice,
  getTheme,
  resolveChoice,
  setChoice,
  setTheme,
  themeInitScript,
  toggleTheme,
} from '../src/lib/theme.js';

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

/** Installs window/document doubles the theme module can round-trip through. */
function installThemeDom(): {
  restore: () => void;
  store: Storage;
  /** The `data-theme` values written to the fake `<html>`, in order. */
  attributes: string[];
} {
  const store = fakeLocalStorage();
  const attributes: string[] = [];
  const documentElement = {
    setAttribute: (name: string, value: string): void => {
      if (name === 'data-theme') {
        attributes.push(value);
      }
    },
  };
  const fakeDocument = { documentElement };
  const fakeWindow = { localStorage: store };

  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, 'document');
  Object.defineProperty(globalThis, 'window', { configurable: true, value: fakeWindow });
  Object.defineProperty(globalThis, 'document', { configurable: true, value: fakeDocument });

  const restoreOne = (name: string, previous: PropertyDescriptor | undefined): void => {
    if (previous) {
      Object.defineProperty(globalThis, name, previous);
    } else {
      Reflect.deleteProperty(globalThis, name);
    }
  };

  return {
    store,
    attributes,
    restore: () => {
      restoreOne('window', previousWindow);
      restoreOne('document', previousDocument);
    },
  };
}

test('an unset or unreadable store reads as the dark default', () => {
  const dom = installThemeDom();
  try {
    assert.equal(getTheme(), 'dark');

    // Only the two names are themes; anything else a store might hold reads as the default.
    dom.store.setItem('migo:theme', 'blue');
    assert.equal(getTheme(), 'dark', 'a corrupted value must not pose as a theme');
    dom.store.setItem('migo:theme', 'light');
    assert.equal(getTheme(), 'light', 'a stored light theme must be honoured');
  } finally {
    dom.restore();
  }
});

test('setTheme moves the attribute and the store together', () => {
  const dom = installThemeDom();
  try {
    setTheme('light');
    assert.deepEqual(dom.attributes, ['light'], 'the <html> attribute must be set at once');
    assert.equal(
      dom.store.getItem('migo:theme'),
      'light',
      'the choice must persist for next visit',
    );
    assert.equal(getTheme(), 'light');

    setTheme('dark');
    assert.deepEqual(dom.attributes, ['light', 'dark']);
    assert.equal(dom.store.getItem('migo:theme'), 'dark');
  } finally {
    dom.restore();
  }
});

test('toggleTheme flips both ways and reports what it chose', () => {
  const dom = installThemeDom();
  try {
    assert.equal(toggleTheme(), 'light');
    assert.equal(dom.store.getItem('migo:theme'), 'light');
    assert.equal(dom.attributes.at(-1), 'light');

    assert.equal(toggleTheme(), 'dark');
    assert.equal(dom.store.getItem('migo:theme'), 'dark');
    assert.equal(dom.attributes.at(-1), 'dark');
  } finally {
    dom.restore();
  }
});

test('the system choice resolves through the OS scheme and degrades to dark', () => {
  const dom = installThemeDom();
  try {
    // No matchMedia on the double: every path that needs it lands on the shared default.
    assert.equal(resolveChoice('system'), 'dark');
    assert.equal(resolveChoice('light'), 'light');
    assert.equal(resolveChoice('dark'), 'dark');

    dom.store.setItem('migo:theme', 'system');
    assert.equal(getChoice(), 'system');
    assert.equal(getTheme(), 'dark', 'without matchMedia, system resolves dark');
  } finally {
    dom.restore();
  }
});

test('setChoice persists the choice and applies its resolution', () => {
  const dom = installThemeDom();
  try {
    setChoice('system');
    assert.equal(dom.store.getItem('migo:theme'), 'system', 'the choice itself is what persists');
    assert.equal(dom.attributes.at(-1), 'dark', 'without matchMedia, system applies dark');

    setChoice('light');
    assert.equal(dom.store.getItem('migo:theme'), 'light');
    assert.equal(dom.attributes.at(-1), 'light');
  } finally {
    dom.restore();
  }
});

test('the pre-paint init script reads the same key and falls back to dark', () => {
  // The script is a string executed before the bundle loads, so its contract is textual: the
  // storage key and the attribute it writes must be the ones the module itself uses.
  assert.ok(themeInitScript.includes("localStorage.getItem('migo:theme')"));
  assert.ok(themeInitScript.includes("setAttribute('data-theme'"));
  // Both an unknown value and a throwing storage end dark — the default every path shares.
  const fallbacks = themeInitScript.match(/'dark'/g) ?? [];
  assert.ok(fallbacks.length >= 2, 'the script must fall back to dark on unknown and on failure');
});

/**
 * What `localStorage.getItem` answers for one run: a stored value, an absent key, or `{ throws }`
 * for a locked-down embedder where storage itself throws.
 */
type StoredAnswer = string | null | { throws: true };

const STORAGE_REFUSED: StoredAnswer = { throws: true };

/**
 * Runs one init-script source against a minimal `window`/`document` double and returns the theme
 * it applied. `systemLight` is `null` when `matchMedia` is absent.
 */
function runInitScript(source: string, stored: StoredAnswer, systemLight: boolean | null): string {
  const applied: string[] = [];
  const document = {
    documentElement: {
      setAttribute: (name: string, value: string): void => {
        if (name === 'data-theme') {
          applied.push(value);
        }
      },
    },
  };
  const window = {
    localStorage: {
      getItem: (): string | null => {
        if (typeof stored === 'object' && stored !== null) {
          throw new Error('storage refused');
        }
        return stored;
      },
    },
    matchMedia:
      systemLight === null ? undefined : (): { matches: boolean } => ({ matches: systemLight }),
  };
  runInNewContext(source, { window, document });
  assert.equal(applied.length, 1, 'the init script must apply the theme exactly once');
  return applied[0] ?? '(none)';
}

test('the external init file restores the same theme as the module script on every path', async () => {
  const external = await readFile(new URL('../../public/theme-init.js', import.meta.url), 'utf8');
  const paths: Array<{ stored: StoredAnswer; systemLight: boolean | null; expected: string }> = [
    { stored: 'light', systemLight: null, expected: 'light' },
    { stored: 'dark', systemLight: null, expected: 'dark' },
    { stored: 'system', systemLight: true, expected: 'light' },
    { stored: 'system', systemLight: false, expected: 'dark' },
    { stored: 'system', systemLight: null, expected: 'dark' },
    { stored: 'blue', systemLight: null, expected: 'dark' },
    { stored: null, systemLight: null, expected: 'dark' },
    { stored: STORAGE_REFUSED, systemLight: null, expected: 'dark' },
  ];
  for (const { stored, systemLight, expected } of paths) {
    const label =
      typeof stored === 'object' && stored !== null ? 'storage throws' : `stored=${String(stored)}`;
    assert.equal(runInitScript(themeInitScript, stored, systemLight), expected);
    assert.equal(
      runInitScript(external, stored, systemLight),
      expected,
      `the external file must agree with the module script when ${label} and systemLight=${String(systemLight)}`,
    );
  }
});
