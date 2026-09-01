/**
 * The chain domain: the RPC conversation with a pinned public network.
 *
 * The {@link ChainClient} is the one domain that never talks to a Migo server, so its double is
 * not the gateway transport but a fake `fetch` that answers JSON-RPC — and the tests care about
 * the parts of the conversation that carry security weight, not the happy plumbing:
 *
 * * the session rule — `eth_chainId` is the first request, and a mismatched answer closes the
 *   session before a single balance, nonce, or transaction byte is asked for;
 * * `broadcast` re-verifies the chain at the moment value-carrying bytes leave, and refuses an
 *   RPC that answers a foreign hash — the hash is the handle the user tracks the send by;
 * * `track` never turns "the RPC accepted it" into `CONFIRMED`: only a receipt with `status: 1`
 *   does that, a `status: 0` receipt is `REVERTED`, a vanished transaction is `DROPPED`, and a
 *   deadline is `EXPIRED` — an unresolved ending, never a quiet success.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { AccountError, account } from '@migo/crypto';

import { ChainClient, ChainError, FUJI_TESTNET } from '../src/index.js';
import type { FetchLike } from '../src/index.js';

/**
 * A JSON-RPC endpoint double: routes by method, records every request, and answers from a script
 * the test mutates between calls (a poll loop must see different answers on later rounds).
 */
class FakeChain {
  readonly requests: Array<{ method: string; params: unknown[] }> = [];
  #handlers = new Map<string, (params: unknown[]) => unknown>();

  on(method: string, handler: (params: unknown[]) => unknown): void {
    this.#handlers.set(method, handler);
  }

  callsTo(method: string): number {
    return this.requests.filter((request) => request.method === method).length;
  }

  fetch: FetchLike = (_input, init) => {
    // The ChainClient always sends a JSON string body, so the cast is the double's whole job.
    const body = JSON.parse((init?.body ?? '{}') as string) as {
      method: string;
      params: unknown[];
    };
    this.requests.push(body);
    const handler = this.#handlers.get(body.method);
    if (handler === undefined) {
      return Promise.resolve(
        new Response(
          JSON.stringify({
            jsonrpc: '2.0',
            id: 1,
            error: { code: -32601, message: `no handler: ${body.method}` },
          }),
          { status: 200 },
        ),
      );
    }
    return Promise.resolve(
      Promise.resolve(handler(body.params)).then(
        (result) =>
          new Response(JSON.stringify({ jsonrpc: '2.0', id: 1, result }), { status: 200 }),
        (error: unknown) =>
          new Response(
            JSON.stringify({
              jsonrpc: '2.0',
              id: 1,
              error: { code: -32000, message: String(error) },
            }),
            { status: 200 },
          ),
      ),
    );
  };
}

/** A Fuji client over a fresh double, with the chain id answered correctly by default. */
function fujiClient(): { chain: ChainClient; fake: FakeChain } {
  const fake = new FakeChain();
  fake.on('eth_chainId', () => '0xa869'); // 43113, Fuji
  const chain = new ChainClient({ network: FUJI_TESTNET, fetch: fake.fetch });
  return { chain, fake };
}

test('the session opens with eth_chainId and refuses a mismatched network', async () => {
  const { chain, fake } = fujiClient();
  fake.on('eth_getBalance', () => '0x1');

  // The first request the endpoint ever sees is the chain id check.
  await chain.getBalance(new Uint8Array(20));
  assert.equal(fake.requests[0]?.method, 'eth_chainId');
  assert.equal(fake.requests[1]?.method, 'eth_getBalance');

  // A session whose chain id disagrees is closed before any other request: no balance was asked.
  const wrong = new FakeChain();
  wrong.on('eth_chainId', () => '0xa86a'); // 43114 — mainnet, not the configured Fuji
  const confused = new ChainClient({ network: FUJI_TESTNET, fetch: wrong.fetch });
  await assert.rejects(confused.getBalance(new Uint8Array(20)), (error: unknown) => {
    assert.ok(error instanceof AccountError);
    assert.equal(error.kind, 'ChainMismatch');
    return true;
  });
  assert.equal(wrong.requests.length, 1, 'the mismatched session asked nothing else');
});

test('balances, nonces, gas, and fees are parsed from hex quantities', async () => {
  const { chain, fake } = fujiClient();
  const address = new Uint8Array(20).fill(0xab);
  const oneAvax = 1_000_000_000_000_000_000n;

  fake.on('eth_getBalance', () => '0xde0b6b3a7640000');
  assert.equal(await chain.getBalance(address), oneAvax);

  fake.on('eth_getTransactionCount', () => '0x2a');
  assert.equal(await chain.getNonce(address), 42);

  fake.on('eth_estimateGas', () => '0x5208');
  assert.equal(
    await chain.estimateGas({ to: address, value: oneAvax, data: new Uint8Array(0) }),
    21000,
  );

  fake.on('eth_maxPriorityFeePerGas', () => '0x77359400'); // 2 gwei
  fake.on('eth_gasPrice', () => '0x6fc23ac00'); // 30 gwei
  const fees = await chain.getFees();
  assert.equal(fees.maxPriorityFeePerGas, 2_000_000_000n);
  assert.equal(fees.maxFeePerGas, 32_000_000_000n); // ceiling, above the observed price

  // The address travels as 0x-prefixed lowercase hex, and the balance read is against "latest".
  const balanceCall = fake.requests.find((request) => request.method === 'eth_getBalance');
  assert.equal(balanceCall?.params[0], '0x' + 'ab'.repeat(20));
  assert.equal(balanceCall?.params[1], 'latest');
});

