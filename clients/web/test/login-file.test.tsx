/**
 * The file sign-in's wire shapes and its device half.
 *
 * The sign-in screen hands the provider a `.migo` file and a passphrase, and the provider walks the
 * ML-DSA identity ceremonies (§182). The ceremony code is the SDK's; what these tests pin is that
 * the calls the web client relies on really put what the server validates on the wire —
 *
 *   1. a login challenge is asked for by purpose + identifier + the stored device id,
 *   2. the login answer carries both signatures as standard base64,
 *   3. an add-device challenge names the account and describes the device,
 *   4. the add-device answer introduces the new credential's public key with its signature,
 *
 * — and that the device record, which is what lets the *next* sign-in reuse the same device
 * instead of minting a new one against the eight-device cap, round-trips through the sanctioned
 * store with its seed intact. A regression that renamed a wire field or dropped the seed would
 * land here, not in a support conversation.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { BootstrapClient, Platform, account } from '@migo/sdk';
import type { Id, ServerEndpoint } from '@migo/sdk';

import {
  clearDeviceRecord,
  loadDeviceRecord,
  saveDeviceRecord,
} from '../src/lib/storage/device-record-store.js';
import { installFakeIndexedDb } from './support/dom-stubs.js';

const HOST = 'migo.test';
const ENDPOINT: ServerEndpoint = {
  host: HOST,
  port: 18080,
  gatewayPort: 18081,
  transport: 'WebSocket',
  scheme: 'Ws',
  restScheme: 'Http',
};

const CHALLENGE_ID = '01ARZ3NDEKTSV4RRFFQ69G5FAV';
const ACCOUNT_ID = '01ARZ3NDEKTSV4RRFFQ69G5FAW' as Id;
const DEVICE_ID = '01ARZ3NDEKTSV4RRFFQ69G5FAX' as Id;
const USERNAME = 'alice';

type CapturedCall = { url: string; init: RequestInit };

/**
 * Answers every ceremony endpoint the file sign-in walks. The challenge's payload is opaque bytes
 * the client signs as given, so any 32 bytes stand in for it; the grant is the shape every
 * bootstrap answer parses.
 */
function makeFetchDouble(calls: CapturedCall[]): typeof fetch {
  return (input, init) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    calls.push({ url, init: init ?? {} });
    const isChallenge = url.endsWith('/v1/auth/identity/challenge');
    const body = isChallenge
      ? {
          challenge_id: CHALLENGE_ID,
          payload: Buffer.from('0123456789abcdef0123456789abcdef').toString('base64'),
          device_id: DEVICE_ID,
          expires_at_ms: 9_000_000_000_000,
        }
      : {
          account_id: ACCOUNT_ID,
          device_id: DEVICE_ID,
          session_id: '01ARZ3NDEKTSV4RRFFQ69G5FAY',
          access_token: 'access',
          refresh_token: 'refresh',
          access_expires_at_ms: 1,
          refresh_expires_at_ms: 2,
          capabilities: '1',
          is_new_account: false,
        };
    return Promise.resolve(
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
  };
}

function bodyOf(calls: CapturedCall[], suffix: string): Record<string, unknown> {
  const call = calls.find((c) => c.url.endsWith(suffix));
  assert.ok(call !== undefined, `a request to ${suffix} must have been made`);
  // The SDK always serializes the body as a JSON string, but the RequestInit type only says
  // `body: BodyInit | null | undefined`, so the cast is the documented seam.
  // eslint-disable-next-line @typescript-eslint/no-base-to-string, @typescript-eslint/no-unsafe-assignment
  const parsed: Record<string, unknown> = JSON.parse(String(call.init.body));
  return parsed;
}

test('the login challenge is asked for by purpose, identifier, and the stored device id', async () => {
  const calls: CapturedCall[] = [];
  const bootstrap = new BootstrapClient(ENDPOINT, { fetch: makeFetchDouble(calls) });

  const challenge = await bootstrap.identityLoginChallenge({
    identifier: USERNAME,
    deviceId: DEVICE_ID,
  });

  const body = bodyOf(calls, '/v1/auth/identity/challenge');
  assert.deepEqual(
    body,
    { purpose: 'login', identifier: USERNAME, device_id: DEVICE_ID },
    'the login challenge body is exactly the three fields the server validates',
  );
  assert.equal(
    challenge.payload.length,
    32,
    'the payload arrives decoded, ready for signLogin without re-encoding',
  );
  assert.equal(String(challenge.challengeId), CHALLENGE_ID, 'the challenge id parses');
});

