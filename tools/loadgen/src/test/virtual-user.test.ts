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

import { clientEndpoint } from '../config.js';
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

/** The stand-in the helper hands the VU as its transport-state sink, so tests can assert on it. */
const STATE_PROBE = (): void => {};

/** Construct a VirtualUser with the SDK factory stubbed, returning the VU and the captured options. */
function buildWithStubbedClient(
  index: number,
  config: Config,
  client: Record<string, unknown> = {},
): { vu: VirtualUser; created: Record<string, unknown>; client: Record<string, unknown> } {
  // Bound so the restored factory keeps its class as `this`, exactly as the original did.
  const original = MigoClient.create.bind(MigoClient);
  let created: Record<string, unknown> = {};
  (MigoClient as unknown as { create: (options: unknown) => unknown }).create = (
    options: unknown,
  ) => {
    created = options as Record<string, unknown>;
    return client;
  };
  try {
    const vu = new VirtualUser(index, {
      config,
      passphrase: 'pw',
      runTag: 'tag42',
      onEventError: () => {},
      onStateChange: STATE_PROBE,
    });
    return { vu, created, client };
  } finally {
    (MigoClient as unknown as { create: unknown }).create = original;
  }
}

test('the transport-state probe is handed to the SDK client, not swallowed', () => {
  const { created } = buildWithStubbedClient(1, CONFIG);
  assert.equal(created['onStateChange'], STATE_PROBE);
});

test('the throwaway username is prefix_runTag_index and server-legal', () => {
  assert.equal(buildWithStubbedClient(3, CONFIG).vu.username, 'loadgen_tag42_3');
  const custom: Config = { ...CONFIG, usernamePrefix: 'stress' };
  assert.equal(buildWithStubbedClient(7, custom).vu.username, 'stress_tag42_7');
  // The whole rule the server's credential validator enforces (migo-auth's
  // credential.rs), not just "the hyphens are gone" — so no future runTag or
  // prefix edge case can reintroduce a name the server refuses with
  // VALIDATION_FAILED on every VU before a single session opens:
  //   * length 3..=32 (USERNAME_MIN_CHARS / USERNAME_MAX_CHARS),
  //   * the first character is an ASCII lowercase letter,
  //   * the last character is not a separator (no trailing `.` or `_`),
  //   * only letters, digits, dots and underscores, and never two
  //     separators in a row.
  for (const { vu } of [buildWithStubbedClient(3, CONFIG), buildWithStubbedClient(7, custom)]) {
    const name = vu.username;
    assert.ok(name.length >= 3 && name.length <= 32, `${name} must be 3-32 characters`);
    assert.match(name, /^[a-z]/, 'the first character must be a lowercase letter');
    assert.match(name, /[a-z0-9]$/, 'the name must not end with a separator');
    assert.doesNotMatch(name, /[^a-z0-9_.]/, 'only letters, digits, dots and underscores');
    assert.doesNotMatch(name, /[_.][_.]/, 'never two separators in a row');
  }
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
  // The gateway port is the one the run's own URLs name, never the SDK's loopback split-port
  // guess: both load harnesses start a single migod listening on one port with the gateway role,
  // so a virtual user dialling `rest + 1` knocks on a closed port, fails to connect, and leaves a
  // run that measures nothing while exiting zero. The literal is asserted because that is the
  // regression, and the equality with `clientEndpoint` because a literal alone would let the
  // derivation and the wiring drift apart again exactly as they did.
  assert.equal(server['gatewayPort'], 8080);
  assert.equal(server['gatewayPort'], clientEndpoint(CONFIG).gatewayPort);
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

test('wireBytes is the client transport counters, snapshotted at call time', () => {
  // The VU owns exactly one client for its whole life, so the reading it hands the runner is the
  // §171 session: whatever the SDK transport has counted so far, both directions, unchanged. The
  // stub carries a mutable counter object to prove the method reads through rather than caching —
  // the reading taken after teardown must include bytes that arrived after an earlier call.
  const counters = { sent: 7, received: 9 };
  const { vu } = buildWithStubbedClient(0, CONFIG, { wireBytes: counters });
  assert.deepEqual(vu.wireBytes(), { sent: 7, received: 9 });
  counters.sent += 500;
  counters.received += 900;
  assert.deepEqual(vu.wireBytes(), { sent: 507, received: 909 });
});
