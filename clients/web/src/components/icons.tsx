'use client';

/**
 * The Migo icon set: one family, one stroke.
 *
 * Every icon is a 24×24 viewBox drawn with the same 1.75 stroke, round caps, round joins, and
 * `currentColor` — so an icon inherits the ink of whatever it sits in, and the set reads as one
 * family at 16, 20, and 24 pixels. This module replaces the emoji glyphs the shell previously
 * used for navigation and actions: emoji render differently per platform (an emoji "button" is a
 * different picture on Android, iOS, and desktop), while these strokes are the same picture
 * everywhere Migo runs.
 *
 * The default size is 20 (`--icon-md`); pass `size={16}` or `size={24}` for the other scale
 * steps. Icons are decorative by contract — every interactive use must carry its own
 * `aria-label` — so the SVG itself is `aria-hidden`.
 */

import type { ReactNode } from 'react';

export type IconName =
  | 'home'
  | 'chats'
  | 'rooms'
  | 'space'
  | 'friends'
  | 'bell'
  | 'search'
  | 'wallet'
  | 'user'
  | 'settings'
  | 'plus'
  | 'send'
  | 'smile'
  | 'attach'
  | 'mic'
  | 'gift'
  | 'game'
  | 'star'
  | 'verified'
  | 'back'
  | 'chevron-right'
  | 'menu'
  | 'close'
  | 'sun'
  | 'moon'
  | 'signout'
  | 'coins'
  | 'refresh'
  | 'sparkle'
  | 'shield'
  | 'pin'
  | 'file'
  | 'download'
  | 'user-plus'
  | 'block';

