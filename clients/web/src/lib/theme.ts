/**
 * The light/dark theme, owned by the `<html>` element's `data-theme` attribute.
 *
 * The stylesheet carries two variable sets — `:root` for light, `[data-theme="dark"]` for dark —
 * so the attribute is the single switch: setting it re-skins everything, no class juggling on
 * individual components. The choice persists in `localStorage` under {@link STORAGE_KEY}; a plain
 * string, never key material, so the audit rule that keeps secrets out of `localStorage` is not
 * touched by this.
 *
 * Dark is the default when nothing is stored — the client shipped dark-only first, so an existing
 * visitor's look must not change until they ask for the light one.
 */

/** The two appearances the client ships. */
export type Theme = 'light' | 'dark';

/** Where the choice persists; namespaced like the rest of the client's local state. */
const STORAGE_KEY = 'migo:theme';

/** The attribute the stylesheet switches on: `<html data-theme="…">`. */
const THEME_ATTRIBUTE = 'data-theme';

/**
 * The theme this browser last chose, or the dark default.
 *
 * Anything that is not the literal `'light'` — an unset key, a value written by a future build, a
 * corrupted string — reads as dark rather than as some theme this build cannot name. Access to
 * `localStorage` can itself throw in locked-down embedders; that too reads as the default instead
 * of taking the toggle down.
 */
export function getTheme(): Theme {
  if (typeof window === 'undefined') {
    return 'dark';
  }
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    return stored === 'light' || stored === 'dark' ? stored : 'dark';
  } catch {
    return 'dark';
  }
}

/**
 * Applies one theme: the attribute now, the persistence behind it.
 *
 * The attribute is written before the store so a blocked or full `localStorage` (private windows)
 * still re-skins the running session — only the next visit falls back to the default.
 */
export function setTheme(theme: Theme): void {
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    return;
  }
  document.documentElement.setAttribute(THEME_ATTRIBUTE, theme);
  try {
    window.localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // Best-effort: the attribute is already set, so this session keeps the chosen theme.
  }
}

/** Flips to the other theme, applies it, persists it, and returns what it chose. */
export function toggleTheme(): Theme {
  const next: Theme = getTheme() === 'dark' ? 'light' : 'dark';
  setTheme(next);
  return next;
}

/**
 * The pre-paint theme restore, run as an inline script at the top of `<body>` in the root layout.
 *
 * React renders the server HTML with the dark default already on `<html>`, but a returning
 * light-theme visitor would still see one dark frame before hydration applies the stored choice —
 * a flash of the wrong theme. This script closes that gap: it runs synchronously during parse,
 * before anything below it is painted, so the very first frame already carries the right skin. It
 * is a string, not an import of this module, because it must execute before the bundle loads;
 * the same key and default are spelled out in it on purpose.
 */
export const themeInitScript = `
(function () {
  try {
    var theme = window.localStorage.getItem('${STORAGE_KEY}');
    if (theme !== 'light' && theme !== 'dark') {
      theme = 'dark';
    }
    document.documentElement.setAttribute('${THEME_ATTRIBUTE}', theme);
  } catch (error) {
    document.documentElement.setAttribute('${THEME_ATTRIBUTE}', 'dark');
  }
})();
`;
