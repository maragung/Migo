/**
 * The Security Checkup's derivations, its store, and its views (§50).
 *
 * # What is pinned, and why each part exists
 *
 *   1. **The row set is the contract.** Identity, Devices, Wallets, Backup, Recovery, E2EE — six
 *      rows in that order, on every client, whatever their states. The order and the titles are
 *      pinned from both a fully-answered input and a nothing-answered one, so a row cannot be
 *      dropped for loading reasons, and a new row cannot be added quietly.
 *   2. **Every ✓ is earned, every warning names its subject.** The device rule (active and unseen
 *      for over 30 days), the wallet counts, the backup record's three states, the recovery
 *      boolean, and the E2EE aggregation are each derived here from plain data, so the wording a
 *      person actually reads is the wording under test — an unnamed warning is a warning nobody
 *      acts on.
 *   3. **The honest unknowns.** A source that has not answered renders `Checking…`, not a guess;
 *      a device with no root behind the identity row renders "cannot speak for", not a ✓. The
 *      honesty rule is the feature, and it is pinned like one.
 *   4. **The backup store's transitions.** The last-export write, the rotation invalidation, and
 *      the fresh-export clear — the three writes this browser's record ever takes — run against
 *      the fake IndexedDB, because a wrong transition here is a checkup that vouches for a file
 *      the server will refuse.
 *   5. **The views.** The row's mark, its line, and its action door — over the same models the
 *      pure half returns, with no client.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { DeviceSummary, Id, WalletSummary } from '@migo/sdk';

import {
  CheckupRowView,
  CheckupRowsView,
  SecurityCheckupPanel,
} from '../src/components/security-checkup-panel.js';
import {
  BACKUP_NEVER,
  BACKUP_OUTDATED,
  IDENTITY_NO_ROOT,
  IDENTITY_OK,
  MAX_DERIVATION_INDEX,
  RECOVERY_NOT_SET,
  RECOVERY_UNREADABLE,
  STALE_DEVICE_DAYS,
  checkupRows,
  conversationNeedsVerification,
  nextDerivationIndex,
  oldActiveDevices,
} from '../src/lib/migo/checkup.js';
import type { CheckupInput, CheckupRowModel } from '../src/lib/migo/checkup.js';
import type { SafetyObservation } from '../src/lib/migo/safety.js';
import { ConversationsContext } from '../src/lib/migo/conversations-provider.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import {
  loadBackupState,
  markBackupOutdated,
  recordBackupExport,
} from '../src/lib/storage/backup-state-store.js';
import { installFakeIndexedDb } from './support/dom-stubs.js';

/** One day in milliseconds, the unit the device-age rule counts in. */
const DAY_MS = 86_400_000;

const NOW = Date.parse('2026-09-08T12:00:00Z');
const ACCOUNT = 'acct_checkup' as Id;

/** A device row, with the age of its last sighting the interesting field. */
function device(fields: {
  id: string;
  name: string;
  status?: string;
  seenAgoMs?: number;
  current?: boolean;
}): DeviceSummary {
  return {
    deviceId: fields.id as Id,
    displayName: fields.name,
    platform: 'web',
    status: fields.status ?? 'active',
    createdAtMs: NOW - 90 * DAY_MS,
    lastSeenAtMs: fields.seenAgoMs === undefined ? NOW : NOW - fields.seenAgoMs,
    hasCredential: true,
    isCurrent: fields.current ?? false,
  };
}

/** A wallet row, as the server's registry returns it. */
function wallet(fields: {
  id: string;
  index: number;
  status?: string;
  label?: string;
}): WalletSummary {
  return {
    walletId: fields.id as Id,
    address: '5aaeb6053f3e94c9b9a09f33669435e7ef1beaed',
    chainType: 'evm',
    derivationIndex: fields.index,
    status: fields.status ?? 'active',
    createdAtMs: NOW - 30 * DAY_MS,
    ...(fields.label !== undefined ? { label: fields.label } : {}),
  };
}

/** A fully-answered checkup input, overridable field by field. */
function input(overrides: Partial<CheckupInput> = {}): CheckupInput {
  return {
    nowMs: NOW,
    identityKeyHeld: true,
    devices: [device({ id: 'd1', name: 'This browser', current: true })],
    wallets: [wallet({ id: 'w1', index: 0, label: 'Primary' })],
    backup: {
      accountId: ACCOUNT,
      lastExportAtMs: NOW - 2 * DAY_MS,
      savedAt: NOW - 2 * DAY_MS,
    },
    recovery: 'configured',
    unverifiedConversations: 0,
    ...overrides,
  };
}

