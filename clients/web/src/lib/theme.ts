/**
 * The theme, owned by the `<html>` element's `data-theme` attribute.
 *
 * The stylesheet carries two variable sets — `:root` for light, `[data-theme="dark"]` for dark —
 * so the attribute is the single switch: setting it re-skins everything, no class juggling on
 * individual components. The choice persists in `localStorage` under {@link STORAGE_KEY}; a plain
 * string, never key material, so the audit rule that keeps secrets out of `localStorage` is not
 * touched by this.
 *
 * The choice is one of light, dark, or system. System is a *choice*, not a theme: the attribute
 * still only ever carries light or dark, resolved from the OS's `prefers-color-scheme` at apply
 * time and re-resolved whenever the OS flips (a `system` listener in the app shell re-applies
 * it), so a room's lighting can change the skin mid-session without a reload.
 *
 * Dark is the default when nothing is stored — the client shipped dark-only first, so an
 * existing visitor's look must not change until they ask for another choice.
 */

/** The two appearances the client ships. */
export type Theme = 'light' | 'dark';

/** The three choices the settings offer; `system` resolves to a {@link Theme} at apply time. */
export type ThemeChoice = Theme | 'system';

/** Where the choice persists; namespaced like the rest of the client's local state. */
const STORAGE_KEY = 'migo:theme';

/** The attribute the stylesheet switches on: `<html data-theme="…">`. */
const THEME_ATTRIBUTE = 'data-theme';

/** The media query whose match (or absence) resolves the `system` choice. */
const SYSTEM_QUERY = '(prefers-color-scheme: light)';

/**
 * The choice this browser last made, or the dark default.
 *
 * Anything that is not one of the three names — an unset key, a value written by a future build,
 * a corrupted string — reads as dark rather than as some choice this build cannot name. Access
 * to `localStorage` can itself throw in locked-down embedders; that too reads as the default.
 */
export function getChoice(): ThemeChoice {
  if (typeof window === 'undefined') {
    return 'dark';
  }
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    return stored === 'light' || stored === 'dark' || stored === 'system' ? stored : 'dark';
  } catch {
    return 'dark';
  }
}

/**
 * Resolves a choice to the theme it paints right now.
 *
 * `system` asks the OS through `prefers-color-scheme`; a window without the media query (an
 * embedder, a test) or a query that throws resolves dark — the default every path shares.
 */
export function resolveChoice(choice: ThemeChoice): Theme {
  if (choice === 'light' || choice === 'dark') {
    return choice;
  }
  try {
    if (typeof window !== 'undefined' && window.matchMedia !== undefined) {
      return window.matchMedia(SYSTEM_QUERY).matches ? 'light' : 'dark';
    }
  } catch {
    // Fall through to the shared default.
  }
  return 'dark';
}

/**
 * The theme the attribute currently carries — the choice, resolved.
 *
 * Kept for the surfaces that only ever deal in themes (the toggle's icon); the settings page
 * deals in choices.
 */
export function getTheme(): Theme {
  return resolveChoice(getChoice());
}

/**
 * Applies one choice: the resolved attribute now, the choice itself persisted behind it.
 *
 * The attribute is written before the store so a blocked or full `localStorage` (private windows)
 * still re-skins the running session — only the next visit falls back to the default.
 */
export function setChoice(choice: ThemeChoice): void {
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    return;
  }
  document.documentElement.setAttribute(THEME_ATTRIBUTE, resolveChoice(choice));
  try {
    window.localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    // Best-effort: the attribute is already set, so this session keeps the chosen theme.
  }
}

/** Applies one theme directly — the toggle's path, persisting the theme as the choice. */
export function setTheme(theme: Theme): void {
  setChoice(theme);
}

/**
 * Watches the OS scheme and re-applies the current choice whenever it moves.
 *
 * Returns the unwatch function. The listener asks cheaply (one media-query listener while
 * mounted) and re-resolves the *stored* choice each time, so an explicit light/dark choice is
 * untouched by the OS and only a `system` choice follows it.
 */
export function watchSystemTheme(onChange: (theme: Theme) => void): () => void {
  if (typeof window === 'undefined' || window.matchMedia === undefined) {
    return () => {};
  }
  let query: MediaQueryList;
  try {
    query = window.matchMedia(SYSTEM_QUERY);
  } catch {
    return () => {};
  }
  const handler = (): void => {
    onChange(resolveChoice(getChoice()));
  };
  query.addEventListener('change', handler);
  return () => {
    query.removeEventListener('change', handler);
  };
}

/** Flips between the two themes, applies it, persists it, and returns what it chose. */
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
 * the same key, query, and default are spelled out in it on purpose.
 */
export const themeInitScript = `
(function () {
  function resolve(choice) {
    if (choice === 'light' || choice === 'dark') {
      return choice;
    }
    try {
      if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) {
        return 'light';
      }
    } catch (error) {}
    return 'dark';
  }
  try {
    var choice = window.localStorage.getItem('${STORAGE_KEY}');
    if (choice !== 'light' && choice !== 'dark' && choice !== 'system') {
      choice = 'dark';
    }
    document.documentElement.setAttribute('${THEME_ATTRIBUTE}', resolve(choice));
  } catch (error) {
    document.documentElement.setAttribute('${THEME_ATTRIBUTE}', 'dark');
  }
})();
`;
