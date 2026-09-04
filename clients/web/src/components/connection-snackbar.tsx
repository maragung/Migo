'use client';

/**
 * The connection Snackbar: the one place the shell says the realtime connection is not healthy.
 *
 * It replaces the old friends-panel badge, which only a reader parked on the Friends tab could
 * see — a reconnect happening while any other tab or pane was showing said nothing anywhere. The
 * Snackbar is fixed to the bottom of the screen over the whole shell (both panes, whichever tab
 * is active), so "Reconnecting…" is visible wherever the person happens to be reading.
 *
 * It speaks only for a signed-in session (`status === 'ready'`): the login and register screens
 * carry their own connecting states on their submit buttons, and a sign-in in flight is not a
 * reconnect. A healthy connection renders nothing at all — silence is the healthy state, and a
 * toast that hangs around while everything works is a toast nobody reads when it matters.
 */

import type { ReactNode } from 'react';

import type { ConnectionState } from '@migo/sdk';

import { useMigo } from '@/lib/migo/use-migo.js';

/** What the Snackbar says for one transport state, or nothing when the state needs no words. */
function snackbarFor(state: ConnectionState): { cls: string; label: string } | null {
  switch (state) {
    case 'ready':
      return null;
    case 'connecting':
    case 'authenticating':
      return { cls: 'snack-wait', label: 'Connecting…' };
    case 'reconnecting':
      return { cls: 'snack-wait', label: 'Reconnecting… your messages will send when it returns.' };
    case 'idle':
    case 'closed':
    default:
      return { cls: 'snack-down', label: 'Offline. Migo reconnects automatically.' };
  }
}

/** The bottom-of-the-screen connection notice, shown while a signed-in session is not connected. */
export function ConnectionSnackbar(): ReactNode {
  const { status, connectionState } = useMigo();
  if (status !== 'ready') {
    return null;
  }
  const info = snackbarFor(connectionState);
  if (info === null) {
    return null;
  }
  return (
    <div className={`conn-snackbar ${info.cls}`} role="status" aria-live="polite">
      <span className="dot" aria-hidden="true" />
      {info.label}
    </div>
  );
}
