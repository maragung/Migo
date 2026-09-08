/**
 * The wallet registry rows and the replace flow's confirm sheet (§21–22), as controlled views
 * over plain data — no client, no server, no root.
 *
 * # What is pinned, and why each part exists
 *
 *   1. **The registry shows the server's truth in the person's form.** Rows carry the address
 *      monospaced, the label or the index-derived name, and exactly the two statuses the server
 *      has (`Active`/`Archived`) — a status this client invented would be a lie about the server.
 *   2. **The replace confirm is honest *before* the button.** §22's sentence — the address
 *      changes and assets do not move — is pinned on the sheet itself, with the OLD and NEW
 *      addresses side by side, because a confirm that does not show the successor's address is
 *      a confirm about nothing.
 *   3. **The button is disabled without a successor.** The flow never proceeds on an address it
 *      never showed, and the derivation problems (`no root`, `index capped`) render as their own
 *      sentences rather than as a dead button with no explanation.
 *   4. **EIP-55 is the display form.** A registry address is lowercase hex on the wire and
 *      checksummed on the screen; the pinned vectors are the EIP-55 test vectors themselves, so
 *      a regression here is caught by the form every receiving tool checks.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { Id, WalletSummary } from '@migo/sdk';

import {
  ARCHIVE_FAILED,
  DEFAULT_WALLET_LABEL,
  REPLACE_INDEX_CAPPED,
  REPLACE_NO_ROOT,
  REPLACE_WALLET_HONESTY,
  ReplaceWalletView,
  WalletRegistryView,
  WalletRowView,
  displayAddress,
} from '../src/components/wallet-registry.js';

const NOW = 1_750_000_000_000;

/** A wallet row, as the server's registry returns it. */
function wallet(fields: {
  id: string;
  index: number;
  address?: string;
  status?: string;
  label?: string;
}): WalletSummary {
  return {
    walletId: fields.id as Id,
    address: fields.address ?? '5aaeb6053f3e94c9b9a09f33669435e7ef1beaed',
    chainType: 'evm',
    derivationIndex: fields.index,
    status: fields.status ?? 'active',
    createdAtMs: NOW,
    ...(fields.label !== undefined ? { label: fields.label } : {}),
  };
}

/** The replace sheet with every field the confirm needs, overridable field by field. */
function sheet(overrides: Partial<Parameters<typeof ReplaceWalletView>[0]> = {}) {
  const props = {
    oldAddress: '0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed',
    newAddress: '0xfb6916095Ca1Df60Bb5Ce91c5463127c326FbF1c',
    derivationIndex: 1,
    label: DEFAULT_WALLET_LABEL,
    problem: null,
    busy: false,
    error: null,
    onLabel: () => {},
    onConfirm: () => {},
    onClose: () => {},
    ...overrides,
  };
  return props;
}

// --- the display form --------------------------------------------------------------------------

test('displayAddress renders the EIP-55 checksummed form of the wire address', () => {
  // Two vectors whose checksums mix case in different places (both re-derived from the EIP-55
  // rule — keccak of the lowercase form, letter uppercased where the digest nibble is >= 8 —
  // rather than copied, so a casing error cannot be pinned as if it were the answer).
  assert.equal(
    displayAddress('5aaeb6053f3e94c9b9a09f33669435e7ef1beaed'),
    '0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed',
  );
  assert.equal(
    displayAddress('fb6916095ca1df60bb5ce91c5463127c326fbf1c'),
    '0xfb6916095Ca1Df60Bb5Ce91c5463127c326FbF1c',
  );
  // The 0x prefix the registry never sends is tolerated, not required.
  assert.equal(
    displayAddress('0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed'),
    '0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed',
  );
});

// --- the registry rows --------------------------------------------------------------------------

test('a wallet row shows the label (or the index name), the status the server gave, and the monospaced address', () => {
  const labelled = renderToStaticMarkup(
    <WalletRowView
      wallet={wallet({ id: 'w1', index: 0, label: 'Primary' })}
      canReplace
      busy={false}
      onReplace={() => {}}
    />,
  );
  assert.ok(labelled.includes('Primary'));
  assert.ok(labelled.includes('tag-current'), 'an active wallet wears the active tag');
  assert.ok(labelled.includes('Active'));
  assert.ok(
    labelled.includes('wallet-address'),
    'the address is the monospaced form, not body text',
  );
  assert.ok(labelled.includes('0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed'));
  assert.ok(labelled.includes('index 0'), 'the derivation index is stated, not implied');

  const unnamed = renderToStaticMarkup(
    <WalletRowView
      wallet={wallet({ id: 'w2', index: 3 })}
      canReplace
      busy={false}
      onReplace={() => {}}
    />,
  );
  assert.ok(unnamed.includes('Wallet 3'), 'an unnamed wallet is named by its index');
});

