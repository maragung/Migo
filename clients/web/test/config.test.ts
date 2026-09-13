/**
 * The public runtime configuration, and the secret that must never be in it.
 *
 * Every field of `config` is inlined into the JavaScript bundle that ships to every browser, so the
 * rule the file states is load-bearing: it may hold only public endpoints, never a server secret.
 * That failure is silent and total — a credential wired through this object would work perfectly and
 * be handed to every visitor — so a test guards the shape and scans for anything credential-shaped.
 * The defaults matter too: a build with no environment configured still has to produce a bundle that
 * points somewhere, and the very first visit (no persisted snapshot) falls back to that env-supplied
 * URL via the same `defaultServerEndpoint()` shape every other client uses.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { serverEndpointFromUrl } from '@migo/sdk';

import {
  config,
  defaultServerEndpoint,
  knownServers,
  serverListFromEnv,
} from '../src/lib/config.js';
import type { WebConfig } from '../src/lib/config.js';

test('the configuration exposes exactly the three public fields', () => {
  assert.deepEqual(Object.keys(config).sort(), ['appVersion', 'defaultApiUrl', 'servers']);
  assert.equal(typeof config.appVersion, 'string');
  assert.ok(config.appVersion.length > 0);
  // defaultApiUrl and servers may be undefined when the build was made without them — the
  // same-origin detector fills the URL's gap at runtime, and a missing list means the
  // picker stays the single-server manual form.
});

test('no configuration value carries anything shaped like a credential', () => {
  const blob = JSON.stringify(config).toLowerCase();
  for (const marker of [
    'secret',
    'passphrase',
    'private',
    'apikey',
    'api_key',
    'token',
    'bearer',
  ]) {
    assert.ok(!blob.includes(marker), `configuration leaked something matching "${marker}"`);
  }
});

test('with no build environment and no window, the endpoint falls back to localhost:8080', () => {
  // Node has no window, so the same-origin detector cannot run; the last-resort
  // fallback is the development server's port.
  assert.equal(config.appVersion, '0.1.0');
  const endpoint = defaultServerEndpoint();
  assert.equal(endpoint.host, 'localhost');
  assert.equal(endpoint.port, 8080);
  assert.equal(endpoint.gatewayPort, 8080);
  assert.equal(endpoint.transport, 'WebSocket');
  assert.equal(endpoint.scheme, 'Ws');
  assert.equal(endpoint.restScheme, 'Http');
});

// --- the server list ---

test('an unset server list parses to an empty list, and knownServers falls back to the default', () => {
  assert.deepEqual(serverListFromEnv(undefined), []);
  // Without a window and without env, the default is the dev fallback endpoint.
  assert.deepEqual(knownServers(), [defaultServerEndpoint()]);
});

test('a comma-separated list parses through the SDK URL rule', () => {
  const list = serverListFromEnv('http://152.53.102.150:8080, https://node2.example.com');
  assert.equal(list.length, 2);
  assert.deepEqual(list[0], serverEndpointFromUrl('http://152.53.102.150:8080'));
  assert.deepEqual(list[1], serverEndpointFromUrl('https://node2.example.com'));
});

test('an entry that is not a URL is skipped, not fatal', () => {
  const list = serverListFromEnv('http://a.example:8080, not a url, ,http://b.example:8080');
  assert.deepEqual(list.map((endpoint) => endpoint.host).sort(), ['a.example', 'b.example']);
});

test('a duplicate entry collapses to one door', () => {
  const list = serverListFromEnv('http://a.example:8080,http://a.example:8080/');
  assert.equal(list.length, 1);
  assert.equal(list[0]?.host, 'a.example');
});

test('an entry that names no host is skipped, not fatal', () => {
  // `a:8080` parses as a bogus protocol with an empty hostname, and `152.53.102.150:8080`
  // (a schemeless host:port, the shape a deployer is most likely to typo) is not a URL at
  // all — neither may become an endpoint that dials nothing.
  const list = serverListFromEnv('a:8080,152.53.102.150:8080,http://b.example:8080');
  assert.deepEqual(
    list.map((endpoint) => endpoint.host),
    ['b.example'],
  );
});

test('a public environment variable set before load overrides the default', async () => {
  const KEY = 'NEXT_PUBLIC_MIGO_APP_VERSION';
  const previous = process.env[KEY];
  process.env[KEY] = '9.9.9-fromenv';
  try {
    // A fresh module instance (cache-busted URL) re-reads the environment at evaluation time.
    const url = `${new URL('../src/lib/config.js', import.meta.url).href}?override`;
    const fresh = (await import(url)) as unknown as { config: WebConfig };
    assert.equal(fresh.config.appVersion, '9.9.9-fromenv');
  } finally {
    if (previous === undefined) {
      delete process.env[KEY];
    } else {
      process.env[KEY] = previous;
    }
  }
});

test('a server list set before load becomes the known servers', async () => {
  const KEY = 'NEXT_PUBLIC_MIGO_SERVERS';
  const previous = process.env[KEY];
  process.env[KEY] = 'http://152.53.102.150:8080,http://node2.example.com:8080';
  try {
    const url = `${new URL('../src/lib/config.js', import.meta.url).href}?servers`;
    const fresh = (await import(url)) as unknown as {
      knownServers: () => ReturnType<typeof serverListFromEnv>;
    };
    const servers = fresh.knownServers();
    assert.equal(servers.length, 2);
    assert.deepEqual(servers[0], serverEndpointFromUrl('http://152.53.102.150:8080'));
    assert.deepEqual(servers[1], serverEndpointFromUrl('http://node2.example.com:8080'));
  } finally {
    if (previous === undefined) {
      delete process.env[KEY];
    } else {
      process.env[KEY] = previous;
    }
  }
});
