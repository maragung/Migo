/**
 * The REST client's transport fold: a fetch that never completes must arrive
 * as a {@link TransportError}, not as the platform's bare rejection.
 *
 * In a browser a refused connection, a network drop, or a CORS preflight the
 * server did not grant all reject fetch with a plain `TypeError`. Left
 * as-is, that shape is outside the SDK's error vocabulary, and every
 * caller's handling — the web client's included — degrades to an opaque
 * "unknown failure". The tests inject a fetch that rejects exactly the way a
 * browser's does and pin the class that comes out the other side.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { BootstrapClient, RemoteError, TransportError } from '../src/index.js';
import type { FetchLike, ServerEndpoint } from '../src/index.js';

const LOOPBACK: ServerEndpoint = {
  host: '127.0.0.1',
  port: 8080,
  gatewayPort: 8081,
  transport: 'WebSocket',
  scheme: 'Ws',
  restScheme: 'Http',
};

test('a fetch that rejects with a TypeError arrives as a TransportError', async () => {
  const client = new BootstrapClient(LOOPBACK, {
    fetch: () => {
      throw new TypeError('Failed to fetch');
    },
  });
  await assert.rejects(client.config(), (error: unknown) => {
    assert.ok(error instanceof TransportError, `expected TransportError, got ${String(error)}`);
    assert.equal(error.message, 'Failed to fetch');
    return true;
  });
});

test('a fetch that rejects without a message still names the failure', async () => {
  const client = new BootstrapClient(LOOPBACK, {
    fetch: () => Promise.reject(new Error()),
  });
  await assert.rejects(client.config(), (error: unknown) => {
    assert.ok(error instanceof TransportError, `expected TransportError, got ${String(error)}`);
    return true;
  });
});

test('a server verdict still arrives as a RemoteError, untouched by the fold', async () => {
  const client = new BootstrapClient(LOOPBACK, {
    fetch: () =>
      Promise.resolve(
        new Response(
          JSON.stringify({ error: { code: 1100, symbol: 'UNAUTHENTICATED', message: '' } }),
          {
            status: 401,
            headers: { 'content-type': 'application/json' },
          },
        ),
      ),
  });
  await assert.rejects(client.globalAdmins('token'), (error: unknown) => {
    assert.ok(error instanceof RemoteError, `expected RemoteError, got ${String(error)}`);
    assert.ok(!(error instanceof TransportError), 'a verdict is not a transport failure');
    return true;
  });
});

// --- the recovery-contact read -------------------------------------------------------------
//
// The security checkup's recovery row rests on exactly one boolean. The tests below inject the
// server's two answers — "yes" and "no contact" — plus the request the read actually issues,
// because the row's honesty depends on the method never doing more than this: a GET, one route,
// the caller's Bearer token, and a boolean back. The address itself is deliberately not on the
// wire (§48: there is no "what is my contact" read), and a `false` answer is the answer, not a
// failure the caller would have to tell apart from a broken route.

/** The one request a contactFetch served. */
interface ServedRequest {
  input: string;
  init: RequestInit | undefined;
}

/** A fetch that records the one request it served and answers it with `configured`. */
function contactFetch(configured: boolean): { served: ServedRequest[]; fetch: FetchLike } {
  const served: ServedRequest[] = [];
  return {
    served,
    fetch: (input: string, init?: RequestInit) => {
      served.push({ input, init });
      return Promise.resolve(
        new Response(JSON.stringify({ configured }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      );
    },
  };
}

test('the recovery-contact read answers the boolean, as a GET of the contact route with the Bearer token', async () => {
  const transport = contactFetch(true);
  const client = new BootstrapClient(LOOPBACK, transport);
  assert.deepEqual(await client.recoveryContact('the-token'), { configured: true });

  assert.equal(transport.served.length, 1, 'exactly one request: the read is not chatty');
  const { input, init } = transport.served[0]!;
  assert.ok(
    String(input).endsWith('/v1/auth/contact'),
    `expected the contact route, got ${String(input)}`,
  );
  assert.equal(init?.method, 'GET');
  assert.equal((init?.headers as Record<string, string>)['authorization'], 'Bearer the-token');
});

test('an account with no contact answers false — that is the answer, not an error', async () => {
  const client = new BootstrapClient(LOOPBACK, contactFetch(false));
  assert.deepEqual(await client.recoveryContact('the-token'), { configured: false });
});

test('a contact body missing the field reads as false rather than undefined leaking into a row', async () => {
  // A server that grew the route but not the field (or answered `{}` for an empty record)
  // must not turn into `{ configured: undefined }`, which renders as neither true nor false.
  const client = new BootstrapClient(LOOPBACK, {
    fetch: () =>
      Promise.resolve(
        new Response(JSON.stringify({}), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      ),
  });
  assert.deepEqual(await client.recoveryContact('the-token'), { configured: false });
});