/** The row keys in the fixed cross-client order. */
const ROW_ORDER = ['identity', 'devices', 'wallets', 'backup', 'recovery', 'e2ee'] as const;

// --- the fixed row set --------------------------------------------------------------------------

test('the row set is the fixed six, in order, whether everything answered or nothing has', () => {
  for (const source of [
    input(),
    input({
      identityKeyHeld: null,
      devices: null,
      wallets: null,
      backup: null,
      recovery: null,
      unverifiedConversations: null,
    }),
  ]) {
    assert.deepEqual(
      checkupRows(source).map((row) => row.key),
      ROW_ORDER,
    );
    assert.deepEqual(
      checkupRows(source).map((row) => row.title),
      ['Identity', 'Devices', 'Wallets', 'Backup', 'Recovery', 'E2EE'],
    );
  }
});

// --- the device rule ---------------------------------------------------------------------------

test('a device active but unseen for over 30 days is the old-device warning, and names the device', () => {
  const rows = oldActiveDevices(
    [device({ id: 'd1', name: 'Pixel 7', seenAgoMs: 31 * DAY_MS })],
    NOW,
  );
  assert.equal(rows.length, 1);
  assert.equal(rows[0]!.displayName, 'Pixel 7');

  const row = checkupRows(input({ devices: rows })).find((r) => r.key === 'devices')!;
  assert.ok(row.line.includes('Old device active'), 'the warning uses its §50 name');
  assert.ok(row.line.includes('“Pixel 7”'), 'the warning names the device it is about');
  assert.ok(row.line.includes('last seen'), 'the warning states the evidence it rests on');
  assert.equal(row.action, 'settings', 'the warning carries the door to the device list');
});

test('the 30-day boundary is honest in both directions, and a revoked or never-seen device is not judged stale', () => {
  const exactlyThirty = device({ id: 'd1', name: 'Laptop', seenAgoMs: STALE_DEVICE_DAYS * DAY_MS });
  const thirtyAndASecond = device({
    id: 'd1',
    name: 'Laptop',
    seenAgoMs: STALE_DEVICE_DAYS * DAY_MS + 1,
  });
  assert.deepEqual(oldActiveDevices([exactlyThirty], NOW), [], '30 days sharp is not yet stale');
  assert.equal(oldActiveDevices([thirtyAndASecond], NOW).length, 1, 'a day past it is');

  assert.deepEqual(
    oldActiveDevices(
      [device({ id: 'd2', name: 'Old phone', seenAgoMs: 400 * DAY_MS, status: 'revoked' })],
      NOW,
    ),
    [],
    'a revoked device is not active, however long ago it was seen',
  );
  const neverSeen = device({ id: 'd3', name: 'Mystery' });
  const rows = [{ ...neverSeen, lastSeenAtMs: 0 }];
  assert.deepEqual(
    oldActiveDevices(rows as DeviceSummary[], NOW),
    [],
    'a device the server has never seen is a fact the list already states, not an invented age',
  );
});

test('a quiet device list earns its check with the count it rests on', () => {
  const devices = [
    device({ id: 'd1', name: 'This browser', current: true }),
    device({ id: 'd2', name: 'Laptop', seenAgoMs: 3 * DAY_MS }),
  ];
  const row = checkupRows(input({ devices })).find((candidate) => candidate.key === 'devices')!;
  assert.equal(row.status, 'ok');
  assert.ok(row.line.includes('2 devices registered'), 'the count is the fact behind the ✓');
  assert.ok(row.line.includes(`within the last ${STALE_DEVICE_DAYS} days`));
  assert.equal(row.action, undefined, 'a quiet row owes no door');
});

// --- the wallet registry counts ------------------------------------------------------------------

test('the wallet row counts active and archived, and an empty registry is a warning, not a ✓', () => {
  const mixed = [
    wallet({ id: 'w1', index: 0, status: 'active', label: 'Primary' }),
    wallet({ id: 'w2', index: 1, status: 'active' }),
    wallet({ id: 'w3', index: 2, status: 'archived' }),
  ];
  const counted = checkupRows(input({ wallets: mixed })).find((r) => r.key === 'wallets')!;
  assert.equal(counted.status, 'ok');
  assert.ok(counted.line.includes('2 wallets active'), 'active first');
  assert.ok(counted.line.includes('1 archived'), 'archived beside it');

  const empty = checkupRows(input({ wallets: [] })).find((r) => r.key === 'wallets')!;
  assert.equal(empty.status, 'warning');
  assert.ok(empty.line.includes('No wallet registered'), 'addressless is a state to notice');
});