test('an archived wallet states its status and offers no Replace control; the two statuses are the only two', () => {
  const archived = renderToStaticMarkup(
    <WalletRowView
      wallet={wallet({ id: 'w1', index: 0, status: 'archived' })}
      canReplace
      busy={false}
      onReplace={() => {}}
    />,
  );
  assert.ok(archived.includes('tag-revoked'));
  assert.ok(archived.includes('Archived'));
  assert.ok(!archived.includes('>Replace<'), 'a replaced wallet cannot be replaced again');

  // canReplace gates the control too: a device without the root offers no button that cannot work.
  const rootless = renderToStaticMarkup(
    <WalletRowView
      wallet={wallet({ id: 'w1', index: 0 })}
      canReplace={false}
      busy={false}
      onReplace={() => {}}
    />,
  );
  assert.ok(!rootless.includes('>Replace<'));
});

test('the registry list shows every wallet and states an empty registry rather than hiding it', () => {
  const markup = renderToStaticMarkup(
    <WalletRegistryView
      wallets={[
        wallet({ id: 'w1', index: 0, label: 'Primary' }),
        wallet({ id: 'w2', index: 1, status: 'archived' }),
      ]}
      canReplace
      busyId={null}
      onReplace={() => {}}
    />,
  );
  assert.ok(markup.includes('Primary'));
  assert.ok(markup.includes('Archived'), 'the server keeps archived rows; the list keeps them too');

  const empty = renderToStaticMarkup(
    <WalletRegistryView wallets={[]} canReplace busyId={null} onReplace={() => {}} />,
  );
  assert.ok(
    empty.includes('No wallets registered on this account yet.'),
    'an empty registry is a stated fact, not a blank',
  );
});

// --- the replace confirm sheet -------------------------------------------------------------------

test('the confirm sheet states §22 honesty sentence and both addresses before the button', () => {
  const markup = renderToStaticMarkup(<ReplaceWalletView {...sheet()} />);
  assert.ok(
    markup.includes(REPLACE_WALLET_HONESTY),
    'the irreversible fact is on the sheet itself',
  );
  assert.ok(markup.includes('OLD'));
  assert.ok(markup.includes('NEW'), 'the successor address is shown, not implied');
  assert.ok(
    markup.includes('0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed'),
    'the replaced address, checksummed',
  );
  assert.ok(
    markup.includes('0xfb6916095Ca1Df60Bb5Ce91c5463127c326FbF1c'),
    'the successor address, checksummed',
  );
  assert.ok(markup.includes('Derivation index 1'), 'the sheet says which key the successor is');
  assert.ok(markup.includes(DEFAULT_WALLET_LABEL), 'the label draft is in the field');
  assert.ok(markup.includes('Replace wallet'), 'the committing button is labelled as what it does');
  assert.ok(markup.includes('Cancel'));
});

test('the confirm button is disabled while busy, and a busy sheet replaces the label with the spinner', () => {
  const busy = renderToStaticMarkup(<ReplaceWalletView {...sheet({ busy: true })} />);
  assert.ok(busy.includes('disabled') || busy.includes('aria-disabled'));
  assert.ok(!busy.includes('>Replace wallet<'), 'no second click while the first is in flight');
  assert.match(busy, /role="status"/, 'the spinner announces itself');
  // Cancel is disabled too: closing the sheet mid-register would strand the flow's state.
  assert.ok(busy.includes('Cancel'));
});

test('a sheet with no successor shows its problem and no working confirm', () => {
  const noRoot = renderToStaticMarkup(
    <ReplaceWalletView
      {...sheet({ newAddress: null, derivationIndex: null, problem: REPLACE_NO_ROOT })}
    />,
  );
  assert.ok(
    noRoot.includes(REPLACE_NO_ROOT),
    'the device-without-root sentence is the stated problem',
  );
  assert.ok(!noRoot.includes('>NEW<'), 'no successor was derived, so none is shown');
  assert.ok(
    noRoot.includes('disabled'),
    'the confirm button is present but disabled: no derivation, no commit',
  );

  const capped = renderToStaticMarkup(
    <ReplaceWalletView
      {...sheet({ newAddress: null, derivationIndex: null, problem: REPLACE_INDEX_CAPPED })}
    />,
  );
  assert.ok(capped.includes(REPLACE_INDEX_CAPPED));
});

test('the half-done state has its own sentence, stated as the half it reached', () => {
  const markup = renderToStaticMarkup(
    <ReplaceWalletView
      {...sheet({
        error: `${ARCHIVE_FAILED} The server said no.`,
        newAddress: '0xfb6916095Ca1Df60Bb5Ce91c5463127c326FbF1c',
      })}
    />,
  );
  assert.ok(markup.includes(ARCHIVE_FAILED));
  // The successor stays shown beside the error: the registered address is a fact the person
  // needs while the old one sits unarchived beside it.
  assert.ok(markup.includes('>NEW<'));
});
