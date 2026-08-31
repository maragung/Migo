/**
 * The `ServerEndpoint` shape: parsing, default-port rule, scheme-transport pairing.
 *
 * The form is the place users express a self-hosting choice, and the rules below are what keeps
 * that expression from turning into a silent misconfiguration. A user who types `migo.example.com`
 * gets the same defaults the previous version of the SDK had; a user who pastes a `host:port`
 * shorthand into the host field gets the port honoured once. A scheme that does not pair with the
 * chosen transport is rejected with the same form-level message the rest of the form uses, and
 * the URL builders are pinned so a future change to one of them cannot quietly route traffic
 * somewhere else.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  assertValidServerEndpoint,
  defaultInternetServerEndpoint,
  defaultLoopbackServerEndpoint,
  defaultSchemesForHost,
  gatewaySchemePrefix,
  gatewayUrl,
  isLoopbackHost,
  parseHost,
  restBaseUrl,
  restSchemePrefix,
  serverEndpointFromUrl,
  validatePorts,
  validateServerEndpoint,
  ServerEndpointError,
} from '../src/index.js';
import type { ServerEndpoint } from '../src/index.js';

test('parseHost accepts a bare host and uses the port fallback', () => {
  const { host, port } = parseHost('migo.example.com', 18080);
  assert.equal(host, 'migo.example.com');
  assert.equal(port, 18080);
});

test('parseHost lowercases the host and splits a host:port shorthand once', () => {
  const { host, port } = parseHost('Migo.Example.com:8443', 18080);
  assert.equal(host, 'migo.example.com');
  assert.equal(port, 8443);
});

test('parseHost trims whitespace before parsing', () => {
  const { host, port } = parseHost('  migo.example.com  ', 18080);
  assert.equal(host, 'migo.example.com');
  assert.equal(port, 18080);
});

test('parseHost rejects an empty input', () => {
  assert.throws(() => parseHost('', 18080), ServerEndpointError);
  assert.throws(() => parseHost('   ', 18080), ServerEndpointError);
});

test('parseHost rejects a port that is not a whole number in [1, 65535]', () => {
  assert.throws(() => parseHost('migo.example.com:0', 18080), ServerEndpointError);
  assert.throws(() => parseHost('migo.example.com:65536', 18080), ServerEndpointError);
  assert.throws(() => parseHost('migo.example.com:abc', 18080), ServerEndpointError);
  assert.throws(() => parseHost('migo.example.com:8080abc', 18080), ServerEndpointError);
});

test('parseHost rejects an empty host portion or an input that has more than one colon', () => {
  assert.throws(() => parseHost(':8080', 18080), ServerEndpointError);
  assert.throws(() => parseHost('a:b:c', 18080), ServerEndpointError);
});

test('validatePorts rejects out-of-range numeric fields', () => {
  assert.throws(() => validatePorts(0, 18081), ServerEndpointError);
  assert.throws(() => validatePorts(18080, 65536), ServerEndpointError);
  assert.throws(() => validatePorts(18080.5, 18081), ServerEndpointError);
});

test('validatePorts accepts the boundaries', () => {
  validatePorts(1, 1);
  validatePorts(65535, 65535);
});

test('the default loopback endpoint uses WS over plain HTTP, with the gateway on rest+1', () => {
  const endpoint = defaultLoopbackServerEndpoint('localhost');
  assert.equal(endpoint.host, 'localhost');
  assert.equal(endpoint.port, 18080);
  assert.equal(endpoint.gatewayPort, 18081);
  assert.equal(endpoint.transport, 'WebSocket');
  assert.equal(endpoint.scheme, 'Ws');
  assert.equal(endpoint.restScheme, 'Http');
});

test('the default internet endpoint uses WSS over HTTPS, with the gateway on the REST port', () => {
  const endpoint = defaultInternetServerEndpoint('migo.example.com', 443);
  assert.equal(endpoint.host, 'migo.example.com');
  assert.equal(endpoint.port, 443);
  assert.equal(endpoint.gatewayPort, 443);
  assert.equal(endpoint.transport, 'WebSocket');
  assert.equal(endpoint.scheme, 'Wss');
  assert.equal(endpoint.restScheme, 'Https');
});

test('isLoopbackHost recognises the three loopback spellings and not a real domain', () => {
  assert.equal(isLoopbackHost('localhost'), true);
  assert.equal(isLoopbackHost('127.0.0.1'), true);
  assert.equal(isLoopbackHost('::1'), true);
  assert.equal(isLoopbackHost('LOCALHOST'), true);
  assert.equal(isLoopbackHost('migo.example.com'), false);
  assert.equal(isLoopbackHost('192.168.1.1'), false);
});

test('defaultSchemesForHost pairs WS/WSS with HTTP/HTTPS for a WebSocket transport', () => {
  assert.deepEqual(defaultSchemesForHost('localhost'), { scheme: 'Ws', restScheme: 'Http' });
  assert.deepEqual(defaultSchemesForHost('127.0.0.1'), { scheme: 'Ws', restScheme: 'Http' });
  assert.deepEqual(defaultSchemesForHost('migo.example.com'), {
    scheme: 'Wss',
    restScheme: 'Https',
  });
});

test('gatewaySchemePrefix and restSchemePrefix reflect the chosen postures', () => {
  const ws = defaultLoopbackServerEndpoint('localhost');
  assert.equal(gatewaySchemePrefix(ws), 'ws');
  assert.equal(restSchemePrefix(ws), 'http');

  const wss = defaultInternetServerEndpoint('migo.example.com', 443);
  assert.equal(gatewaySchemePrefix(wss), 'wss');
  assert.equal(restSchemePrefix(wss), 'https');
});

test('gatewaySchemePrefix returns "quic" for a QUIC transport regardless of TLS posture', () => {
  const quic: ReturnType<typeof defaultLoopbackServerEndpoint> = {
    ...defaultLoopbackServerEndpoint('localhost'),
    transport: 'Quic',
    scheme: 'Quic',
  };
  const quicTls: ReturnType<typeof defaultLoopbackServerEndpoint> = {
    ...defaultLoopbackServerEndpoint('localhost'),
    transport: 'Quic',
    scheme: 'QuicTls',
  };
  assert.equal(gatewaySchemePrefix(quic), 'quic');
  assert.equal(gatewaySchemePrefix(quicTls), 'quic');
});

test('restBaseUrl and gatewayUrl are the documented shapes', () => {
  const endpoint = defaultLoopbackServerEndpoint('localhost');
  assert.equal(restBaseUrl(endpoint), 'http://localhost:18080');
  assert.equal(gatewayUrl(endpoint), 'ws://localhost:18081/ws');
});

test('gatewayUrl keeps the /ws path even when the gateway and REST ports are the same', () => {
  const endpoint = defaultInternetServerEndpoint('migo.example.com', 443);
  assert.equal(restBaseUrl(endpoint), 'https://migo.example.com:443');
  assert.equal(gatewayUrl(endpoint), 'wss://migo.example.com:443/ws');
});

test('serverEndpointFromUrl parses a plain HTTP origin and picks the loopback schemes on localhost', () => {
  const endpoint = serverEndpointFromUrl('http://localhost:18080');
  assert.equal(endpoint.host, 'localhost');
  assert.equal(endpoint.port, 18080);
  assert.equal(endpoint.gatewayPort, 18081);
  assert.equal(endpoint.scheme, 'Ws');
  assert.equal(endpoint.restScheme, 'Http');
});

test('serverEndpointFromUrl parses an HTTPS origin and picks the TLS pair on a real domain', () => {
  const endpoint = serverEndpointFromUrl('https://migo.example.com');
  assert.equal(endpoint.host, 'migo.example.com');
  // HTTPS carries no explicit port; the URL parser makes it 443. A TLS deployment serves /ws
  // on the port the URL names, so the gateway rides the same port.
  assert.equal(endpoint.port, 443);
  assert.equal(endpoint.gatewayPort, 443);
  assert.equal(endpoint.scheme, 'Wss');
  assert.equal(endpoint.restScheme, 'Https');
});

test('serverEndpointFromUrl treats a plain HTTP origin on a public host as a single-port deployment', () => {
  // This repository's own VPS shape: `migod` serves REST and /ws on one plain-HTTP port. The
  // URL's own scheme is the ground truth (the same rule Android and desktop apply), so the
  // gateway rides the same port plain — never `wss://` one port up, which nothing serves.
  const endpoint = serverEndpointFromUrl('http://152.53.102.150:8080');
  assert.equal(endpoint.host, '152.53.102.150');
  assert.equal(endpoint.port, 8080);
  assert.equal(endpoint.gatewayPort, 8080);
  assert.equal(endpoint.scheme, 'Ws');
  assert.equal(endpoint.restScheme, 'Http');
});

test('serverEndpointFromUrl throws on a non-URL string', () => {
  assert.throws(() => serverEndpointFromUrl('not-a-url'), ServerEndpointError);
});

test('serverEndpointFromUrl returns the loopback default for an empty or whitespace input', () => {
  // Empty input is the dev-policy fallback, not an error: a build that runs without an env var
  // should still get a usable endpoint rather than crash.
  const fallback = serverEndpointFromUrl('');
  assert.equal(fallback.host, 'localhost');
  assert.equal(fallback.port, 18080);
  assert.equal(fallback.scheme, 'Ws');
  assert.equal(fallback.restScheme, 'Http');
});

test('serverEndpointFromUrl honours an explicit port and drops the path component', () => {
  // The /v1 prefix is REST's path, the server's, and the SDK rebuilds it on every call. A user
  // path is never what the form needs.
  const endpoint = serverEndpointFromUrl('http://localhost:8443/v1/old?x=y');
  assert.equal(endpoint.host, 'localhost');
  assert.equal(endpoint.port, 8443);
  assert.equal(endpoint.gatewayPort, 8444);
  assert.equal(endpoint.scheme, 'Ws');
  assert.equal(endpoint.restScheme, 'Http');
});

test('serverEndpointFromUrl defaults an http URL to plain WebSocket on a loopback host', () => {
  const endpoint = serverEndpointFromUrl('http://127.0.0.1:18080');
  assert.equal(endpoint.scheme, 'Ws');
  assert.equal(endpoint.restScheme, 'Http');
});

test('the default-port rule: the gateway port defaults to rest+1 on a loopback and matches the rest port on a non-loopback', () => {
  const loopback = defaultLoopbackServerEndpoint('localhost', 18080);
  assert.equal(loopback.gatewayPort - loopback.port, 1);

  const internet = defaultInternetServerEndpoint('migo.example.com', 443);
  assert.equal(internet.gatewayPort, internet.port);
});

test('serverEndpointFromUrl honours the same default-port rule', () => {
  // A plain origin on a loopback keeps the dev split — gateway on the next port.
  const loopback = serverEndpointFromUrl('http://localhost:18080');
  assert.equal(loopback.gatewayPort - loopback.port, 1);

  // Every other origin — TLS or plain — serves /ws on the port the URL names: the from-url
  // form is for env or legacy settings, where the URL is the only evidence of the deployment's
  // shape and a plain public origin names a single-port deployment.
  const real = serverEndpointFromUrl('https://migo.example.com:8443');
  assert.equal(real.gatewayPort, real.port);
  const singlePortPlain = serverEndpointFromUrl('http://migo.example.com:8080');
  assert.equal(singlePortPlain.gatewayPort, singlePortPlain.port);
});

test('validateServerEndpoint returns null for a well-formed endpoint', () => {
  const endpoint = defaultLoopbackServerEndpoint('localhost', 18080);
  assert.equal(validateServerEndpoint(endpoint).error, null);
});

test('validateServerEndpoint rejects an empty host', () => {
  const result = validateServerEndpoint({
    ...defaultLoopbackServerEndpoint('localhost'),
    host: '',
  });
  assert.match(result.error ?? '', /host/);
});

test('validateServerEndpoint rejects a port that is not in [1, 65535]', () => {
  for (const bad of [0, 65536, -1, 1.5]) {
    const result = validateServerEndpoint({
      ...defaultLoopbackServerEndpoint('localhost'),
      port: bad,
    });
    assert.ok(
      result.error !== null && /port/.test(result.error),
      `expected an error for port ${bad}, got ${JSON.stringify(result)}`,
    );
  }
});

test('validateServerEndpoint rejects a gateway port that is not in [1, 65535]', () => {
  const result = validateServerEndpoint({
    ...defaultLoopbackServerEndpoint('localhost'),
    gatewayPort: 0,
  });
  assert.match(result.error ?? '', /gateway port/);
});

test('validateServerEndpoint rejects a WebSocket transport with a QUIC scheme', () => {
  const result = validateServerEndpoint({
    ...defaultLoopbackServerEndpoint('localhost'),
    transport: 'WebSocket',
    scheme: 'Quic',
  });
  assert.match(result.error ?? '', /WS or WSS/);
});

test('validateServerEndpoint rejects a QUIC transport with a WebSocket scheme', () => {
  const result = validateServerEndpoint({
    ...defaultLoopbackServerEndpoint('localhost'),
    transport: 'Quic',
    scheme: 'Wss',
  });
  assert.match(result.error ?? '', /QUIC or QUIC-TLS/);
});

test('validateServerEndpoint accepts a QUIC transport with a QUIC-TLS scheme', () => {
  const result = validateServerEndpoint({
    ...defaultLoopbackServerEndpoint('localhost'),
    transport: 'Quic',
    scheme: 'QuicTls',
    restScheme: 'Https',
  });
  assert.equal(result.error, null);
});

test('assertValidServerEndpoint throws on a bad shape and accepts a good one', () => {
  const good: ServerEndpoint = defaultInternetServerEndpoint('migo.example.com', 443);
  assertValidServerEndpoint(good);
  assert.throws(
    () =>
      assertValidServerEndpoint({
        ...good,
        port: 0,
      }),
    ServerEndpointError,
  );
});