// --- the next derivation index -------------------------------------------------------------------

test('the next derivation index is one past the highest index the registry holds, archived included', () => {
  assert.equal(nextDerivationIndex([]), 0, 'an empty registry starts at wallet 0');
  assert.equal(
    nextDerivationIndex([wallet({ id: 'w1', index: 0, status: 'archived' })]),
    1,
    'an archived index is still a taken key: reusing it would re-register the old address',
  );
  assert.equal(
    nextDerivationIndex([
      wallet({ id: 'w1', index: 0 }),
      wallet({ id: 'w2', index: 4 }),
      wallet({ id: 'w3', index: 2 }),
    ]),
    5,
    'the highest, not the last, is what the successor steps past',
  );
});

test('the derivation index refuses to step past the cap rather than silently deriving wallet 100', () => {
  assert.equal(nextDerivationIndex([wallet({ id: 'w1', index: MAX_DERIVATION_INDEX })]), null);
  assert.notEqual(
    nextDerivationIndex([wallet({ id: 'w1', index: MAX_DERIVATION_INDEX - 1 })]),
    null,
  );
});

// --- the backup row ------------------------------------------------------------------------------

test('a never-exported browser is told so, plainly, with the door to fix it', () => {
  const row = checkupRows(input({ backup: undefined })).find((r) => r.key === 'backup')!;
  assert.equal(row.status, 'warning');
  assert.ok(row.line.includes(BACKUP_NEVER));
  assert.equal(row.action, 'account');
});

test('a recorded export earns the check with its date; a rotation turns the same record into the outdated warning', () => {
  const ok = checkupRows(input()).find((r) => r.key === 'backup')!;
  assert.equal(ok.status, 'ok');
  assert.ok(ok.line.startsWith('Backed up '), 'the date leads');
  assert.ok(ok.line.endsWith('on this device.'), 'and the honesty about which device recorded it');

  const outdated = checkupRows(
    input({
      backup: {
        accountId: ACCOUNT,
        lastExportAtMs: NOW - 2 * DAY_MS,
        invalidatedAtMs: NOW - DAY_MS,
        savedAt: NOW - DAY_MS,
      },
    }),
  ).find((r) => r.key === 'backup')!;
  assert.equal(outdated.status, 'warning');
  assert.ok(outdated.line.includes(BACKUP_OUTDATED));
  assert.equal(outdated.action, 'account', 'the way out is the fresh file the Account panel seals');
});

// --- the recovery row -----------------------------------------------------------------------------

test('the recovery row says exactly what the server said, and nothing when it said nothing', () => {
  const configured = checkupRows(input({ recovery: 'configured' })).find(
    (r) => r.key === 'recovery',
  )!;
  assert.equal(configured.status, 'ok');
  assert.ok(configured.line.includes('Recovery contact configured.'));

  const missing = checkupRows(input({ recovery: 'missing' })).find((r) => r.key === 'recovery')!;
  assert.equal(missing.status, 'warning');
  assert.ok(missing.line.includes(RECOVERY_NOT_SET));
  assert.equal(missing.action, 'account');

  const unreadable = checkupRows(input({ recovery: 'unavailable' })).find(
    (r) => r.key === 'recovery',
  )!;
  assert.equal(
    unreadable.status,
    'unknown',
    'a server that would not answer is not a ✓ and not a warning',
  );
  assert.ok(unreadable.line.includes(RECOVERY_UNREADABLE));
});

// --- the E2EE row --------------------------------------------------------------------------------

test('the E2EE row counts only conversations a person has seen change in, and says so with grammar', () => {
  const quiet = checkupRows(input({ unverifiedConversations: 0 })).find((r) => r.key === 'e2ee')!;
  assert.equal(quiet.status, 'ok');
  assert.ok(quiet.line.includes('No unacknowledged key changes'));

  const one = checkupRows(input({ unverifiedConversations: 1 })).find((r) => r.key === 'e2ee')!;
  assert.equal(one.status, 'warning');
  assert.ok(one.line.includes('1 conversation needs identity verification.'), 'singular');

  const three = checkupRows(input({ unverifiedConversations: 3 })).find((r) => r.key === 'e2ee')!;
  assert.ok(three.line.includes('3 conversations need identity verification.'), 'plural');
  assert.equal(
    three.action,
    'review-conversation',
    'the door opens the conversation that owes a review',
  );
});

test('an unread E2EE source stays pending: "could not read" is not "nothing changed"', () => {
  const row = checkupRows(input({ unverifiedConversations: null })).find((r) => r.key === 'e2ee')!;
  assert.equal(row.status, 'pending');
  assert.ok(row.line.includes('Checking…'));
});

