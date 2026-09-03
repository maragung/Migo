/**
 * Configuration is the tool's safety interlock: a load test that quietly ran against the wrong
 * target, or ten times slower than asked, is worse than one that refused to start. So the contract
 * under test is threefold — the documented defaults, the flag > MIGO_ env > NEXT_PUBLIC_ env >
 * default precedence, and validation that rejects a bad value with a message naming the field. Every
 * numeric limit is probed at its exact boundary (the first accepted value and the first rejected
 * one), because an off-by-one in a bound is exactly the kind of thing a looser test sails past.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ConfigError, parseArgs } from '../config.js';
import type { Config } from '../config.js';

/** Parse and assert a non-help result, returning the config. */
function cfg(argv: string[], env: NodeJS.ProcessEnv = {}): Config {
  const result = parseArgs(argv, env);
  if (result.help) throw new Error('expected a config, got help');
  return result.config;
}

test('defaults are applied when nothing is supplied', () => {
  const c = cfg([]);
  assert.equal(c.apiUrl, 'http://localhost:8080');
  assert.equal(c.gatewayUrl, 'ws://localhost:8080/ws');
  assert.equal(c.scenario, 'messaging');
  assert.equal(c.vus, 10);
  assert.equal(c.durationMs, 30_000);
  assert.equal(c.ratePerSec, 5);
  assert.equal(c.connectConcurrency, 20);
  assert.equal(c.appVersion, '0.1.0');
  assert.equal(c.locale, 'en-US');
  assert.equal(c.country, 'ID');
  assert.equal(c.usernamePrefix, 'loadgen');
  assert.equal(c.passphrase, undefined);
  assert.equal(c.requestTimeoutMs, 15_000);
  assert.equal(c.maxErrorRate, 1);
  assert.equal(c.output, 'text');
  assert.equal(c.logLevel, 'normal');
});

test('precedence: flag beats MIGO_ env beats NEXT_PUBLIC_ env beats default', () => {
  assert.equal(cfg([], { NEXT_PUBLIC_MIGO_API_URL: 'http://next:1' }).apiUrl, 'http://next:1');
  assert.equal(
    cfg([], { MIGO_API_URL: 'http://migo:2', NEXT_PUBLIC_MIGO_API_URL: 'http://next:1' }).apiUrl,
    'http://migo:2',
  );
  assert.equal(
    cfg(['--api-url', 'http://flag:3'], { MIGO_API_URL: 'http://migo:2' }).apiUrl,
    'http://flag:3',
  );
});

test('both --flag value and --flag=value forms are accepted', () => {
  assert.equal(cfg(['--vus', '25']).vus, 25);
  assert.equal(cfg(['--vus=25']).vus, 25);
  assert.equal(cfg(['--scenario=presence']).scenario, 'presence');
  assert.equal(cfg(['--passphrase', 'pw']).passphrase, 'pw');
});

test('the gateway URL is derived from the API URL: scheme mapped, /ws added when absent', () => {
  assert.equal(cfg(['--api-url', 'http://localhost:8080']).gatewayUrl, 'ws://localhost:8080/ws');
  assert.equal(
    cfg(['--api-url', 'https://api.example.com']).gatewayUrl,
    'wss://api.example.com/ws',
  );
  // A URL that already has a path keeps it, and does not get /ws appended.
  assert.equal(cfg(['--api-url', 'http://host/api']).gatewayUrl, 'ws://host/api');
  // An explicit gateway URL overrides derivation entirely.
  assert.equal(cfg(['--gateway-url', 'ws://custom:9/ws']).gatewayUrl, 'ws://custom:9/ws');
});

test('log level: --quiet, --verbose, default, and --quiet winning over --verbose', () => {
  assert.equal(cfg([]).logLevel, 'normal');
  assert.equal(cfg(['--quiet']).logLevel, 'quiet');
  assert.equal(cfg(['--verbose']).logLevel, 'verbose');
  assert.equal(cfg(['--quiet', '--verbose']).logLevel, 'quiet');
});

