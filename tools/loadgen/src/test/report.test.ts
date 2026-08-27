/**
 * A report is read after the run is over and routinely pasted somewhere else, so two properties
 * matter beyond "it renders": it is a pure function of its input (no clock, no randomness, no
 * map-order surprise), and it discloses no credential or full IP. Determinism is proved by rendering
 * two independently-built but identical outcomes and comparing bytes; disclosure is proved by
 * feeding a deliberately hostile server URL — userinfo plus an IP host — and a password, then
 * asserting none of them survive into either the text or the JSON.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import type { Config } from '../config.js';
import { computeErrorRate, isOk, renderJson, renderText } from '../report.js';
import type { RunOutcome } from '../report.js';
import { Metrics } from '../stats.js';

const BASE_CONFIG: Config = {
  apiUrl: 'http://localhost:8080',
  gatewayUrl: 'ws://localhost:8080/ws',
  scenario: 'messaging',
  vus: 4,
  durationMs: 30_000,
  ratePerSec: 5,
  connectConcurrency: 20,
  appVersion: '0.1.0',
  locale: 'en-US',
  country: 'ID',
  usernamePrefix: 'loadgen',
  password: undefined,
  requestTimeoutMs: 15_000,
  maxErrorRate: 1,
  output: 'text',
  logLevel: 'normal',
};

function makeConfig(overrides: Partial<Config> = {}): Config {
  return { ...BASE_CONFIG, ...overrides };
}

/** A metrics object with a fixed, fully-determined set of samples. */
function fixedMetrics(): Metrics {
  const metrics = new Metrics();
  for (const ms of [5, 15, 25, 35]) metrics.latency('connect').record(ms);
  for (let i = 0; i < 4; i += 1) metrics.recordOk('connect');
  for (const ms of [10, 20, 30, 40, 50, 60, 70, 80, 90, 100]) metrics.latency('send').record(ms);
  for (let i = 0; i < 10; i += 1) metrics.recordOk('send');
  metrics.recordError('send', 'transport');
  metrics.recordError('send', 'transport');
  return metrics;
}

function makeOutcome(
  config: Config,
  metrics: Metrics,
  overrides: Partial<RunOutcome> = {},
): RunOutcome {
  return {
    config,
    scenarioName: config.scenario,
    requestedVus: config.vus,
    connectedCount: config.vus,
    durationMsActual: 30_000,
    interrupted: false,
    metrics,
    ...overrides,
  };
}

test('renderText is deterministic across two independently-built identical outcomes', () => {
  const a = renderText(makeOutcome(makeConfig(), fixedMetrics()));
  const b = renderText(makeOutcome(makeConfig(), fixedMetrics()));
  assert.equal(a, b);
});

test('renderJson is deterministic and parses as JSON', () => {
  const a = renderJson(makeOutcome(makeConfig(), fixedMetrics()));
  const b = renderJson(makeOutcome(makeConfig(), fixedMetrics()));
  assert.equal(a, b);
  assert.doesNotThrow(() => JSON.parse(a));
});

test('renderText pins the report format for a fixed input', () => {
  const text = renderText(makeOutcome(makeConfig(), fixedMetrics()));
  assert.ok(text.includes('Migo load test — scenario "messaging"'));
  assert.ok(text.includes('  server         http://localhost:8080  (ws://localhost:8080/ws)'));
  // n=10 send latencies [10..100]: p50 idx4=50, p90 idx8=90, p99 idx9=100.
  assert.ok(text.includes('p50 50.0ms  p90 90.0ms  p99 100.0ms  (min 10.0ms max 100.0ms)'));
  assert.ok(text.includes('~0.3/s')); // 10 sends / 30s
  // 2 errors of 16 total operations = 12.50%, within the default budget of 1.
  assert.ok(text.includes('Result: OK  (error rate 12.50%)'));
});

test('an interrupted run is marked in the header', () => {
  const text = renderText(makeOutcome(makeConfig(), fixedMetrics(), { interrupted: true }));
  assert.ok(text.includes('scenario "messaging" (interrupted)'));
});

test('renderJson carries the expected shape and per-operation fields', () => {
  const doc = JSON.parse(renderJson(makeOutcome(makeConfig(), fixedMetrics()))) as {
    scenario: string;
    server: { api: string; gateway: string };
    requestedVus: number;
    connectedCount: number;
    ok: boolean;
    errorRate: number;
    operations: Array<{
      label: string;
      ok: number;
      errors: number;
      errorsByClass: Record<string, number>;
      throughputPerSec: number | null;
      latency: { count: number; p50: number };
    }>;
  };
  assert.equal(doc.scenario, 'messaging');
  assert.equal(doc.requestedVus, 4);
  assert.equal(doc.connectedCount, 4);
  assert.equal(doc.ok, true);
  const send = doc.operations.find((op) => op.label === 'send');
  assert.ok(send, 'send operation is present');
  assert.equal(send.ok, 10);
  assert.equal(send.errors, 2);
  assert.deepEqual(send.errorsByClass, { transport: 2 });
  assert.equal(send.latency.p50, 50);
  const connect = doc.operations.find((op) => op.label === 'connect');
  assert.ok(connect, 'connect operation is present');
  assert.equal(connect.throughputPerSec, null, 'a lifecycle phase has no throughput');
});

test('computeErrorRate and isOk read the budget correctly', () => {
  const metrics = new Metrics();
  for (let i = 0; i < 9; i += 1) metrics.recordOk('send');
  metrics.recordError('send', 'transport');
  assert.equal(computeErrorRate(makeOutcome(makeConfig(), metrics)), 0.1);
  assert.equal(isOk(makeOutcome(makeConfig({ maxErrorRate: 1 }), metrics)), true);
  assert.equal(isOk(makeOutcome(makeConfig({ maxErrorRate: 0.05 }), metrics)), false);
  assert.equal(isOk(makeOutcome(makeConfig({ maxErrorRate: 0.1 }), metrics)), true); // boundary: <=
});

test('an empty run has a zero error rate and does not divide by zero', () => {
  const rate = computeErrorRate(makeOutcome(makeConfig(), new Metrics()));
  assert.equal(rate, 0);
  assert.ok(Number.isFinite(rate));
});

test('the report never prints a credential, a token, or a full IP address', () => {
  const config = makeConfig({
    apiUrl: 'http://admin:s3cr3tPw@198.51.100.7:8080',
    gatewayUrl: 'wss://t0k3nValue@198.51.100.7:8080/ws',
    password: 'topSecretPw',
  });
  const text = renderText(makeOutcome(config, fixedMetrics()));
  const json = renderJson(makeOutcome(config, fixedMetrics()));
  for (const secret of [
    's3cr3tPw',
    'admin:s3cr3tPw',
    't0k3nValue',
    '198.51.100.7',
    'topSecretPw',
  ]) {
    assert.ok(!text.includes(secret), `text must not contain ${secret}`);
    assert.ok(!json.includes(secret), `json must not contain ${secret}`);
  }
  // The sanitized server is still shown, just without the credential and full IP.
  assert.ok(text.includes('198.51.100.x'));
  const doc = JSON.parse(json) as { server: { api: string; gateway: string } };
  assert.equal(doc.server.api, 'http://198.51.100.x:8080');
});
