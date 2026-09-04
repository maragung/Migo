/**
 * What the connection Snackbar says, and when it says nothing.
 *
 * The Snackbar is the shell's only connection notice, so its discipline is the whole UX of
 * reconnecting: a signed-in session sees one line at the bottom of the screen for every
 * unhealthy transport state (with a different line for "trying" versus "gave up for now"), a
 * healthy connection renders *nothing* — silence is the healthy state, and a toast that hangs
 * around while everything works is a toast nobody reads when it matters — and a session that is
 * not signed in sees nothing either, because the auth screens carry their own connecting states
 * and a sign-in in flight is not a reconnect.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ConnectionSnackbar } from '../src/components/connection-snackbar.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import type { MigoContextValue } from '../src/lib/migo/provider.js';
import type { AuthStatus } from '../src/lib/migo/provider.js';
import type { ConnectionState, Id } from '@migo/sdk';

/** The Snackbar under a context double with the given session and transport states. */
function render(status: AuthStatus, connectionState: ConnectionState): string {
  const value = {
    status,
    connectionState,
    accountId: 'acct_self' as Id,
    deviceId: null,
    error: null,
    resetNonce: 0,
    persistKeyStore: () => {},
    client: null,
    register: () => Promise.resolve(),
    loginWithFile: () => Promise.resolve(),
    logout: () => Promise.resolve(),
  } as MigoContextValue;
  return renderToStaticMarkup(
    <MigoContext.Provider value={value}>
      <ConnectionSnackbar />
    </MigoContext.Provider>,
  );
}

test('a signed-in session sees the Snackbar for every unhealthy transport state', () => {
  const reconnecting = render('ready', 'reconnecting');
  assert.ok(reconnecting.includes('Reconnecting…'), 'the reconnecting line is missing');
  assert.ok(reconnecting.includes('conn-snackbar'), 'the snackbar shell is missing');

  const connecting = render('ready', 'connecting');
  assert.ok(connecting.includes('Connecting…'), 'the connecting line is missing');

  const offline = render('ready', 'closed');
  assert.ok(offline.includes('Offline'), 'the offline line is missing');
});

test('a healthy connection, and a session that is not signed in, render nothing', () => {
  assert.equal(render('ready', 'ready'), '', 'a healthy connection must stay silent');
  assert.equal(
    render('connecting', 'connecting'),
    '',
    'a sign-in in flight is not a reconnect — the auth screens own that state',
  );
  assert.equal(
    render('anonymous', 'closed'),
    '',
    'a signed-out visitor must not see a connection notice',
  );
});
