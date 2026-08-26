'use client';

import { useSyncExternalStore } from 'react';

import type { Id } from '@migo/sdk';

/**
 * Which conversation is open, held in the URL fragment.
 *
 * The web client is a static bundle: there is no server to render a route per conversation, and
 * `output: 'export'` cannot prerender `/chat/[id]` because conversation ids only exist at runtime on
 * the device. A dynamic segment would therefore need either a server or a build-time list of every
 * conversation that will ever exist, and neither is available to a client-side-only app.
 *
 * The fragment is the right place for it for a second, better reason: a fragment is never sent to a
 * server. A conversation id in the path would appear in the HTTP request line of whatever static host
 * serves the bundle, and end up in its access log — metadata about who talks to whom, leaked by the
 * URL scheme alone. `#c=<id>` stays in the browser.
 *
 * The fragment is still a real navigation entry, so Back closes the thread and the deep link survives
 * a reload.
 */

/** Fragment key holding the open conversation id. */
const FRAGMENT_KEY = 'c';

/** Subscribes to fragment changes. `hashchange` covers both link clicks and Back/Forward. */
function subscribe(onStoreChange: () => void): () => void {
  window.addEventListener('hashchange', onStoreChange);
  return () => window.removeEventListener('hashchange', onStoreChange);
}

/** The live fragment. A primitive, so `useSyncExternalStore`'s identity check is a comparison. */
function getSnapshot(): string {
  return window.location.hash;
}

/**
 * The fragment as seen during prerender: empty.
 *
 * `next build` renders these pages once at build time with no URL, and the first client render has to
 * match that HTML or React discards it. Reporting an empty fragment on the server and letting the
 * subscription deliver the real one is what keeps hydration consistent.
 */
function getServerSnapshot(): string {
  return '';
}

/** Extracts the conversation id from a fragment, or null if none is open. */
export function parseConversationFragment(hash: string): Id | null {
  const raw = hash.startsWith('#') ? hash.slice(1) : hash;
  if (raw.length === 0) {
    return null;
  }
  const params = new URLSearchParams(raw);
  const id = params.get(FRAGMENT_KEY);
  return id !== null && id.length > 0 ? (id as Id) : null;
}

/** The href that opens `id`. Same-document, so a plain anchor is the whole navigation. */
export function conversationHref(id: Id): string {
  return `#${FRAGMENT_KEY}=${encodeURIComponent(id)}`;
}

/** Opens a conversation, pushing a history entry so Back returns to the list. */
export function openConversation(id: Id): void {
  if (typeof window === 'undefined') {
    return;
  }
  window.location.hash = `${FRAGMENT_KEY}=${encodeURIComponent(id)}`;
}

/**
 * Closes the open conversation.
 *
 * `replaceState` rather than clearing `location.hash`, which would leave a bare `#` in the address bar
 * and add a history entry that goes nowhere. `replaceState` fires no event, so the notification is
 * dispatched by hand.
 */
export function closeConversation(): void {
  if (typeof window === 'undefined') {
    return;
  }
  const { pathname, search } = window.location;
  window.history.replaceState(null, '', `${pathname}${search}`);
  window.dispatchEvent(new Event('hashchange'));
}

/** The open conversation, re-read whenever the fragment changes. */
export function useOpenConversation(): Id | null {
  const hash = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
  return parseConversationFragment(hash);
}