test('the login answer carries both signatures as standard base64', async () => {
  const calls: CapturedCall[] = [];
  const bootstrap = new BootstrapClient(ENDPOINT, { fetch: makeFetchDouble(calls) });

  // A deterministic root and seed, so the signatures are stable and the assertion can compare
  // bytes rather than shape.
  const root = account.MigoRoot.fromBytes(new Uint8Array(32).fill(7));
  const credential = account.DeviceCredential.fromSeed(new Uint8Array(32).fill(9));
  const payload = new Uint8Array(32).fill(3);
  const identitySignature = account.IdentityKey.fromRoot(root).signLogin(payload);
  const deviceSignature = credential.signLogin(payload);

  await bootstrap.identityLogin({
    challengeId: CHALLENGE_ID as Id,
    identitySignature,
    deviceSignature,
  });

  const body = bodyOf(calls, '/v1/auth/identity/login');
  assert.equal(body.challenge_id, CHALLENGE_ID, 'the challenge id crosses as given');
  assert.deepEqual(
    new Uint8Array(Buffer.from(String(body.identity_signature), 'base64')),
    identitySignature,
    'the identity signature round-trips to the exact bytes signed under the login context',
  );
  assert.deepEqual(
    new Uint8Array(Buffer.from(String(body.device_signature), 'base64')),
    deviceSignature,
    'the device signature round-trips to the exact bytes signed under the device context',
  );
});

test('the add-device challenge names the account and describes the device', async () => {
  const calls: CapturedCall[] = [];
  const bootstrap = new BootstrapClient(ENDPOINT, { fetch: makeFetchDouble(calls) });

  await bootstrap.addDeviceChallenge({
    accountId: ACCOUNT_ID,
    device: { platform: Platform.Web, displayName: 'Migo Web (Test)' },
  });

  const body = bodyOf(calls, '/v1/auth/identity/challenge');
  assert.equal(body.purpose, 'add-device', 'the purpose distinguishes the ceremony');
  assert.equal(body.account_id, ACCOUNT_ID, 'the account comes from the opened container');
  const device = body.device as Record<string, unknown>;
  assert.equal(
    device.display_name,
    'Migo Web (Test)',
    'the device block is snake_case on the wire',
  );
});

test('the add-device answer introduces the new credential with both keys it needs', async () => {
  const calls: CapturedCall[] = [];
  const bootstrap = new BootstrapClient(ENDPOINT, { fetch: makeFetchDouble(calls) });

  const root = account.MigoRoot.fromBytes(new Uint8Array(32).fill(7));
  const credential = account.DeviceCredential.fromSeed(new Uint8Array(32).fill(9));
  const payload = new Uint8Array(32).fill(3);

  await bootstrap.addDevice({
    challengeId: CHALLENGE_ID as Id,
    identitySignature: account.IdentityKey.fromRoot(root).signLogin(payload),
    devicePublicKey: credential.publicKey(),
    deviceSignature: credential.signLogin(payload),
  });

  const body = bodyOf(calls, '/v1/auth/identity/add-device');
  assert.equal(body.challenge_id, CHALLENGE_ID);
  assert.deepEqual(
    new Uint8Array(Buffer.from(String(body.device_public_key), 'base64')),
    credential.publicKey(),
    'the new credential public key crosses as the exact bytes the device will sign with',
  );
});

test('the device record round-trips with its seed, and only for its own account', async () => {
  const fake = installFakeIndexedDb();
  try {
    const seed = account.DeviceCredential.generate().exposeSeed();
    await saveDeviceRecord({
      accountId: ACCOUNT_ID,
      deviceId: DEVICE_ID,
      username: USERNAME,
      credentialSeed: seed,
      savedAt: 1_000,
    });

    const record = await loadDeviceRecord(ACCOUNT_ID);
    assert.ok(record !== undefined, 'the record must read back after a save');
    assert.equal(record.username, USERNAME, 'the username survives for the next challenge');
    assert.equal(String(record.deviceId), DEVICE_ID, 'the device id survives');
    assert.deepEqual(
      new Uint8Array(record.credentialSeed),
      seed,
      'the credential seed survives byte for byte — it is the device half of the ceremony',
    );
    // The seed is live key material: what read back must be usable as the credential itself.
    assert.deepEqual(
      account.DeviceCredential.fromSeed(new Uint8Array(record.credentialSeed)).signLogin(
        new Uint8Array(32).fill(1),
      ),
      account.DeviceCredential.fromSeed(seed).signLogin(new Uint8Array(32).fill(1)),
      'a record-read seed signs exactly as the seed that was saved',
    );

    assert.equal(
      await loadDeviceRecord('01ARZ3NDEKTSV4RRFFQ69G5FAZ' as Id),
      undefined,
      'another account has no record just because this one does',
    );

    await clearDeviceRecord(ACCOUNT_ID);
    assert.equal(await loadDeviceRecord(ACCOUNT_ID), undefined, 'clearing forgets the record');
  } finally {
    fake.restore();
  }
});
