/**
 * The register form must drive the SDK into the right body shape on the wire.
 *
 * The register page collects username, email, passphrase, and gender, and the captcha card is
 * gone from the web client: the production server runs with the gate disabled and the identity
 * ceremonies are never captcha-gated anyway. What has to stay true for the form to work end to
 * end:
 *
 *   1. The provider's `register` puts every field the server validates into the wire body —
 *      including the disclosed gender, in the server's numbering — and omits the ones the form
 *      left blank rather than sending them empty.
 *   2. The identity key crosses as the standard base64 of exactly the bytes the root derives.
 *
 * These tests pin both invariants by driving `MigoClient.register` through a `fetch` double and
 * asserting the JSON body has the right fields. A regression that broke the wire shape (a renamed
 * field, a wrong id type, a gender sent as a string) would land in one of these assertions and
 * never in a less observable place.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { MigoClient, Platform, BandwidthMode } from '@migo/sdk';
import type { Grant, ServerEndpoint } from '@migo/sdk';

const HOST = 'migo.test';
const ENDPOINT: ServerEndpoint = {
  host: HOST,
  port: 18080,
  gatewayPort: 18081,
  transport: 'WebSocket',
  scheme: 'Ws',
  restScheme: 'Http',
};

const USERNAME = 'alice';
const PASSWORD = 'correct-horse-battery-staple';

type CapturedCall = { url: string; init: RequestInit };

function makeFetchDouble(calls: CapturedCall[]): typeof fetch {
  return (input, init) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    calls.push({ url, init: init ?? {} });
    const body = {
      account_id: '01ARZ3NDEKTSV4RRFFQ69G5FAV',
      device_id: '01ARZ3NDEKTSV4RRFFQ69G5FAW',
      session_id: '01ARZ3NDEKTSV4RRFFQ69G5FAX',
      access_token: 'access',
      refresh_token: 'refresh',
      access_expires_at_ms: 1,
      refresh_expires_at_ms: 2,
      capabilities: '1',
      is_new_account: true,
    };
    return Promise.resolve(
      new Response(JSON.stringify(body), {
        status: 201,
        headers: { 'content-type': 'application/json' },
      }),
    );
  };
}

/**
 * A socket factory that refuses, so the post-grant handshake fails here rather than on the network.
 *
 * These tests assert on the REST body `register` sends, and the handshake that follows it is not
 * under test. Leaving it to the global `WebSocket` made the outcome depend on the host: node 22 and
 * later ship one, so the transport really tried to resolve the test hostname and left a promise
 * pending after the assertions had run, which the test runner reports as a failure in the test that
 * happened to be open. Refusing synchronously keeps the failure inside the `catch` the test already
 * has, and keeps the suite off the network.
 */
function refusingSocket(): never {
  throw new Error('the handshake is not under test in this file');
}

function makeClient(calls: CapturedCall[]): MigoClient {
  return MigoClient.create({
    server: ENDPOINT,
    hello: {
      platform: Platform.Web,
      appVersion: 'test',
      locale: 'en-US',
      bandwidthMode: BandwidthMode.Auto,
    },
    deviceDisplayName: 'Test Browser',
    fetch: makeFetchDouble(calls),
    webSocketFactory: refusingSocket,
  });
}

function registerBodyOf(calls: CapturedCall[]): Record<string, unknown> {
  const registerCall = calls.find((c) => c.url.endsWith('/v1/auth/register'));
  assert.ok(registerCall !== undefined, 'a /v1/auth/register POST must have been made');
  // The SDK always serializes the body as a JSON string, but the RequestInit type only says
  // `body: BodyInit | null | undefined`, so the cast is the documented seam.
  // eslint-disable-next-line @typescript-eslint/no-base-to-string, @typescript-eslint/no-unsafe-assignment
  const parsed: Record<string, unknown> = JSON.parse(String(registerCall.init.body));
  return parsed;
}

test('the server form renders the picker body: transport, host, port, scheme, and the commit control', async () => {
  const { ServerForm } = await import('../src/components/server-form.js');
  const markup = renderToStaticMarkup(<ServerForm value={ENDPOINT} onCommit={() => undefined} />);
  assert.ok(markup.includes('class="server-form"'), 'the picker body must render its shell');
  assert.ok(
    markup.includes('aria-label="Realtime transport"'),
    'the transport segmented control must be present',
  );
  assert.ok(markup.includes('WebSocket'), 'the WebSocket transport must be offered');
  assert.ok(markup.includes('QUIC'), 'the QUIC transport must be offered');
  assert.ok(markup.includes('placeholder="migo.example.com"'), 'the host field must be present');
  assert.ok(markup.includes('placeholder="18080"'), 'the port field must be present');
  assert.ok(
    markup.includes('Use this server'),
    'the commit control must name what it does with the choice',
  );
});

