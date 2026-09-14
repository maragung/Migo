'use client';

/**
 * The connection mark: the one mark the app wears for the realtime connection's health.
 *
 * It replaces the old bottom-of-the-screen Snackbar, whose sentence asked for attention the
 * state itself rarely deserves — a reconnect happening while the person is reading something
 * else is news a glance can carry, so the app says it with colour alone: green steady while
 * connected, yellow pulsing while a connect or reconnect is in flight, red once the transport
 * has dropped and the automatic retry owns the recovery. What it means stays one hover (or one
 * screen-reader pass) away through its label.
 *
 * The mark used to float in the shell's corner, above whatever window was showing — which put
 * it over the chat window, where a person reading a thread neither asked for it nor could act
 * on it. It now sits inline in the spots the $MIG balance used to own before the balance moved
 * up into the me bar: the list windows' footer band and the desk taskbar's chip. Those are the
 * resting glances — a status bar in the oldest sense — and the vocabulary is unchanged by the
 * move: the same hues, the same words behind them.
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

/** What the mark wears for one transport state: a colour class, the word, and the words behind it. */
function markFor(state: ConnectionState): { cls: string; word: string; label: string } {
  switch (state) {
    case 'ready':
      return { cls: 'conn-dot-up', word: 'Online', label: 'Connected' };
    case 'connecting':
    case 'authenticating':
      return { cls: 'conn-dot-wait', word: 'Connecting…', label: 'Connecting…' };
    case 'reconnecting':
      return {
        cls: 'conn-dot-wait',
        word: 'Reconnecting…',
        label: 'Reconnecting… your messages will send when it returns.',
      };
    case 'idle':
    case 'closed':
    default:
      return {
        cls: 'conn-dot-down',
        word: 'Offline',
        label: 'Offline. Migo reconnects automatically.',
      };
  }
}

/**
 * The inline connection mark, worn by a signed-in session whatever the transport is doing.
 *
 * The word beside the dot is the glance's half (Online, Reconnecting…, Offline) and the label
 * behind the hover is the sentence; neither states more than the transport actually knows.
 */
export function ConnectionStatusDot(): ReactNode {
  const { status, connectionState } = useMigo();
  if (status !== 'ready') {
    return null;
  }
  const info = markFor(connectionState);
  return (
    <span
      className={`conn-mark ${info.cls}`}
      role="status"
      aria-live="polite"
      aria-label={info.label}
      title={info.label}
    >
      <span className="conn-mark-dot" aria-hidden="true" />
      {info.word}
    </span>
  );
}