// --- the identity row -----------------------------------------------------------------------------

test('the identity row is earned from the key store: held earns the check, absent is an unknown, never a fake ✓', () => {
  const held = checkupRows(input({ identityKeyHeld: true })).find((r) => r.key === 'identity')!;
  assert.equal(held.status, 'ok');
  assert.ok(held.line.includes(IDENTITY_OK));
  assert.ok(held.line.includes('ML-DSA-65'), 'the algorithm is named');

  const absent = checkupRows(input({ identityKeyHeld: false })).find((r) => r.key === 'identity')!;
  assert.equal(absent.status, 'unknown');
  assert.ok(absent.line.includes(IDENTITY_NO_ROOT));
});

// --- the E2EE aggregation semantics ---------------------------------------------------------------

const FIRST_DEVICE = 'dev_1' as Id;
const SECOND_DEVICE = 'dev_2' as Id;

function unhex(text: string): Uint8Array {
  return new Uint8Array((text.match(/../g) ?? []).map((pair) => parseInt(pair, 16)));
}

const STABLE = unhex('202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f');
const ROTATED = unhex('808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f');

function observation(deviceId: Id, fingerprint: Uint8Array): SafetyObservation {
  return { deviceId, fingerprint };
}

test('a conversation needs verification only where an acknowledged fingerprint changed', () => {
  // A device never observed is the baseline, not a change — the same rule the per-conversation
  // safety read keeps, so the checkup cannot manufacture warnings from thin air.
  assert.equal(
    conversationNeedsVerification([observation(FIRST_DEVICE, STABLE)], new Map()),
    false,
  );
  assert.equal(
    conversationNeedsVerification(
      [observation(FIRST_DEVICE, STABLE)],
      new Map([[FIRST_DEVICE, STABLE]]),
    ),
    false,
    'an unchanged fingerprint is quiet',
  );
  assert.equal(
    conversationNeedsVerification(
      [observation(FIRST_DEVICE, ROTATED)],
      new Map([[FIRST_DEVICE, STABLE]]),
    ),
    true,
    'a changed fingerprint is the warning',
  );
  // A change on one device is enough: the conversation's verification is broken as a whole.
  assert.equal(
    conversationNeedsVerification(
      [observation(FIRST_DEVICE, STABLE), observation(SECOND_DEVICE, ROTATED)],
      new Map<Id, Uint8Array>([
        [FIRST_DEVICE, STABLE],
        [SECOND_DEVICE, STABLE],
      ]),
    ),
    true,
  );
});

// --- the backup store ------------------------------------------------------------------------------

test('a recorded export round-trips with its timestamp and no invalidation', async () => {
  const fake = installFakeIndexedDb();
  try {
    assert.equal(await loadBackupState(ACCOUNT), undefined, 'nothing before the first export');
    await recordBackupExport(ACCOUNT);
    const state = await loadBackupState(ACCOUNT);
    assert.ok(state !== undefined);
    assert.ok(state.lastExportAtMs > 0, 'the export time is recorded');
    assert.equal(state.invalidatedAtMs, undefined, 'a fresh export vouches for itself');
    assert.equal(
      await loadBackupState('acct_other' as Id),
      undefined,
      'the record is per account, like the device record beside it',
    );
  } finally {
    fake.restore();
  }
});

test('marking the backup outdated invents no export, is idempotent, and is cleared by the next export', async () => {
  const fake = installFakeIndexedDb();
  try {
    // No record: "never backed up" is already the truer warning, so no record is invented.
    await markBackupOutdated(ACCOUNT);
    assert.equal(await loadBackupState(ACCOUNT), undefined);

    await recordBackupExport(ACCOUNT);
    await markBackupOutdated(ACCOUNT);
    const outdated = await loadBackupState(ACCOUNT);
    assert.ok(outdated?.invalidatedAtMs !== undefined, 'the rotation stamped the record');
    assert.ok(
      outdated.lastExportAtMs > 0,
      'the export time survives: the fact, and when it stopped being good news',
    );

    // A second mark is a no-op: the stamp names the rotation, and only one happened.
    const before = outdated.invalidatedAtMs;
    await markBackupOutdated(ACCOUNT);
    assert.equal((await loadBackupState(ACCOUNT))?.invalidatedAtMs, before);

    // A fresh export supersedes the file the stamp was about.
    await recordBackupExport(ACCOUNT);
    assert.equal(
      (await loadBackupState(ACCOUNT))?.invalidatedAtMs,
      undefined,
      'the new file vouches for the current key, so the warning clears',
    );
  } finally {
    fake.restore();
  }
});

