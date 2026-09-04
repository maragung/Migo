/**
 * The Migo store app: one page, four shelves, on-chain payment.
 *
 * Entry point. The app boots by resuming the web client's session (same origin, same IndexedDB —
 * see `lib/session.ts`), reads the server's catalogue for prices and entitlements for ownership,
 * and renders the shelves. `/store/<shelf>` is a hash-free path segment the file server already
 * resolves to this bundle's index; the shelf selection is read from the location and kept in the
 * URL so a shared link lands on the shelf it names.
 */

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './app.js';
import './style.css';

const container = document.getElementById('root');
if (container !== null) {
  createRoot(container).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
