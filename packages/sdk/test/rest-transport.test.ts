/**
 * The REST client's transport fold: a fetch that never completes must arrive
 * as a {@link TransportError}, not as the platform's bare rejection.
 *
 * In a browser a refused connection, a network drop, or a CORS preflight the
 * server did not grant all reject fetch with a plain `TypeError`. Left
 * as-is, that shape is outside the SDK's error vocabulary, and every
 * caller's handling — the web client's included — degrades to an opaque
 * "unknown failure". The tests inject a fetch that rejects exactly the way a
 * browser's does and pin the class that comes out the other side.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { BootstrapClient, RemoteError, TransportError } from '../src/index.js';
import type { ServerEndpoint } from '../src/index.js';

const LOOPBACK: ServerEndpoint = {
  host: '127.0.0.1',
  port: 8080,
  gatewayPort: 8081,
  transport: 'WebSocket',
  scheme: 'Ws',
  restScheme: 'Http',
};

test('a fetch that rejects with a TypeError arrives as a TransportError', async () => {
  const client = new BootstrapClient(LOOPBACK, {
    fetch: () => {
      throw new TypeError('Failed to fetch');
    },
  });
  await assert.rejects(client.config(), (error: unknown) => {
    assert.ok(error instanceof TransportError, `expected TransportError, got ${String(error)}`);
    assert.equal(error.message, 'Failed to fetch');
    return true;
  });
});

test('a fetch that rejects without a message still names the failure', async () => {
  const client = new BootstrapClient(LOOPBACK, {
    fetch: () => Promise.reject(new Error()),
  });
  await assert.rejects(client.config(), (error: unknown) => {
    assert.ok(error instanceof TransportError, `expected TransportError, got ${String(error)}`);
    return true;
  });
});

test('a server verdict still arrives as a RemoteError, untouched by the fold', async () => {
  const client = new BootstrapClient(LOOPBACK, {
    fetch: () =>
      Promise.resolve(
        new Response(
          JSON.stringify({ error: { code: 1100, symbol: 'UNAUTHENTICATED', message: '' } }),
          {
            status: 401,
            headers: { 'content-type': 'application/json' },
          },
        ),
      ),
  });
  await assert.rejects(client.globalAdmins('token'), (error: unknown) => {
    assert.ok(error instanceof RemoteError, `expected RemoteError, got ${String(error)}`);
    assert.ok(!(error instanceof TransportError), 'a verdict is not a transport failure');
    return true;
  });
});
