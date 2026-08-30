'use client';

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { getTheme, toggleTheme } from '@/lib/theme.js';
import type { Theme } from '@/lib/theme.js';

/**
 * The light/dark switch: a single round button that flips {@link Theme} and persists it.
 *
 * The icon names the theme a click would move to — the sun while dark, the moon while light — and
 * the label says the same in words, so the control is legible wherever a glyph is not. The theme
 * itself is not React state: it lives on `<html data-theme>` and in `localStorage`, owned by
 * {@link setTheme}. This component holds only the echo needed to draw the right icon, seeded dark
 * so the server and the first client render agree (a hydration mismatch over a cosmetic icon is
 * worse than one frame of the wrong glyph), then corrected from storage after mount.
 *
 * Both props are optional overrides — a parent that already tracks the theme (or a test) can pin
 * the appearance and intercept the flip, while the default stands alone on any page.
 */
export function ThemeToggle({
  theme: themeProp,
  onToggle,
  className = '',
}: {
  /** Pins the rendered theme; defaults to the persisted one. */
  theme?: Theme;
  /** Called on click; defaults to flipping the persisted theme. */
  onToggle?: () => void;
  /** Extra class names for placement (e.g. the fixed corner on the auth screens). */
  className?: string;
}): ReactNode {
  const [current, setCurrent] = useState<Theme>('dark');

  // The stored choice differs from the seeded default on a returning light-theme visitor; adopt
  // it after mount so the first interactive paint shows the icon the click would act on.
  useEffect(() => {
    setCurrent(getTheme());
  }, []);

  const theme = themeProp ?? current;
  const next: Theme = theme === 'dark' ? 'light' : 'dark';
  const handle = onToggle ?? (() => setCurrent(toggleTheme()));

  return (
    <button
      type="button"
      className={`theme-toggle ${className}`.trim()}
      aria-label={`Switch to ${next} theme`}
      title={`Switch to ${next} theme`}
      onClick={handle}
    >
      <span aria-hidden="true">{theme === 'dark' ? '☀️' : '🌙'}</span>
    </button>
  );
}
