'use client';

/**
 * The shell's window vocabulary, shared by the machine and its chrome.
 *
 * The desktop metaphor holds windows in one flat list with a z-order; every surface that talks
 * about them (the taskbar's buttons, the phone's tab strip, the machine itself) needs the same
 * shape and the same labels, so they live here rather than in any one of them.
 */

import type { Id } from '@migo/sdk';

import type { IconName } from './icons.js';

/** The things that open as windows: a conversation thread, or one of the app's panels. */
export type WinKind =
  | 'chat'
  | 'notifications'
  | 'search'
  | 'wallet'
  | 'profile'
  | 'account'
  | 'settings'
  | 'admins'
  | 'store'
  | 'games';

/** One open window on the desk (or, on a phone, one tab in the strip). */
export interface WinState {
  /** `chat:<conversation>` for a thread; the kind itself for one-per-kind panels. */
  id: string;
  kind: WinKind;
  /** What the title bar and the taskbar tab call it. */
  title: string;
  /** The desk position the cascade chose. */
  x: number;
  y: number;
  /** The z-order slot; the shell's focus counter owns it. */
  z: number;
  /** A minimized window renders nothing; its tab is what remains of it. */
  minimized: boolean;
  /** The conversation a chat window shows. */
  conversationId?: Id;
}

/** What the taskbar calls each kind, beside the title. */
export const KIND_LABEL: Readonly<Record<WinKind, string>> = {
  chat: 'Chat',
  notifications: 'Alerts',
  search: 'Search',
  wallet: 'Wallet',
  profile: 'Profile',
  account: 'Account',
  settings: 'Settings',
  admins: 'Admins',
  store: 'Store',
  games: 'Games',
};

/** The icon each kind's tab carries. */
export const KIND_ICON: Readonly<Record<WinKind, IconName>> = {
  chat: 'chats',
  notifications: 'bell',
  search: 'search',
  wallet: 'wallet',
  profile: 'user',
  account: 'shield',
  settings: 'settings',
  admins: 'shield',
  store: 'gift',
  games: 'game',
};

/** The desk's window sizes: the default side window, and the two the design names. */
export const WINDOW_SIZES: Readonly<{ w: number; h: number }> = { w: 400, h: 320 };
export const STORE_WINDOW = { w: 430, h: 386 };

/** A chat window's identity, so a conversation can never open twice. */
export function chatWinId(conversationId: Id): string {
  return `chat:${conversationId}`;
}