// --- the views --------------------------------------------------------------------------------------

function rowOf(key: CheckupRowModel['key'], rows: readonly CheckupRowModel[]): CheckupRowModel {
  const row = rows.find((candidate) => candidate.key === key);
  assert.ok(row !== undefined, `the "${key}" row exists`);
  return row;
}

test('a row draws the mark its status earned and the door its warning owes', () => {
  const rows = checkupRows(input({ recovery: 'missing' }));

  const ok = renderToStaticMarkup(
    <CheckupRowView row={rowOf('identity', rows)} onAction={() => {}} />,
  );
  assert.ok(ok.includes('✓'), 'an earned check is a check');
  assert.ok(!ok.includes('btn-ghost'), 'a quiet row offers no door');

  const warning = renderToStaticMarkup(
    <CheckupRowView row={rowOf('recovery', rows)} onAction={() => {}} />,
  );
  assert.ok(warning.includes('!'), 'a warning carries a warning mark');
  assert.ok(warning.includes('My Account'), 'and the door to the surface that resolves it');

  const stale = renderToStaticMarkup(
    <CheckupRowView
      row={rowOf(
        'devices',
        checkupRows(
          input({ devices: [device({ id: 'd1', name: 'Pixel 7', seenAgoMs: 40 * DAY_MS })] }),
        ),
      )}
      onAction={() => {}}
    />,
  );
  assert.ok(stale.includes('!'), 'a warning carries a warning mark');
  assert.ok(stale.includes('Settings'), 'and the door to the surface that resolves it');
});

test('a pending row is a status, not a silence, and an unknown row renders no mark of success', () => {
  const rows = checkupRows(input({ devices: null, identityKeyHeld: false }));
  const pending = renderToStaticMarkup(
    <CheckupRowView row={rowOf('devices', rows)} onAction={() => {}} />,
  );
  assert.match(pending, /role="status"/, 'the pending mark announces itself as a status');
  assert.ok(pending.includes('Checking…'));

  const unknown = renderToStaticMarkup(
    <CheckupRowView row={rowOf('identity', rows)} onAction={() => {}} />,
  );
  assert.ok(unknown.includes('—'), 'the unknown mark is a dash, not a check');
  assert.ok(!unknown.includes('✓'));
});

test('the rows view draws the six in the fixed order', () => {
  const markup = renderToStaticMarkup(
    <CheckupRowsView rows={checkupRows(input())} onAction={() => {}} />,
  );
  const positions = ['Identity', 'Devices', 'Wallets', 'Backup', 'Recovery', 'E2EE'].map((title) =>
    markup.indexOf(title),
  );
  assert.ok(
    positions.every((position) => position >= 0),
    'every row title renders',
  );
  assert.deepEqual(
    [...positions].sort((left, right) => left - right),
    positions,
    'the rows render in the cross-client order',
  );
});

test('the panel under a null client keeps its shape: the title, the six rows, and no faked check', () => {
  const markup = renderToStaticMarkup(
    <MigoContext.Provider
      value={{
        status: 'ready',
        connectionState: 'ready',
        accountId: ACCOUNT,
        deviceId: null,
        error: null,
        resetNonce: 0,
        persistKeyStore: () => {},
        client: null,
        register: () => Promise.resolve(),
        loginWithFile: () => Promise.resolve(),
        logout: () => Promise.resolve(),
      }}
    >
      <ConversationsContext.Provider
        value={{
          items: [],
          loading: false,
          error: null,
          hasMore: false,
          loadMore: () => {},
          reload: () => {},
          unread: new Set(),
          markRead: () => {},
          noteConversation: () => {},
          forgetConversation: () => {},
          lastPreviews: new Map(),
        }}
      >
        <SecurityCheckupPanel
          onOpenAccount={() => {}}
          onOpenSettings={() => {}}
          onOpenConversation={() => {}}
        />
      </ConversationsContext.Provider>
    </MigoContext.Provider>,
  );
  assert.ok(markup.includes('Security Checkup'));
  for (const title of ['Identity', 'Devices', 'Wallets', 'Backup', 'Recovery', 'E2EE']) {
    assert.ok(markup.includes(title), `the ${title} row is present before any source answers`);
  }
  assert.ok(markup.includes(IDENTITY_NO_ROOT), 'no client means the honest no-root line');
  assert.ok(
    !markup.includes('checkup-ok'),
    'and no row stands as passed before any source has answered (the hint text aside)',
  );
});
