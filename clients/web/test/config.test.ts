/**
 * The public runtime configuration, and the secret that must never be in it.
 *
 * Every field of `config` is inlined into the JavaScript bundle that ships to every browser, so the
 * rule the file states is load-bearing: it may hold only public endpoints, never a server secret.
 * That failure is silent and total — a credential wired through this object would work perfectly and
 * be handed to every visitor — so a test guards the shape and scans for anything credential-shaped.
 * The defaults matter too: a build with no environment configured still has to produce a bundle that
 * points somewhere and whose URL schemes match their transport, so those are pinned here as well.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { config } from '../src/lib/config.js';
import type { WebConfig } from '../src/lib/config.js';

test('the configuration exposes exactly the three public endpoint fields, each a non-empty string', () => {
  assert.deepEqual(Object.keys(config).sort(), ['apiBaseUrl', 'appVersion', 'gatewayUrl']);
  for (const value of Object.values(config)) {
    assert.equal(typeof value, 'string');
    assert.ok((value as string).length > 0);
  }
});

test('no configuration value carries anything shaped like a credential', () => {
  const blob = JSON.stringify(config).toLowerCase();
  for (const marker of ['secret', 'password', 'private', 'apikey', 'api_key', 'token', 'bearer']) {
    assert.ok(!blob.includes(marker), `configuration leaked something matching "${marker}"`);
  }
});

test('with no build environment set, the endpoints fall back to the documented local defaults', () => {
  assert.equal(config.apiBaseUrl, 'http://localhost:8080');
  assert.equal(config.gatewayUrl, 'ws://localhost:8080/ws');
  assert.equal(config.appVersion, '0.1.0');
});

test('the default schemes match their transport: a websocket gateway and an http API', () => {
  assert.ok(/^wss?:\/\//.test(config.gatewayUrl), 'the gateway must be a websocket URL');
  assert.ok(/^https?:\/\//.test(config.apiBaseUrl), 'the API base must be an http(s) URL');
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
