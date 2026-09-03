/**
 * A virtual user is the one place the tool wires itself to the SDK, so what matters here is the
 * shape of that wiring: the throwaway username is derived deterministically, and the MigoClient is
 * created with the run's URLs, timeout, and a load-test hello. The SDK factory is stubbed — so no
 * socket is ever opened — and the options it was handed are captured and asserted. Register and
 * disconnect are deliberately not exercised: they are network calls, and this suite must never
 * connect to anything.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { BandwidthMode, MigoClient, Platform } from '@migo/sdk';

import type { Config } from '../config.js';
import { VirtualUser } from '../virtual-user.js';

const CONFIG: Config = {
  apiUrl: 'http://localhost:8080',
  gatewayUrl: 'ws://localhost:8080/ws',
  scenario: 'messaging',
  vus: 10,
  durationMs: 30_000,
  ratePerSec: 5,
  connectConcurrency: 20,
  appVersion: '9.9.9',
  locale: 'en-GB',
  country: 'GB',
  usernamePrefix: 'loadgen',
  passphrase: undefined,
  requestTimeoutMs: 12_345,
  maxErrorRate: 1,
  output: 'text',
  logLevel: 'normal',
};

/** Construct a VirtualUser with the SDK factory stubbed, returning the VU and the captured options. */
function buildWithStubbedClient(
  index: number,
  config: Config,
): { vu: VirtualUser; created: Record<string, unknown> } {
  // Bound so the restored factory keeps its class as `this`, exactly as the original did.
  const original = MigoClient.create.bind(MigoClient);
  let created: Record<string, unknown> = {};
  (MigoClient as unknown as { create: (options: unknown) => unknown }).create = (
    options: unknown,
  ) => {
    created = options as Record<string, unknown>;
    return {};
  };
  try {
    const vu = new VirtualUser(index, {
      config,
      passphrase: 'pw',
      runTag: 'tag42',
      onEventError: () => {},
    });
    return { vu, created };
  } finally {
    (MigoClient as unknown as { create: unknown }).create = original;
  }
}

test('the throwaway username is prefix-runTag-index', () => {
  assert.equal(buildWithStubbedClient(3, CONFIG).vu.username, 'loadgen-tag42-3');
  const custom: Config = { ...CONFIG, usernamePrefix: 'stress' };
  assert.equal(buildWithStubbedClient(7, custom).vu.username, 'stress-tag42-7');
});

test('a fresh VU is not yet connected and has no partner or conversation', () => {
  const { vu } = buildWithStubbedClient(0, CONFIG);
  assert.equal(vu.index, 0);
  assert.equal(vu.connected, false);
  assert.equal(vu.partner, undefined);
  assert.equal(vu.conversationId, undefined);
});

test('the MigoClient is created with the run endpoint, timeout, and identifiable device name', () => {
  const { created } = buildWithStubbedClient(3, CONFIG);
  const server = created['server'] as Record<string, unknown>;
  assert.equal(server['host'], 'localhost');
  assert.equal(server['port'], 8080);
  assert.equal(server['gatewayPort'], 8081);
  assert.equal(server['transport'], 'WebSocket');
  assert.equal(server['scheme'], 'Ws');
  assert.equal(server['restScheme'], 'Http');
  assert.equal(created['requestTimeoutMs'], 12_345);
  assert.equal(created['deviceDisplayName'], 'loadgen/tag42/3');
  assert.equal(typeof created['onEventError'], 'function');
});

test('the client hello identifies the tool as a load test on the configured version/locale', () => {
  const { created } = buildWithStubbedClient(3, CONFIG);
  const hello = created['hello'] as Record<string, unknown>;
  assert.equal(hello['platform'], Platform.LoadTest);
  assert.equal(hello['appVersion'], '9.9.9');
  assert.equal(hello['locale'], 'en-GB');
  assert.equal(hello['bandwidthMode'], BandwidthMode.Normal);
});