test('--help short-circuits to a help result', () => {
  assert.equal(parseArgs(['--help'], {}).help, true);
});

test('--vus is a positive integer, rejected at its boundary with a message naming it', () => {
  assert.equal(cfg(['--vus', '1']).vus, 1); // first accepted value
  assert.throws(() => cfg(['--vus', '0']), /--vus must be a positive integer/); // first rejected
  assert.throws(() => cfg(['--vus', '-1']), /--vus/);
  assert.throws(() => cfg(['--vus', '1.5']), /--vus/);
  assert.throws(() => cfg(['--vus', 'abc']), /--vus/);
});

test('--connect-concurrency is a positive integer with the same boundary', () => {
  assert.equal(cfg(['--connect-concurrency', '1']).connectConcurrency, 1);
  assert.throws(() => cfg(['--connect-concurrency', '0']), /--connect-concurrency/);
});

test('--rate is a non-negative number: 0 is allowed, negatives and non-finite are not', () => {
  assert.equal(cfg(['--rate', '0']).ratePerSec, 0); // 0 means as-fast-as-possible
  assert.equal(cfg(['--rate', '5.5']).ratePerSec, 5.5);
  assert.throws(() => cfg(['--rate', '-0.1']), /--rate must be a non-negative number/);
  assert.throws(() => cfg(['--rate', 'Infinity']), /--rate/);
});

test('--max-error-rate is a fraction in [0, 1], tested at both boundaries', () => {
  assert.equal(cfg(['--max-error-rate', '0']).maxErrorRate, 0);
  assert.equal(cfg(['--max-error-rate', '1']).maxErrorRate, 1);
  assert.equal(cfg(['--max-error-rate', '0.5']).maxErrorRate, 0.5);
  assert.throws(
    () => cfg(['--max-error-rate', '1.0001']),
    /--max-error-rate must be between 0 and 1/,
  );
  assert.throws(() => cfg(['--max-error-rate', '-0.0001']), /--max-error-rate/);
});

test('--duration accepts ms/s/m and a bare number of seconds, and rejects the rest', () => {
  assert.equal(cfg(['--duration', '30s']).durationMs, 30_000);
  assert.equal(cfg(['--duration', '2m']).durationMs, 120_000);
  assert.equal(cfg(['--duration', '500ms']).durationMs, 500);
  assert.equal(cfg(['--duration', '45']).durationMs, 45_000); // bare number = seconds
  assert.equal(cfg(['--duration', '0']).durationMs, 0);
  assert.equal(cfg(['--duration', ' 10s ']).durationMs, 10_000); // trimmed
  assert.throws(() => cfg(['--duration', '1.5s']), /--duration/);
  assert.throws(() => cfg(['--duration', '10h']), /--duration/);
  assert.throws(() => cfg(['--duration', 'soon']), /--duration/);
});

test('--output is text or json, and anything else is refused naming the field', () => {
  assert.equal(cfg(['--output', 'text']).output, 'text');
  assert.equal(cfg(['--output', 'json']).output, 'json');
  assert.throws(() => cfg(['--output', 'xml']), /--output must be "text" or "json"/);
});

test('an unparseable API URL is refused at load, naming the field', () => {
  assert.throws(() => cfg(['--api-url', 'notaurl']), ConfigError);
  assert.throws(() => cfg(['--api-url', 'notaurl']), /--api-url is not a valid URL/);
  // The API URL is only validated when the gateway URL must be derived from it.
  assert.doesNotThrow(() => cfg(['--api-url', 'notaurl', '--gateway-url', 'ws://x/ws']));
});

test('malformed argument lists are rejected', () => {
  assert.throws(() => cfg(['positional']), /unexpected argument: positional/);
  assert.throws(() => cfg(['--vus']), /missing value for --vus/);
  assert.throws(() => cfg(['--vus', '--scenario', 'presence']), /missing value for --vus/);
});
