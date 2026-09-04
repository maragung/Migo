'use client';

/**
 * The store as a right-pane tab: the same /store/ app the origin serves, docked like a chat.
 *
 * The store is its own bundle (`clients/store`) — its own session restore, its own pay flow,
 * its own shelves — and it shares this browser's Migo session through IndexedDB, which is why
 * an iframe is the whole integration: same origin, same login, no bridge to build and no
 * second copy of the purchase logic to keep honest. Docking it here answers the ask the
 * banner's old `window.open` could not: the store sits in the pane beside the threads, one
 * closable chip like a chat window, instead of a browser tab that leaves the app behind.
 *
 * The pane is the store's viewport, so the frame fills it and scrolls inside itself — the
 * pane's own scroll never fights the shelves'. The title is spoken for accessibility; the
 * loading/failed states are the frame's own (`title` only, no script reaches across the
 * same-origin boundary from here).
 */

import type { ReactNode } from 'react';

/** Where the origin serves the store bundle — the same path the old new-tab link opened. */
const STORE_URL = '/store/';

/** The store, as the pane's content. */
export function StorePanel(): ReactNode {
  return (
    <div className="store-panel">
      <iframe
        className="store-frame"
        src={STORE_URL}
        title="Migo Store"
        /* Same-origin: the frame shares this session's IndexedDB. Nothing else is granted —
           no allow-* attributes, so the store stays exactly the app the origin serves. */
      />
    </div>
  );
}
