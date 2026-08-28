/**
 * The server-endpoint flow on the auth form.
 *
 * The form persists the user's chosen endpoint to IndexedDB and reads it back on the next visit;
 * the SDK now uses that structured endpoint in place of the old `baseUrl`/`gatewayUrl` pair. The
 * test that follows proves the three pieces of that contract:
 *
 *   1. The persistence helpers round-trip a {@link ServerEndpoint} through IndexedDB, byte for
 *      byte -- so a real user with a real typed address will see the same value come back, and
 *      the `migo:server-endpoint:v1` key is the one and only place the value lives.
 *   2. The form's {@link buildFromForm} function rejects the shapes the field validates against
 *      (empty host, port out of range, scheme/transport mismatch) with the same form-level error
 *      message the rest of the form uses, and accepts the well-formed shapes it should.
 *   3. The persisted endpoint is read by the same `loadServerEndpoint` the page uses on mount,
 *      so the snapshot a previous visit wrote is what the next visit's disclosure pre-fills from.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import { defaultLoopbackServerEndpoint } from '@migo/sdk';
import type { ServerEndpoint } from '@migo/sdk';

import { buildFromForm } from '../src/components/server-form.js';
import {
  clearServerEndpoint,
  loadServerEndpoint,
  saveServerEndpoint,
} from '../src/lib/storage/server-endpoint-store.js';
import { installFakeIndexedDb, installRecordingWebStorage } from './support/dom-stubs.js';

let idb: ReturnType<typeof installFakeIndexedDb>;

beforeEach(() => {
  idb = installFakeIndexedDb();
});

afterEach(() => {
  idb.restore();
});

test('a ServerEndpoint round-trips through IndexedDB under the documented key', async () => {
  const stored: ServerEndpoint = {
    host: 'migo.example.com',
    port: 8443,
    gatewayPort: 8444,
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  };
  await saveServerEndpoint(stored);
  const read = await loadServerEndpoint();
  assert.deepEqual(read, stored);
  // The snapshot is the only entry in the store, so a stray write to a third key would be caught.
  assert.deepEqual([...idb.store.keys()].sort(), ['migo:server-endpoint:v1']);
});

test('the first visit has no persisted endpoint', async () => {
  assert.equal(await loadServerEndpoint(), undefined);
});

test('clearing the endpoint removes the snapshot', async () => {
  await saveServerEndpoint(defaultLoopbackServerEndpoint('localhost'));
  await clearServerEndpoint();
  assert.equal(await loadServerEndpoint(), undefined);
});

test('buildFromForm returns a valid WebSocket endpoint from a typed form', () => {
  const endpoint = buildFromForm({
    host: 'migo.example.com',
    port: '8443',
    gatewayPort: '8444',
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  });
  assert.equal(endpoint.host, 'migo.example.com');
  assert.equal(endpoint.port, 8443);
  assert.equal(endpoint.gatewayPort, 8444);
  assert.equal(endpoint.scheme, 'Wss');
  assert.equal(endpoint.restScheme, 'Https');
});

test('buildFromForm accepts a host:port shorthand pasted into the host field', () => {
  const endpoint = buildFromForm({
    host: 'migo.example.com:8443',
    port: '',
    gatewayPort: '8444',
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  });
  assert.equal(endpoint.host, 'migo.example.com');
  assert.equal(endpoint.port, 8443);
});

test('buildFromForm lowercases the host and trims surrounding whitespace', () => {
  const endpoint = buildFromForm({
    host: '  Migo.Example.com  ',
    port: '8443',
    gatewayPort: '8444',
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  });
  assert.equal(endpoint.host, 'migo.example.com');
});

test('buildFromForm rejects an empty host', () => {
  assert.throws(
    () =>
      buildFromForm({
        host: '   ',
        port: '8443',
        gatewayPort: '8444',
        transport: 'WebSocket',
        scheme: 'Wss',
        restScheme: 'Https',
      }),
    /host is required/,
  );
});

test('buildFromForm rejects a port out of range or not a whole number', () => {
  for (const bad of ['0', '65536', 'abc', '8080abc']) {
    assert.throws(
      () =>
        buildFromForm({
          host: 'migo.example.com',
          port: bad,
          gatewayPort: '8444',
          transport: 'WebSocket',
          scheme: 'Wss',
          restScheme: 'Https',
        }),
      /port/,
      `expected throw for port "${bad}"`,
    );
  }
});

test('buildFromForm rejects a QUIC scheme on a WebSocket transport', () => {
  assert.throws(
    () =>
      buildFromForm({
        host: 'migo.example.com',
        port: '8443',
        gatewayPort: '8444',
        transport: 'WebSocket',
        scheme: 'Quic',
        restScheme: 'Https',
      }),
    /WS or WSS/,
  );
});

test('buildFromForm rejects a WS scheme on a QUIC transport', () => {
  assert.throws(
    () =>
      buildFromForm({
        host: 'migo.example.com',
        port: '8443',
        gatewayPort: '8444',
        transport: 'Quic',
        scheme: 'Ws',
        restScheme: 'Https',
      }),
    /QUIC or QUIC-TLS/,
  );
});

test('buildFromForm accepts a QUIC transport with a QUIC-TLS scheme', () => {
  const endpoint = buildFromForm({
    host: 'migo.example.com',
    port: '8443',
    gatewayPort: '8444',
    transport: 'Quic',
    scheme: 'QuicTls',
    restScheme: 'Https',
  });
  assert.equal(endpoint.transport, 'Quic');
  assert.equal(endpoint.scheme, 'QuicTls');
});

test('the server endpoint never lands in localStorage, sessionStorage, or a cookie', async () => {
  const web = installRecordingWebStorage();
  try {
    const stored: ServerEndpoint = {
      host: 'migo.example.com',
      port: 8443,
      gatewayPort: 8444,
      transport: 'WebSocket',
      scheme: 'Wss',
      restScheme: 'Https',
    };
    await saveServerEndpoint(stored);
    const read = await loadServerEndpoint();
    assert.deepEqual(read, stored);
    assert.deepEqual(web.writes(), []);
    assert.deepEqual(web.accesses, []);
  } finally {
    web.restore();
  }
});
