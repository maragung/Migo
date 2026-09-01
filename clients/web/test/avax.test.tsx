/**
 * What the AVAX surface is allowed to claim and accept (§184).
 *
 * The chain surface turns what the RPC answers into what a person reads, and turns what a person
 * typed into what a transaction carries. Both directions are pinned here, because both can be
 * wrong in ways no rendering test would catch:
 *
 *   1. **Amount arithmetic must be exact.** AVAX has 18 decimals and `Number` loses precision
 *      nine digits below one AVAX, so every conversion is `bigint` in and a trimmed decimal string
 *      out — the amount a person typed must be the amount they read back.
 *   2. **The parser refuses, never repairs.** A signed amount, a second dot, or a 19th fraction
 *      digit is not rounded or truncated to something sendable; it is refused before any RPC
 *      leaves.
 *   3. **A fee never overstates itself.** Before the receipt exists a send shows its confirmed
 *      ceiling; after, the gas the block actually spent.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { AVALANCHE_MAINNET, FUJI_TESTNET } from '@migo/sdk';
import type { TrackedTx } from '@migo/sdk';

import { avaxOf, hexOf, navaxOf, parseAvaxAmount } from '../src/lib/avax.js';
import { ChainTxLine, feeLabel, networkName } from '../src/components/avax-section.js';

const WEI = 10n ** 18n;

test('a wei amount renders as trimmed AVAX', () => {
  assert.equal(avaxOf(0n), '0');
  assert.equal(avaxOf(WEI), '1');
  assert.equal(avaxOf(WEI + WEI / 2n), '1.5');
  // The smallest fraction a transaction can carry, not collapsed to 0 or 1e-18.
  assert.equal(avaxOf(1n), '0.000000000000000001');
  // Trailing zeros are trimmed, leading zeros in the fraction are not.
  assert.equal(avaxOf(WEI / 10n), '0.1');
  assert.equal(avaxOf(WEI / 100n), '0.01');
});

test('a wei fee renders as nAVAX, the unit §184 quotes fees in', () => {
  // 25 gwei of max fee × 21000 gas = 525000 nAVAX = 0.000525 AVAX.
  assert.equal(navaxOf(25n * 10n ** 9n * 21_000n), '525000');
  assert.equal(navaxOf(10n ** 9n), '1');
});

test('an amount string becomes the exact wei it names', () => {
  assert.equal(parseAvaxAmount('1.5'), WEI + WEI / 2n);
  assert.equal(parseAvaxAmount('  2  '), 2n * WEI);
  assert.equal(parseAvaxAmount('0.000000000000000001'), 1n);
  assert.equal(parseAvaxAmount('.5'), WEI / 2n);
});

test('an amount this chain cannot carry is refused, not repaired', () => {
  for (const bad of [
    '',
    '   ',
    '-1',
    '+1',
    '1.2.3',
    'abc',
    '1e18',
    '0x10',
    // The 19th fraction digit would be silently rounded away by a float parser.
    '0.1234567890123456789',
  ]) {
    assert.equal(parseAvaxAmount(bad), null, `"${bad}" was accepted`);
  }
});

test('hexOf is lowercase and padded', () => {
  assert.equal(hexOf(new Uint8Array([0x00, 0x0f, 0xa5])), '000fa5');
  assert.equal(hexOf(new Uint8Array(32).fill(0x5a)), '5a'.repeat(32));
});

test('a chain id is named by its network, and an unnamed one says so', () => {
  assert.equal(networkName(AVALANCHE_MAINNET.chainId), 'Avalanche C-Chain');
  assert.equal(networkName(FUJI_TESTNET.chainId), 'Avalanche Fuji');
  // A record sealed under a chain this build cannot name must not borrow another network's name.
  assert.equal(networkName(1), 'chain 1');
});

test('a fee shows its ceiling until the receipt, the spent gas after', () => {
  assert.equal(
    feeLabel({ feeWei: 525_000n * 10n ** 9n }),
    'fee ≤ 525000 nAVAX',
    'a pending send understated its ceiling',
  );
  assert.equal(
    feeLabel({ feeWei: 525_000n * 10n ** 9n, block: 620_000_000, gasUsed: 21_000n }),
    'fee 21000 gas',
    'a confirmed send overstated its cost',
  );
});

test('an activity row shows the outcome word in its own tone', () => {
  const base: TrackedTx = {
    txHash: new Uint8Array(32).fill(0x5a),
    chainId: AVALANCHE_MAINNET.chainId,
    to: new Uint8Array(20).fill(0x11),
    valueWei: WEI + WEI / 2n,
    feeWei: 525_000n * 10n ** 9n,
    gasLimit: 21_000,
    atUnix: 1_800_000_000,
    outcome: 'PENDING',
  };

  const pending = renderToStaticMarkup(<ChainTxLine row={base} />);
  assert.match(pending, /−1\.5 AVAX/);
  assert.match(pending, /Avalanche C-Chain/);
  assert.match(pending, /pending/);
  assert.match(pending, /fee ≤ 525000 nAVAX/);
  // The hash is shown as its head, and the full hash is never pasted onto the page.
  assert.match(pending, new RegExp(`0x${'5a'.repeat(6)}`));

  const confirmed = renderToStaticMarkup(
    <ChainTxLine row={{ ...base, outcome: 'CONFIRMED', block: 620_000_000, gasUsed: 21_000n }} />,
  );
  assert.match(confirmed, /confirmed/);
  assert.match(confirmed, /fee 21000 gas/);
  assert.match(confirmed, /avax-outcome-confirmed/);

  const reverted = renderToStaticMarkup(
    <ChainTxLine row={{ ...base, outcome: 'REVERTED', block: 620_000_001, gasUsed: 30_000n }} />,
  );
  assert.match(reverted, /avax-outcome-failed/);

  const expired = renderToStaticMarkup(<ChainTxLine row={{ ...base, outcome: 'EXPIRED' }} />);
  assert.match(expired, /avax-outcome-expired/);
});
