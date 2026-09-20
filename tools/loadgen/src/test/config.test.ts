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

import { gatewayUrl } from '@migo/sdk';

import { ConfigError, clientEndpoint, parseArgs } from '../config.js';
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

test('the endpoint dials the port the node listens on, not the next one up', () => {
  // The regression this function exists for. `--api-url http://localhost:18090` is a plain origin on
  // a loopback host, and the SDK's own derivation reads that as the split-port *dev pair* — REST on
  // 18090, gateway on 18091. Both load harnesses start the opposite shape: one migod bound to
  // 127.0.0.1:18090 that serves `/ws` on that same listener. So the derivation put every virtual
  // user's WebSocket on a port nothing was listening on, the handshake was refused, and the run
  // reported a node it never reached with every counter at zero.
  const endpoint = clientEndpoint(cfg(['--api-url', 'http://localhost:18090']));
  assert.equal(endpoint.port, 18090);
  assert.equal(endpoint.gatewayPort, 18090);
  // Named explicitly so the failure message says which port was dialled rather than only that two
  // numbers differ.
  assert.notEqual(endpoint.gatewayPort, 18091);
  // 127.0.0.1 and ::1 take the same path: the derivation's loopback set is what it is, and the
  // harness defaults are written with `localhost`.
  assert.equal(clientEndpoint(cfg(['--api-url', 'http://127.0.0.1:18200'])).gatewayPort, 18200);
});

test('the gateway URL the report prints is the gateway URL the sockets dial', () => {
  // The strongest form of the claim, because it is the one a reader of the report relies on: take
  // the endpoint the client is built from, render it with the SDK's own URL builder, and compare it
  // to the line the report prints. Compared after URL normalisation — `wss://host:443/ws` and
  // `wss://host/ws` are one endpoint written two ways, and the default-port spelling is not what is
  // under test here.
  const cases = [
    ['http://localhost:18090', undefined],
    ['http://localhost:8080', undefined],
    ['https://api.example.com', undefined],
    ['http://152.53.102.150:8080', undefined],
    ['http://localhost:18090', 'ws://localhost:18091/ws'],
    ['https://api.example.com', 'wss://api.example.com/ws'],
  ] as const;
  for (const [apiUrl, gatewayOverride] of cases) {
    const argv = ['--api-url', apiUrl];
    if (gatewayOverride !== undefined) argv.push('--gateway-url', gatewayOverride);
    const config = cfg(argv);
    const dialled = new URL(gatewayUrl(clientEndpoint(config)));
    const printed = new URL(config.gatewayUrl);
    assert.equal(
      dialled.href,
      printed.href,
      `${apiUrl}${gatewayOverride === undefined ? '' : ` + ${gatewayOverride}`}: the report says ` +
        `${printed.href} and the run dialled ${dialled.href}`,
    );
  }
});

test('an explicit --gateway-url governs the run, and carries its TLS posture with it', () => {
  // A developer running the real split-port pair can still say so, and now it takes effect: before
  // this, the flag reached the report and nothing else, so the one flag that could have pointed the
  // run at the second listener changed only the sentence describing the run.
  const split = clientEndpoint(
    cfg(['--api-url', 'http://localhost:18090', '--gateway-url', 'ws://localhost:18091/ws']),
  );
  assert.equal(split.port, 18090);
  assert.equal(split.gatewayPort, 18091);
  assert.equal(split.scheme, 'Ws');

  // The scheme comes from the gateway URL, the REST posture from the API URL — the two flags are
  // independent fields of one endpoint, and a run may be plain on one side and TLS on the other.
  const tls = clientEndpoint(
    cfg(['--api-url', 'https://api.example.com', '--gateway-url', 'wss://api.example.com/ws']),
  );
  assert.equal(tls.scheme, 'Wss');
  assert.equal(tls.restScheme, 'Https');
  assert.equal(tls.gatewayPort, 443);

  // The same endpoint written with an explicit port is the same endpoint.
  assert.equal(
    clientEndpoint(
      cfg([
        '--api-url',
        'https://api.example.com',
        '--gateway-url',
        'wss://api.example.com:443/ws',
      ]),
    ).gatewayPort,
    443,
  );
});

test('the REST side of the endpoint still comes from --api-url', () => {
  const plain = clientEndpoint(cfg(['--api-url', 'http://152.53.102.150:8080']));
  assert.equal(plain.restScheme, 'Http');
  assert.equal(plain.port, 8080);
  assert.equal(plain.transport, 'WebSocket');
  // The VPS shape is the one the derivation already got right, and it must keep working: a plain
  // origin on a non-loopback host serves `/ws` on its own port.
  assert.equal(plain.gatewayPort, 8080);

  const tls = clientEndpoint(cfg(['--api-url', 'https://Node.Example.com']));
  assert.equal(tls.restScheme, 'Https');
  assert.equal(tls.port, 443);
  assert.equal(tls.host, 'node.example.com');
});

test('a gateway URL the endpoint shape cannot express is refused, naming the reason', () => {
  // One host per endpoint, and the SDK dials `/ws` on it: each of these asks for something this
  // shape cannot say, and silently dialling the API host instead would put the report and the run
  // back into disagreement — the defect this function exists to close.
  assert.throws(
    () =>
      clientEndpoint(
        cfg(['--api-url', 'http://localhost:18090', '--gateway-url', 'ws://other:18090/ws']),
      ),
    /names host "other" while --api-url names "localhost"/,
  );
  assert.throws(
    () =>
      clientEndpoint(
        cfg(['--api-url', 'http://localhost:18090', '--gateway-url', 'quic://localhost:18090/ws']),
      ),
    /must be a ws:\/\/ or wss:\/\/ URL/,
  );
  assert.throws(
    () =>
      clientEndpoint(cfg(['--api-url', 'http://host/migo', '--gateway-url', 'ws://host/migo/ws'])),
    /the SDK dials the gateway at "\/ws"/,
  );
  // A bare origin is the same request as `/ws`, not a different one.
  assert.doesNotThrow(() =>
    clientEndpoint(
      cfg(['--api-url', 'http://localhost:18090', '--gateway-url', 'ws://localhost:18090']),
    ),
  );
});
