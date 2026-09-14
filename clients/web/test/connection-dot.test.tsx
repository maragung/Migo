/**
 * What the connection dot wears, and when it wears nothing.
 *
 * The dot is the shell's only connection mark, so its discipline is the whole UX of
 * reconnecting: a signed-in session wears it in the corner for *every* transport state — green
 * steady while connected (the one look a person can seek out to know things are fine, rather
 * than wondering whether an absent warning is health or a dead indicator), amber pulsing for
 * connecting and reconnecting alike, red for a transport that has dropped and whose recovery
 * the automatic retry owns — and the words behind each colour stay one hover away through the
 * title. A session that is not signed in sees nothing, because the auth screens carry their
 * own connecting states and a sign-in in flight is not a reconnect.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ConnectionStatusDot } from '../src/components/connection-status-dot.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import type { MigoContextValue } from '../src/lib/migo/provider.js';
import type { AuthStatus } from '../src/lib/migo/provider.js';
import type { ConnectionState, Id } from '@migo/sdk';

/** The dot under a context double with the given session and transport states. */
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
      <ConnectionStatusDot />
    </MigoContext.Provider>,
  );
}

test('a signed-in session wears the dot in every transport state, hue first and words behind it', () => {
  const connected = render('ready', 'ready');
  assert.ok(connected.includes('conn-dot-up'), 'the connected hue is missing');
  assert.ok(connected.includes('Connected'), 'the connected words are missing');

  const reconnecting = render('ready', 'reconnecting');
  assert.ok(reconnecting.includes('conn-dot-wait'), 'the reconnecting hue is missing');
  assert.ok(reconnecting.includes('Reconnecting…'), 'the reconnecting words are missing');

  const connecting = render('ready', 'connecting');
  assert.ok(connecting.includes('conn-dot-wait'), 'the connecting hue is missing');
  assert.ok(connecting.includes('Connecting…'), 'the connecting words are missing');

  const offline = render('ready', 'closed');
  assert.ok(offline.includes('conn-dot-down'), 'the offline hue is missing');
  assert.ok(offline.includes('Offline'), 'the offline words are missing');

  const authenticating = render('ready', 'authenticating');
  assert.ok(authenticating.includes('conn-dot-wait'), 'the authenticating hue is missing');
});

test('a session that is not signed in renders nothing', () => {
  assert.equal(
    render('connecting', 'connecting'),
    '',
    'a sign-in in flight is not a reconnect — the auth screens own that state',
  );
  assert.equal(
    render('anonymous', 'closed'),
    '',
    'a signed-out visitor must not see a connection mark',
  );
});