test('MigoClient.register sends the username, password, email, and device block', async () => {
  const calls: CapturedCall[] = [];
  const client = makeClient(calls);

  try {
    const grant: Grant = await client.register({
      username: USERNAME,
      password: PASSWORD,
      email: 'alice@example.com',
    });
    assert.equal(grant.isNewAccount, true);
  } catch {
    // The post-grant handshake will fail because the socket factory refuses; that is fine — the
    // bootstrap call is the one we are asserting on, and it has already run.
  }

  const init = calls.find((c) => c.url.endsWith('/v1/auth/register'))?.init;
  assert.ok(init !== undefined);
  assert.equal(init.method, 'POST');
  const headers = init.headers as Record<string, string>;
  assert.equal(headers['content-type'], 'application/json');
  const body = registerBodyOf(calls);
  assert.deepEqual(body.username, USERNAME, 'username is forwarded as-is');
  assert.deepEqual(body.password, PASSWORD, 'password is forwarded as-is');
  assert.deepEqual(body.email, 'alice@example.com', 'email is forwarded as-is');
  const device = body.device as Record<string, unknown>;
  assert.ok(device !== undefined, 'the device block is present');
  assert.deepEqual(device.display_name, 'Test Browser', 'the display name is forwarded');
  assert.deepEqual(device.platform, 'web', 'the platform is the snake_case wire name');
  assert.equal(body.captcha, undefined, 'no captcha key is sent when no proof is supplied');
  assert.equal(body.gender, undefined, 'an undisclosed gender is omitted, not sent empty');
});

test('MigoClient.register carries the disclosed gender in the server numbering', async () => {
  const calls: CapturedCall[] = [];
  const client = makeClient(calls);

  try {
    await client.register({
      username: USERNAME,
      password: PASSWORD,
      gender: 2,
    });
  } catch {
    // The handshake is not under test here either.
  }
  const body = registerBodyOf(calls);
  assert.equal(body.gender, 2, 'the gender must cross as the server numbering, a bare number');
  assert.equal(
    typeof body.gender,
    'number',
    'the gender must cross as a JSON number, not a string',
  );
});

test('MigoClient.register carries the identity key as base64 when a founding root is supplied', async () => {
  const calls: CapturedCall[] = [];
  const client = makeClient(calls);
  // A deterministic root, so the derived identity key is stable and the assertion can compare
  // bytes rather than shape: 32 bytes is what the account root is (§182).
  const { account, KeyStore } = await import('@migo/sdk');
  const root = account.MigoRoot.fromBytes(new Uint8Array(32).fill(7));
  const identityKey = account.IdentityKey.fromRoot(root).publicKey();

  try {
    await client.register({
      username: USERNAME,
      password: PASSWORD,
      identityPublicKey: identityKey,
    });
  } catch {
    // The socket handshake will fail under node; the bootstrap call is the one we assert on.
  }
  const body = registerBodyOf(calls);
  // The ML-DSA-65 public key is 1952 bytes; whatever its exact length here, the body must carry
  // the standard-base64 of exactly those bytes and nothing else.
  const encoded = body.identity_public_key;
  assert.equal(typeof encoded, 'string', 'the identity key crosses as a base64 string');
  const decoded = Buffer.from(String(encoded), 'base64');
  assert.deepEqual(
    new Uint8Array(decoded),
    identityKey,
    'the base64 round-trips to the exact key bytes the caller supplied',
  );

  // And the same call without the key must not carry the field at all, so a password-only
  // client's wire shape is unchanged.
  const bareCalls: CapturedCall[] = [];
  const bare = makeClient(bareCalls);
  try {
    await bare.register({ username: USERNAME, password: PASSWORD });
  } catch {
    // The handshake is not under test here either.
  }
  const bareBody = registerBodyOf(bareCalls);
  assert.equal(
    bareBody.identity_public_key,
    undefined,
    'no identity key is sent when the caller supplies none',
  );
  // The key store exercises the founding path too; referencing it keeps the import honest.
  assert.ok(KeyStore.founding(root).root() !== null, 'the founding key store holds the root');
});
