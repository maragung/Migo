/**
 * The register form must drive the SDK into the right body shape on the wire.
 *
 * The previous release shipped a register page that only filled `username`/`password`/`email` and
 * handed them to the provider. The captcha gate the server had just been turned on meant a fresh
 * registration from an engaged network always failed with `CAPTCHA_REQUIRED` and the user saw "The
 * server rejected the request." with no path forward. Three things have to be true for the form to
 * work end to end:
 *
 *   1. The page mounts a captcha widget so the user is asked for a challenge answer.
 *   2. The widget fetches `/v1/auth/captcha` against the user-picked server and exposes the proof.
 *   3. The provider's `register` carries the captcha through to the wire body so the server
 *      verifies it.
 *
 * These tests pin the third invariant by driving `MigoClient.register` through a `fetch` double and
 * asserting the JSON body has the right fields, pin the first two by rendering the captcha and
 * server widgets (the only pieces of the page that do not pull in `next/navigation`) and checking
 * the challenge image, the answer input, and the refresh and easier-challenge controls are in the
 * markup, and pin the challenge's own wire shape by driving `BootstrapClient.requestCaptcha`
 * through the same double. A regression that removed the widget, broke the wire field name, or
 * stripped the captcha from the body would land in one of these assertions and never in a less
 * observable place.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { BootstrapClient, MigoClient, Platform, BandwidthMode } from '@migo/sdk';
import type { CaptchaProof, Grant, Id, ServerEndpoint } from '@migo/sdk';

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
// The answer a user would have read off the rendered challenge image: five to six
// letters and digits, nothing else.
const CAPTCHA: CaptchaProof = {
  challenge_id: '01ARZ3NDEKTSV4RRFFQ69G5FAV' as Id,
  answer: 'AB3D7',
};

type CapturedCall = { url: string; init: RequestInit };

function makeFetchDouble(calls: CapturedCall[]): typeof fetch {
  return (input, init) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    calls.push({ url, init: init ?? {} });
    // The double answers every endpoint the bootstrap surface hits. The challenge is an
    // image now — a base64 PNG plus the mode the server issued — while the grant keeps
    // the shape register has always parsed.
    const isCaptcha = url.endsWith('/v1/auth/captcha');
    const body = isCaptcha
      ? {
          challenge_id: '01ARZ3NDEKTSV4RRFFQ69G5FAV',
          image_png_base64: 'aGVsbG8=',
          mode: 'image',
          ttl_seconds: 120,
        }
      : {
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
        status: isCaptcha ? 200 : 201,
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

test('the captcha widget renders the challenge image, the answer input, and the challenge controls', async () => {
  const { CaptchaWidget } = await import('../src/components/captcha-widget.js');
  const markup = renderToStaticMarkup(
    <CaptchaWidget endpoint={ENDPOINT} onChange={() => undefined} />,
  );
  assert.ok(markup.includes('class="captcha-widget"'), 'captcha widget must render its shell');
  assert.ok(markup.includes('<img'), 'the challenge must render as an image element');
  assert.ok(
    markup.includes('alt="'),
    'the challenge image must carry an accessible description (never the answer)',
  );
  assert.ok(markup.includes('Captcha'), 'captcha label must be visible to the user');
  assert.ok(markup.includes('Answer'), 'the captcha answer input must be visible');
  assert.ok(
    markup.includes('placeholder="5–6 characters"'),
    'the answer field must take five to six letters and digits',
  );
  assert.ok(
    markup.includes('aria-label="Request a new captcha"'),
    'the refresh control must be present',
  );
  assert.ok(
    markup.includes('aria-label="Request an easier-to-read captcha"'),
    'the easier-challenge control must be present',
  );
  assert.ok(markup.includes('Easier challenge'), 'the easier-challenge control must be labelled');
});

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

test('requestCaptcha posts the chosen mode and parses the image challenge', async () => {
  const calls: CapturedCall[] = [];
  const bootstrap = new BootstrapClient(ENDPOINT, { fetch: makeFetchDouble(calls) });

  const challenge = await bootstrap.requestCaptcha();
  const altChallenge = await bootstrap.requestCaptcha('image_alt');

  const captchaCalls = calls.filter((call) => call.url.endsWith('/v1/auth/captcha'));
  assert.equal(captchaCalls.length, 2, 'one request per requestCaptcha call');
  // The SDK always serializes the body as a JSON string, but the RequestInit type only says
  // `body: BodyInit | null | undefined`, so the cast is the documented seam.
  // eslint-disable-next-line @typescript-eslint/no-base-to-string, @typescript-eslint/no-unsafe-assignment
  const defaultBody: Record<string, unknown> = JSON.parse(String(captchaCalls[0]?.init.body));
  // eslint-disable-next-line @typescript-eslint/no-base-to-string, @typescript-eslint/no-unsafe-assignment
  const altBody: Record<string, unknown> = JSON.parse(String(captchaCalls[1]?.init.body));
  assert.deepEqual(defaultBody, {}, 'an omitted mode posts an empty object body');
  assert.deepEqual(altBody, { mode: 'image_alt' }, 'a given mode rides in the body');
  assert.equal(challenge.image_png_base64, 'aGVsbG8=', 'the image bytes cross as base64');
  assert.equal(challenge.mode, 'image', 'the issued mode is echoed back');
  assert.equal(challenge.ttl_seconds, 120, 'the ttl crosses as a number');
  assert.equal(
    altChallenge.challenge_id,
    challenge.challenge_id,
    'the challenge id parses into the SDK id type',
  );
});

test('MigoClient.register sends a captcha proof in the body when one is supplied', async () => {
  const calls: CapturedCall[] = [];
  const client = MigoClient.create({
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

  try {
    const grant: Grant = await client.register({
      username: USERNAME,
      password: PASSWORD,
      captcha: CAPTCHA,
    });
    assert.equal(grant.isNewAccount, true);
  } catch {
    // The post-grant handshake will fail because `globalThis.WebSocket` is absent under node; that
    // is fine — the bootstrap call is the one we are asserting on, and it has already run.
  }

  const registerCall = calls.find((c) => c.url.endsWith('/v1/auth/register'));
  assert.ok(registerCall !== undefined, 'a /v1/auth/register POST must have been made');
  const init = registerCall.init;
  assert.equal(init.method, 'POST');
  const headers = init.headers as Record<string, string>;
  assert.equal(headers['content-type'], 'application/json');
  // The SDK always serializes the body as a JSON string, but the RequestInit type only says
  // `body: BodyInit | null | undefined`, so the cast is the documented seam.
  // eslint-disable-next-line @typescript-eslint/no-base-to-string, @typescript-eslint/no-unsafe-assignment
  const body: Record<string, unknown> = JSON.parse(String(init.body));
  assert.deepEqual(body.username, USERNAME, 'username is forwarded as-is');
  assert.deepEqual(body.password, PASSWORD, 'password is forwarded as-is');
  const device = body.device as Record<string, unknown>;
  assert.ok(device !== undefined, 'the device block is present');
  assert.deepEqual(device.display_name, 'Test Browser', 'the display name is forwarded');
  assert.deepEqual(device.platform, 'web', 'the platform is the snake_case wire name');
  assert.deepEqual(
    body.captcha,
    { challenge_id: CAPTCHA.challenge_id, answer: CAPTCHA.answer },
    'the captcha proof is in the body verbatim',
  );
});

test('MigoClient.register omits the captcha block when no proof is supplied', async () => {
  const calls: CapturedCall[] = [];
  const client = MigoClient.create({
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
  try {
    await client.register({
      username: USERNAME,
      password: PASSWORD,
    });
  } catch {
    // The socket handshake will fail under node; the bootstrap call is the one we assert on.
  }
  const registerCall = calls.find((c) => c.url.endsWith('/v1/auth/register'));
  assert.ok(registerCall !== undefined, 'a /v1/auth/register POST must have been made');
  // eslint-disable-next-line @typescript-eslint/no-base-to-string, @typescript-eslint/no-unsafe-assignment
  const body: Record<string, unknown> = JSON.parse(String(registerCall.init.body));
  assert.equal(body.captcha, undefined, 'no captcha key is sent when no proof is supplied');
});
