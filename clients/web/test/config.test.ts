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

import { config, defaultServerEndpoint } from '../src/lib/config.js';
import type { WebConfig } from '../src/lib/config.js';

test('the configuration exposes exactly the two public fields', () => {
  assert.deepEqual(Object.keys(config).sort(), ['appVersion', 'defaultApiUrl']);
  assert.equal(typeof config.appVersion, 'string');
  assert.ok(config.appVersion.length > 0);
  // defaultApiUrl may be undefined when the build was made without one — the same-origin
  // detector fills the gap at runtime, which is the fix for the fresh-visit "could not
  // reach the server" that a burned-in localhost default produced.
});

test('no configuration value carries anything shaped like a credential', () => {
  const blob = JSON.stringify(config).toLowerCase();
  for (const marker of ['secret', 'passphrase', 'private', 'apikey', 'api_key', 'token', 'bearer']) {
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
