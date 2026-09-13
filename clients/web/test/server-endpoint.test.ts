/**
 * The server-choice flow on the auth form.
 *
 * The form persists the user's chosen endpoint *and mode* to IndexedDB and reads them back on
 * the next visit; the SDK uses that structured endpoint in place of the old `baseUrl`/`gatewayUrl`
 * pair. The tests prove the pieces of that contract:
 *
 *   1. The persistence helpers round-trip a choice through IndexedDB under the `:v2` key — the
 *      record is the one and only place the choice lives — and a v1 record from a build before
 *      modes existed migrates on read, so nobody loses their saved server.
 *   2. The form's {@link buildFromForm} function rejects the shapes the field validates against
 *      (empty host, port out of range, scheme/transport mismatch) with the same form-level error
 *      message the rest of the form uses, and accepts the well-formed shapes it should.
 *   3. The healing rules that reconcile a stale snapshot with the deployment it belongs to keep
 *      their narrow, self-hoster-respecting shape.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import { defaultLoopbackServerEndpoint, serverEndpointFromUrl } from '@migo/sdk';
import type { ServerEndpoint } from '@migo/sdk';

import { buildFromForm } from '../src/components/server-form.js';
import {
  clearServerChoice,
  healStaleEndpoint,
  loadServerChoice,
  saveServerChoice,
} from '../src/lib/storage/server-endpoint-store.js';
import type { StoredServerChoice } from '../src/lib/storage/server-endpoint-store.js';
import { idbSet } from '../src/lib/storage/idb.js';
import {
  installFakeIndexedDb,
  installFakeWindow,
  installRecordingWebStorage,
} from './support/dom-stubs.js';

const KEY_V1 = 'migo:server-endpoint:v1';
const KEY_V2 = 'migo:server-endpoint:v2';

let idb: ReturnType<typeof installFakeIndexedDb>;

beforeEach(() => {
  idb = installFakeIndexedDb();
});

afterEach(() => {
  idb.restore();
});

const MANUAL: ServerEndpoint = {
  host: 'migo.example.com',
  port: 8443,
  gatewayPort: 8444,
  transport: 'WebSocket',
  scheme: 'Wss',
  restScheme: 'Https',
};

test('a choice round-trips through IndexedDB under the documented v2 key', async () => {
  const choice: StoredServerChoice = { mode: 'auto', endpoint: MANUAL };
  await saveServerChoice(choice);
  const read = await loadServerChoice();
  assert.deepEqual(read, choice);
  // The v2 snapshot is the only entry in the store, so a stray write to a third key would be caught.
  assert.deepEqual([...idb.store.keys()].sort(), [KEY_V2]);
});

test('each mode round-trips without the store rewriting it', async () => {
  for (const mode of ['auto', 'server', 'manual'] as const) {
    await saveServerChoice({ mode, endpoint: MANUAL });
    const read = await loadServerChoice();
    assert.equal(read?.mode, mode, `mode ${mode} must survive the round-trip`);
    assert.deepEqual(read?.endpoint, MANUAL);
  }
});

test('the first visit has no persisted choice', async () => {
  assert.equal(await loadServerChoice(), undefined);
});

test('clearing the choice removes the snapshot', async () => {
  await saveServerChoice({ mode: 'manual', endpoint: defaultLoopbackServerEndpoint('localhost') });
  await clearServerChoice();
  assert.equal(await loadServerChoice(), undefined);
  assert.deepEqual([...idb.store.keys()], []);
});

// --- the v1 -> v2 migration ---

test('a v1 record from a build before modes reads back as a manual choice', async () => {
  await idbSet(KEY_V1, MANUAL);
  const read = await loadServerChoice();
  assert.deepEqual(read, { mode: 'manual', endpoint: MANUAL });
});

test('a v2 record takes precedence over a stale v1 record beside it', async () => {
  await idbSet(KEY_V1, MANUAL);
  const fresh: StoredServerChoice = {
    mode: 'server',
    endpoint: serverEndpointFromUrl('http://152.53.102.150:8080'),
  };
  await saveServerChoice(fresh);
  // saveServerChoice deletes the v1 record as part of the write, so the stale address does not
  // linger in a second copy.
  assert.deepEqual([...idb.store.keys()].sort(), [KEY_V2]);
  const read = await loadServerChoice();
  assert.deepEqual(read, fresh);
});

test('clearing removes a leftover v1 record too', async () => {
  await idbSet(KEY_V1, MANUAL);
  await clearServerChoice();
  assert.deepEqual([...idb.store.keys()], []);
});

test('a corrupt v2 mode narrows to manual rather than throwing', async () => {
  await idbSet(KEY_V2, { mode: 'what', endpoint: MANUAL });
  const read = await loadServerChoice();
  assert.equal(read?.mode, 'manual');
  assert.deepEqual(read?.endpoint, MANUAL);
});

test('the server choice never lands in localStorage, sessionStorage, or a cookie', async () => {
  const web = installRecordingWebStorage();
  try {
    const choice: StoredServerChoice = { mode: 'auto', endpoint: MANUAL };
    await saveServerChoice(choice);
    const read = await loadServerChoice();
    assert.deepEqual(read, choice);
    assert.deepEqual(web.writes(), []);
    assert.deepEqual(web.accesses, []);
  } finally {
    web.restore();
  }
});

// --- buildFromForm (the manual fields' validation) ---

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

// --- healing a stale snapshot against the deployment ---

// The deployment this build belongs to, for the healing tests below: the single-port plain-HTTP
// posture the production server answers on.
const deployment: ServerEndpoint = {
  host: '152.53.102.150',
  port: 8080,
  gatewayPort: 8080,
  transport: 'WebSocket',
  scheme: 'Ws',
  restScheme: 'Http',
};

test('an http: page corrects a stale TLS/split-port snapshot in memory', () => {
  const win = installFakeWindow('/login/', '', 'http:');
  try {
    // The SDK's non-loopback default and the pre-single-port layout, in one snapshot.
    const healed = healStaleEndpoint(
      {
        host: '152.53.102.150',
        port: 8080,
        gatewayPort: 8081,
        transport: 'WebSocket',
        scheme: 'Wss',
        restScheme: 'Http',
      },
      undefined,
    );
    assert.equal(healed.scheme, 'Ws');
    assert.equal(healed.restScheme, 'Http');
    assert.equal(healed.gatewayPort, healed.port);
  } finally {
    win.restore();
  }
});

test('an https: page leaves a TLS snapshot alone', () => {
  const win = installFakeWindow('/login/', '', 'https:');
  try {
    const stored: ServerEndpoint = {
      host: 'migo.example.com',
      port: 8443,
      gatewayPort: 8443,
      transport: 'WebSocket',
      scheme: 'Wss',
      restScheme: 'Https',
    };
    assert.deepEqual(healStaleEndpoint(stored, undefined), stored);
  } finally {
    win.restore();
  }
});

test('a snapshot naming the deployment host adopts the deployment port and schemes', () => {
  // Saved against the deployment's older layout: right host, wrong ports, TLS guesses.
  const healed = healStaleEndpoint(
    {
      host: '152.53.102.150',
      port: 18080,
      gatewayPort: 18081,
      transport: 'WebSocket',
      scheme: 'Wss',
      restScheme: 'Https',
    },
    deployment,
  );
  assert.equal(healed.host, '152.53.102.150');
  assert.equal(healed.port, 8080);
  assert.equal(healed.gatewayPort, 8080);
  assert.equal(healed.scheme, 'Ws');
  assert.equal(healed.restScheme, 'Http');
});

test('a snapshot naming another host is a self-hoster record and stays as typed', () => {
  const stored: ServerEndpoint = {
    host: 'home.example.org',
    port: 18080,
    gatewayPort: 18081,
    transport: 'WebSocket',
    scheme: 'Ws',
    restScheme: 'Http',
  };
  assert.deepEqual(healStaleEndpoint(stored, deployment), stored);
});

test('a snapshot already on the deployment endpoint is returned unchanged', () => {
  assert.deepEqual(healStaleEndpoint({ ...deployment }, deployment), deployment);
});
