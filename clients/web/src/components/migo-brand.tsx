'use client';

/**
 * The Migo brand mark: the rotated-square diamond and the word beside it.
 *
 * The desktop shell stamps this over its turquoise ground (the watermark that says whose desk
 * this is before any window opens), and the taskbar carries the diamond alone at 16px — the
 * same mark the auth screens lead with, drawn once so the brand is one shape everywhere.
 */

import type { ReactNode } from 'react';

/** The diamond: a rounded square rotated 45°, filled with the ink it is handed. */
export function MigoDiamond({
  size = 24,
  color = 'currentColor',
}: {
  size?: number;
  color?: string;
}): ReactNode {
  return (
    <svg
      className="migo-diamond"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
    >
      <rect x="6" y="6" width="12" height="12" rx="2.6" transform="rotate(45 12 12)" fill={color} />
    </svg>
  );
}

/** The word: heavy, tight-tracked, inheriting its ink. */
export function MigoWord({ size = 26 }: { size?: number }): ReactNode {
  return (
    <span className="migo-word" style={{ fontSize: size }}>
      Migo
    </span>
  );
}

/** The brand: diamond and word, side by side. */
export function MigoBrand({ size = 26 }: { size?: number }): ReactNode {
  return (
    <span className="migo-brand">
      <MigoDiamond size={size * 0.92} />
      <MigoWord size={size} />
    </span>
  );
}
