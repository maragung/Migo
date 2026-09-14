'use client';

/**
 * The connection dot: the one mark the shell wears for the realtime connection's health.
 *
 * It replaces the old bottom-of-the-screen Snackbar, whose sentence asked for attention the
 * state itself rarely deserves — a reconnect happening while the person is reading something
 * else is news a glance can carry, so the shell says it with colour alone: green steady while
 * connected, yellow pulsing while a connect or reconnect is in flight, red once the transport
 * has dropped and the automatic retry owns the recovery. The dot sits in the bottom-right
 * corner, above the taskbar whichever side the user docked it, and never takes a pointer, so
 * it cannot sit in the way of a click; what it means stays one hover (or one screen-reader
 * pass) away through its label.
 *
 * Unlike the Snackbar it is worn at every state, green included: the person who wants to know
 * the connection is fine should be able to look somewhere and see it, not wonder whether the
 * absence of a warning is health or a dead indicator.
 *
 * It speaks only for a signed-in session (`status === 'ready'`): the login and register screens
 * carry their own connecting states on their submit buttons, and a sign-in in flight is not a
 * reconnect.
 */

import type { ReactNode } from 'react';

import type { ConnectionState } from '@migo/sdk';

import { useMigo } from '@/lib/migo/use-migo.js';

/** What the dot wears for one transport state: a colour class and the words behind it. */
function dotFor(state: ConnectionState): { cls: string; label: string } {
  switch (state) {
    case 'ready':
      return { cls: 'conn-dot-up', label: 'Connected' };
    case 'connecting':
    case 'authenticating':
      return { cls: 'conn-dot-wait', label: 'Connecting…' };
    case 'reconnecting':
      return {
        cls: 'conn-dot-wait',
        label: 'Reconnecting… your messages will send when it returns.',
      };
    case 'idle':
    case 'closed':
    default:
      return { cls: 'conn-dot-down', label: 'Offline. Migo reconnects automatically.' };
  }
}

/** The corner connection mark, worn by a signed-in session whatever the transport is doing. */
export function ConnectionStatusDot(): ReactNode {
  const { status, connectionState } = useMigo();
  if (status !== 'ready') {
    return null;
  }
  const info = dotFor(connectionState);
  return (
    <span
      className={`conn-dot ${info.cls}`}
      role="status"
      aria-live="polite"
      aria-label={info.label}
      title={info.label}
    />
  );
}