/** The drawn body of each icon, as `<path>`/`<circle>` elements under one `<g>`. */
const GLYPHS: Readonly<Record<IconName, ReactNode>> = {
  home: (
    <>
      <path d="M4 10.5 12 4l8 6.5" />
      <path d="M6 9.5V20h12V9.5" />
      <path d="M10 20v-5h4v5" />
    </>
  ),
  chats: (
    <>
      <path d="M4 6.5A2.5 2.5 0 0 1 6.5 4h11A2.5 2.5 0 0 1 20 6.5v7a2.5 2.5 0 0 1-2.5 2.5H9l-4.2 3.2c-.5.4-1.3 0-1.3-.7V6.5Z" />
      <path d="M8 9.5h8" />
      <path d="M8 12.5h5" />
    </>
  ),
  rooms: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17" />
      <path d="M12 3.5c2.6 2.3 4 5.2 4 8.5s-1.4 6.2-4 8.5c-2.6-2.3-4-5.2-4-8.5s1.4-6.2 4-8.5Z" />
    </>
  ),
  space: (
    <>
      <path d="M4 12h3l2.5-6 4 12 2.5-6h4" />
    </>
  ),
  friends: (
    <>
      <circle cx="9" cy="8.5" r="3.5" />
      <path d="M3.5 19.5c.6-3 2.7-4.5 5.5-4.5s4.9 1.5 5.5 4.5" />
      <path d="M15.5 5.6a3.3 3.3 0 0 1 0 6.3" />
      <path d="M17.5 15.4c1.7.6 2.7 1.9 3 4.1" />
    </>
  ),
  bell: (
    <>
      <path d="M6 10a6 6 0 0 1 12 0c0 3.4.8 5.3 1.6 6.4.4.5 0 1.1-.6 1.1H5c-.6 0-1-.6-.6-1.1C5.2 15.3 6 13.4 6 10Z" />
      <path d="M10 20.2a2.2 2.2 0 0 0 4 0" />
    </>
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="6.5" />
      <path d="m16 16 4.5 4.5" />
    </>
  ),
  wallet: (
    <>
      <path d="M4 7.5A2.5 2.5 0 0 1 6.5 5h11A2.5 2.5 0 0 1 20 7.5v9a2.5 2.5 0 0 1-2.5 2.5h-11A2.5 2.5 0 0 1 4 16.5v-9Z" />
      <path d="M13.5 12H20" />
      <path d="M13.5 9.5v5" />
    </>
  ),
  user: (
    <>
      <circle cx="12" cy="8" r="4" />
      <path d="M4.5 20c.8-3.4 3.7-5.5 7.5-5.5s6.7 2.1 7.5 5.5" />
    </>
  ),
  'user-plus': (
    <>
      <circle cx="9.5" cy="8" r="3.5" />
      <path d="M3.5 19.5c.7-3 2.6-4.5 6-4.5 1 0 1.9.15 2.7.45" />
      <path d="M17.5 13.5v6M14.5 16.5h6" />
    </>
  ),
  block: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="m6 6 12 12" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 3.5v2M12 18.5v2M4.9 7.5l1.7 1M17.4 15.5l1.7 1M4.9 16.5l1.7-1M17.4 8.5l1.7-1" />
    </>
  ),
  plus: (
    <>
      <path d="M12 5v14M5 12h14" />
    </>
  ),
  send: (
    <>
      <path d="M4 11.5 20 4l-7.5 16-2-6.5L4 11.5Z" />
    </>
  ),
  smile: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M8.5 14c.8 1.2 2 1.8 3.5 1.8s2.7-.6 3.5-1.8" />
      <path d="M9 9.5h.01M15 9.5h.01" />
    </>
  ),
  attach: (
    <>
      <path d="m20 11-7.6 7.6a4.2 4.2 0 0 1-6-6l8-8a2.8 2.8 0 0 1 4 4l-8 8a1.4 1.4 0 0 1-2-2l7-7" />
    </>
  ),
  mic: (
    <>
      <rect x="9" y="3.5" width="6" height="10.5" rx="3" />
      <path d="M5.5 11.5a6.5 6.5 0 0 0 13 0" />
      <path d="M12 18v2.5" />
    </>
  ),
  gift: (
    <>
      <path d="M4 11h16v9H4z" />
      <path d="M4 7.5h16V11H4z" />
      <path d="M12 7.5V20" />
      <path d="M12 7.5C10.5 5 9.5 4 8 4a2 2 0 0 0 0 3.5M12 7.5C13.5 5 14.5 4 16 4a2 2 0 0 1 0 3.5" />
    </>
  ),
  game: (
    <>
      <rect x="3" y="7" width="18" height="10.5" rx="4" />
      <path d="M7.5 10.5v3M6 12h3" />
      <path d="M15.5 11h.01M17.5 13h.01" />
    </>
  ),
  star: (
    <>
      <path d="m12 4 2.4 4.9 5.4.8-3.9 3.8.9 5.4-4.8-2.5-4.8 2.5.9-5.4L4.2 9.7l5.4-.8L12 4Z" />
    </>
  ),
  verified: (
    <>
      <path d="m12 3.5 2.2 1.8 2.8-.3 1 2.7 2.5 1.4-.7 2.8.7 2.8-2.5 1.4-1 2.7-2.8-.3L12 20.5l-2.2-1.8-2.8.3-1-2.7-2.5-1.4.7-2.8-.7-2.8 2.5-1.4 1-2.7 2.8.3L12 3.5Z" />
      <path d="m9 12 2 2 4-4.5" />
    </>
  ),
  back: (
    <>
      <path d="m14.5 5.5-6.5 6.5 6.5 6.5" />
    </>
  ),
  'chevron-right': (
    <>
      <path d="m9.5 5.5 6.5 6.5-6.5 6.5" />
    </>
  ),
  menu: (
    <>
      <path d="M4.5 7h15M4.5 12h15M4.5 17h15" />
    </>
  ),
  close: (
    <>
      <path d="m6 6 12 12M18 6 6 18" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4" />
    </>
  ),
  moon: (
    <>
      <path d="M20 13.5A8 8 0 0 1 10.5 4a8 8 0 1 0 9.5 9.5Z" />
    </>
  ),
  signout: (
    <>
      <path d="M14 5H6.5A1.5 1.5 0 0 0 5 6.5v11A1.5 1.5 0 0 0 6.5 19H14" />
      <path d="M10 12h10M20 12l-3-3M20 12l-3 3" />
    </>
  ),
  coins: (
    <>
      <circle cx="9" cy="9" r="5.5" />
      <path d="M16.2 6.3a5.5 5.5 0 1 1-9.9 4.9" />
      <path d="M12.5 20a5.5 5.5 0 1 0 8-7.6" />
    </>
  ),
  refresh: (
    <>
      <path d="M20 12a8 8 0 1 1-2.3-5.6" />
      <path d="M20 4v3.5h-3.5" />
    </>
  ),
  sparkle: (
    <>
      <path d="M12 4l1.7 4.3L18 10l-4.3 1.7L12 16l-1.7-4.3L6 10l4.3-1.7L12 4Z" />
      <path d="M18.5 15.5l.8 2 2 .8-2 .8-.8 2-.8-2-2-.8 2-.8.8-2Z" />
    </>
  ),
  shield: (
    <>
      <path d="M12 3.5 5 6v5c0 4.6 3 8.2 7 9.5 4-1.3 7-4.9 7-9.5V6l-7-2.5Z" />
      <path d="m9 12 2 2 4-4" />
    </>
  ),
  pin: (
    <>
      <path d="M14 3.5 20.5 10M17.2 6.8 11 13l-.7 3.4a1 1 0 0 1-1.7.5l-2.5-2.5a1 1 0 0 1 .5-1.7L10 12l6.2-6.2" />
      <path d="m6.5 17.5-3 3" />
    </>
  ),
  file: (
    <>
      <path d="M13.5 3.5H7A1.5 1.5 0 0 0 5.5 5v14A1.5 1.5 0 0 0 7 20.5h10a1.5 1.5 0 0 0 1.5-1.5V8.5l-5-5Z" />
      <path d="M13.5 3.5v5h5" />
    </>
  ),
  download: (
    <>
      <path d="M12 4v10" />
      <path d="m8 10.5 4 4 4-4" />
      <path d="M5 19.5h14" />
    </>
  ),
};

/** One icon of the family at a scale step, inheriting the current ink. */
export function Icon({
  name,
  size = 20,
  className,
}: {
  name: IconName;
  size?: 16 | 20 | 24;
  className?: string;
}): ReactNode {
  return (
    <svg
      className={className ? `icon icon-${name} ${className}` : `icon icon-${name}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {GLYPHS[name]}
    </svg>
  );
}

/** The coin mark: Migo's $MIG currency, drawn as a filled chip rather than a stroke glyph. */
export function CoinMark({ size = 16 }: { size?: 14 | 16 | 20 }): ReactNode {
  return (
    <svg
      className="icon coin-mark"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
      focusable="false"
    >
      <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="1.75" />
      <path
        d="M8.5 8h7M8.5 12h7M8.5 16h7"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
      />
    </svg>
  );
}
