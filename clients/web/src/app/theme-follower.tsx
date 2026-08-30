'use client';

/**
 * The system-theme follower: keeps a `system` choice honest while the app runs.
 *
 * The pre-paint script resolves the choice once, at load; a visitor whose OS flips between light
 * and dark (a scheduled appearance, a sunset switch) would otherwise keep the wrong skin until
 * the next reload. This component mounts once in the root layout, watches
 * `prefers-color-scheme`, and re-applies the stored choice whenever the OS moves — an explicit
 * light or dark choice is untouched, so only a `system` choice actually follows the OS.
 *
 * It renders nothing; it is a behaviour, not a picture.
 */

import { useEffect } from 'react';
import type { ReactNode } from 'react';

import { getChoice, setChoice, watchSystemTheme } from '@/lib/theme.js';

export function ThemeFollower(): ReactNode {
  useEffect(() => {
    return watchSystemTheme(() => {
      // Re-applying resolves the stored choice against the OS's new answer; an explicit light
      // or dark choice re-applies itself, unchanged by the move.
      if (getChoice() === 'system') {
        setChoice('system');
      }
    });
  }, []);
  return null;
}