test('broadcast re-verifies the chain and refuses a foreign answered hash', async () => {
  const { chain, fake } = fujiClient();
  const root = account.MigoRoot.fromBytes(new Uint8Array(32).fill(0x5a));
  const wallet = account.EvmWallet.fromRoot(root, 0);
  const tx = new account.Eip1559Tx({
    chainId: 43113,
    nonce: 0,
    maxPriorityFeePerGas: 2_000_000_000n,
    maxFeePerGas: 30_000_000_000n,
    gasLimit: 21000,
    to: new Uint8Array(20).fill(0xcd),
    value: 1n,
    data: new Uint8Array(0),
  });
  const signed = tx.sign(wallet);

  // The session was already verified by a read; broadcast checks the chain id *again*, at the one
  // moment value-carrying bytes leave.
  fake.on('eth_getBalance', () => '0x1');
  await chain.getBalance(wallet.address());
  const before = fake.callsTo('eth_chainId');

  fake.on('eth_sendRawTransaction', () => signed.txHashHex());
  const answered = await chain.broadcast(signed);
  assert.equal(answered, signed.txHashHex());
  assert.equal(
    fake.callsTo('eth_chainId'),
    before + 1,
    'broadcast re-verifies the chain id after the session was already verified',
  );
  const sent = fake.requests.find((request) => request.method === 'eth_sendRawTransaction')
    ?.params[0] as string;
  assert.equal(sent.slice(0, 4), '0x02');
  assert.equal(
    sent.length,
    2 + signed.raw().length * 2,
    'the raw transaction travels hex-encoded, type byte first',
  );

  // An endpoint that answers a different hash than Keccak-256(raw) is refused: the tracker would
  // follow someone else's transaction to its ending.
  fake.on('eth_sendRawTransaction', () => '0x' + '00'.repeat(32));
  await assert.rejects(chain.broadcast(signed), (error: unknown) => {
    assert.ok(error instanceof ChainError);
    assert.match(error.message, /foreign hash/);
    return true;
  });
});

test('a chain error from the endpoint carries the JSON-RPC code', async () => {
  const { chain, fake } = fujiClient();
  fake.on('eth_getBalance', () => Promise.reject(new Error('insufficient funds for gas')));
  await assert.rejects(chain.getBalance(new Uint8Array(20)), (error: unknown) => {
    assert.ok(error instanceof ChainError);
    assert.equal(error.code, -32000);
    return true;
  });
});

test('track confirms only through a receipt with status 1', async () => {
  const { chain, fake } = fujiClient();
  const txHash = '0x' + '11'.repeat(32);
  const states: string[] = [];

  // The receipt arrives on the second poll, so the tracker first sees the transaction in the
  // mempool (PENDING) and only then in a block (CONFIRMED) — the two states spec #41 keeps apart.
  let mined = false;
  fake.on('eth_getTransactionReceipt', () =>
    mined ? { status: '0x1', blockNumber: '0x2a', gasUsed: '0x5208' } : null,
  );
  fake.on('eth_getTransactionByHash', () => ({ hash: txHash }));
  setTimeout(() => {
    mined = true;
  }, 5);

  const result = await chain.track(txHash, {
    initialIntervalMs: 1,
    onState: (state) => states.push(state),
  });
  assert.equal(result.outcome, 'CONFIRMED');
  assert.equal(result.blockNumber, 42);
  assert.equal(result.gasUsed, 21000n);
  assert.deepEqual(states, ['PENDING', 'CONFIRMED']);
});

test('track reports a status-0 receipt as REVERTED, never as confirmed', async () => {
  const { chain, fake } = fujiClient();
  fake.on('eth_getTransactionReceipt', () => ({
    status: '0x0',
    blockNumber: '0x2a',
    gasUsed: '0x5208',
  }));
  fake.on('eth_getTransactionByHash', () => ({}));
  const result = await chain.track('0x' + '22'.repeat(32), { initialIntervalMs: 1 });
  assert.equal(result.outcome, 'REVERTED');
});

test('track reports a vanished transaction as DROPPED', async () => {
  const { chain, fake } = fujiClient();
  const txHash = '0x' + '33'.repeat(32);
  // In the mempool on the first look, gone by the second.
  let seenOnce = false;
  fake.on('eth_getTransactionReceipt', () => null);
  fake.on('eth_getTransactionByHash', () => {
    if (!seenOnce) {
      seenOnce = true;
      return { hash: txHash };
    }
    return null;
  });
  const result = await chain.track(txHash, { initialIntervalMs: 1, maxIntervalMs: 1 });
  assert.equal(result.outcome, 'DROPPED');
});

test('track reports a deadline as EXPIRED, an unresolved ending', async () => {
  const { chain, fake } = fujiClient();
  fake.on('eth_getTransactionReceipt', () => null);
  fake.on('eth_getTransactionByHash', () => ({})); // still in the mempool, never mined
  const result = await chain.track('0x' + '44'.repeat(32), {
    initialIntervalMs: 1,
    maxIntervalMs: 1,
    timeoutMs: 5,
  });
  assert.equal(result.outcome, 'EXPIRED');
});
