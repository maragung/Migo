'use client';

/**
 * The Security Checkup's derivations (§50): the pure half of the six fixed rows — Identity,
 * Devices, Wallets, Backup, Recovery, E2EE — every client draws.
 *
 * The row set is a cross-client contract, but each row's *state* must be earned from real data,
 * never faked: a ✓ is a fact the caller can point at (a key this device holds, a registry the
 * server returned, a timestamp this browser wrote), a warning names the thing it is about, and a
 * check that cannot be derived is stated as unknown rather than guessed. That is the whole
 * discipline of this module: `checkupRows` turns plain inputs into the six rows with exactly
 * three statuses — `ok`, `warning`, and the honest `unknown`/`pending` for a check whose source
 * has not answered yet.
 *
 * Every input is either a value the panel loaded or `null` while it is loading, so the panel can
 * render the rows the moment it mounts and fill them in as each source lands — a row that waits
 * for the slowest fetch before showing anything reads as a broken panel.
 */

import type { DeviceSummary, Id, WalletSummary } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import type { BackupState } from '@/lib/storage/backup-state-store.js';

import { bytesEqual } from './safety.js';
import type { SafetyObservation } from './safety.js';

/**
 * A device this old and still ACTIVE is the §50 "Old device active" warning: a credential that
 * has not been used in a month is one nobody is minding, and the person should either recognise
 * it or remove it.
 */
export const STALE_DEVICE_DAYS = 30;

/** One day in milliseconds, the unit the device-age rule counts in. */
const DAY_MS = 86_400_000;

/**
 * The highest derivation index the replace flow will derive. BIP-44 permits far more, but a
 * registry is a person-driven list: an index this far past the row count means the server's list
 * and this device's root have drifted apart, and silently deriving wallet #100 to keep the flow
 * moving would hide that. The cap turns it into a refusal the view can state.
 */
export const MAX_DERIVATION_INDEX = 100;

/**
 * The devices whose status is `active` while their last sighting is older than
 * {@link STALE_DEVICE_DAYS} — the rows behind the "Old device active" warning, in list order.
 *
 * A device whose last sighting is unknown (`lastSeenAtMs` of 0) is not treated as stale: the
 * server writes a real timestamp on every sighting it knows, and "never seen" is a fact the
 * devices row already states plainly without this rule inventing an age for it.
 */
export function oldActiveDevices(
  devices: readonly DeviceSummary[],
  nowMs: number,
): DeviceSummary[] {
  return devices.filter(
    (device) =>
      device.status === 'active' &&
      device.lastSeenAtMs > 0 &&
      nowMs - device.lastSeenAtMs > STALE_DEVICE_DAYS * DAY_MS,
  );
}

/**
 * Whether one conversation has an unacknowledged peer key change: any peer device whose
 * *current* fingerprint differs from the one this conversation last acknowledged.
 *
 * The same semantics the per-conversation read keeps (§164): a device never observed is the
 * baseline, not a change — the checkup counts only changes a person has seen and not resolved,
 * which is the set that owes them an out-of-band comparison. The checkup's read is deliberately
 * read-only: it never records anything, so it can neither raise nor clear a warning as a side
 * effect of aggregating one.
 */
export function conversationNeedsVerification(
  observations: readonly SafetyObservation[],
  acknowledged: ReadonlyMap<Id, Uint8Array>,
): boolean {
  return observations.some(
    (observation) =>
      acknowledged.has(observation.deviceId) &&
      !bytesEqual(acknowledged.get(observation.deviceId)!, observation.fingerprint),
  );
}

/**
 * The index the next wallet should be derived at: one past the highest index the registry holds,
 * over *all* rows — archived included, because an index is a key: reusing a retired index would
 * re-register the old address, not mint a new wallet.
 *
 * `null` when the next index passes {@link MAX_DERIVATION_INDEX} (the drift refusal above). An
 * empty registry answers `0`, which is wallet 0 — the AVAX panel's own wallet.
 */
export function nextDerivationIndex(wallets: readonly WalletSummary[]): number | null {
  let highest = -1;
  for (const wallet of wallets) {
    if (Number.isFinite(wallet.derivationIndex) && wallet.derivationIndex > highest) {
      highest = wallet.derivationIndex;
    }
  }
  const next = highest + 1;
  return next > MAX_DERIVATION_INDEX ? null : next;
}

/** How a checkup row stands — the three honest states plus "the source has not answered yet". */
export type CheckupStatus = 'ok' | 'warning' | 'unknown' | 'pending';

/** What a row offers as the way out of a warning, if the warning has one. */
export type CheckupAction = 'account' | 'settings' | 'review-conversation';

/** One row of the fixed six, fully derived: what it says, how it stands, and its door if it has one. */
export interface CheckupRowModel {
  /** The fixed row key — the cross-client contract this row set is. */
  key: 'identity' | 'devices' | 'wallets' | 'backup' | 'recovery' | 'e2ee';
  /** The row's name, spelled the way the spec's checkup spells it. */
  title: string;
  status: CheckupStatus;
  /** The one line the row states: a fact for `ok`, the named warning for `warning`. */
  line: string;
  /** The door out of a warning, when the warning has one. */
  action?: CheckupAction;
}

/** What the rows are derived from — every source `null` while it is still loading. */
export interface CheckupInput {
  nowMs: number;
  /** Whether this device holds the account identity key (root or rotated successor). */
  identityKeyHeld: boolean | null;
  devices: readonly DeviceSummary[] | null;
  wallets: readonly WalletSummary[] | null;
  /** This browser's backup record, `undefined` when it has never exported one, `null` while loading. */
  backup: BackupState | undefined | null;
  /**
   * The recovery-contact answer: `'configured'`/`'missing'` once the server has answered,
   * `'unavailable'` when it would not (a server that does not expose the read yet), `null` while
   * the call is in flight.
   */
  recovery: 'configured' | 'missing' | 'unavailable' | null;
  /** How many known direct conversations have an unacknowledged key change; `null` while loading. */
  unverifiedConversations: number | null;
}

/** The identity row's earned ✓: the account's ML-DSA-65 key is in this device's store. */
export const IDENTITY_OK =
  'ML-DSA-65 identity key held on this device — rotation is available in My Account.';

/** The identity row's honest no-root state: not a warning, and never a faked ✓. */
export const IDENTITY_NO_ROOT =
  'This device does not hold the account root, so it cannot speak for the identity key.';

/** The recovery row's warning, §50's own words for it. */
export const RECOVERY_NOT_SET = 'Recovery contact not set.';

/** The recovery row's honest state when the server would not answer the question. */
export const RECOVERY_UNREADABLE =
  'Could not read whether a recovery contact is set on this server.';

/** What the backup row says when no `.migo` file has ever left this device. */
export const BACKUP_NEVER = 'Never backed up on this device.';

/** The backup row's warning once a rotation retired the key the sealed file carries. */
export const BACKUP_OUTDATED =
  'Backup outdated — the identity key was rotated after this file was sealed.';

/** The E2EE row's warning when conversations owe an identity verification. */
export const E2EE_NEEDS_VERIFICATION = 'identity verification';

/**
 * The six rows, in the fixed order, from whatever has landed so far.
 *
 * `null` inputs render as `pending` ("Checking…") rows in place, so the shape of the checkup is
 * visible before any source answers; a source that answered with nothing to show (an empty
 * wallet registry, an absent backup record) is a real state and gets its real line.
 */
export function checkupRows(input: CheckupInput): CheckupRowModel[] {
  const rows: CheckupRowModel[] = [];

  // Identity: the one row this device answers from its own store, synchronously, so it is never
  // pending — the key is either in the store or it is not, and both are statements.
  rows.push({
    key: 'identity',
    title: 'Identity',
    status: input.identityKeyHeld === true ? 'ok' : 'unknown',
    line: input.identityKeyHeld === true ? IDENTITY_OK : IDENTITY_NO_ROOT,
  });

  if (input.devices === null) {
    rows.push(pendingRow('devices', 'Devices'));
  } else {
    const stale = oldActiveDevices(input.devices, input.nowMs);
    rows.push({
      key: 'devices',
      title: 'Devices',
      status: stale.length > 0 ? 'warning' : 'ok',
      line:
        stale.length > 0
          ? `${stale.length === 1 ? 'Old device active' : 'Old devices active'}: ${stale
              .map(
                (device) =>
                  `“${device.displayName}” (last seen ${formatRelative(device.lastSeenAtMs, input.nowMs)})`,
              )
              .join(', ')}.`
          : `${input.devices.length} ${
              input.devices.length === 1 ? 'device' : 'devices'
            } registered; every active device was seen within the last ${STALE_DEVICE_DAYS} days.`,
      // The devices the warning names are removable in Settings, so the warning carries the door.
      action: stale.length > 0 ? 'settings' : undefined,
    });
  }

  if (input.wallets === null) {
    rows.push(pendingRow('wallets', 'Wallets'));
  } else {
    const active = input.wallets.filter((wallet) => wallet.status === 'active').length;
    rows.push({
      key: 'wallets',
      title: 'Wallets',
      // An empty registry is a warning rather than a ✓: the account's EVM side is addressless
      // until a wallet is registered, and "no wallet" is a state a person should notice.
      status: active > 0 ? 'ok' : 'warning',
      line:
        active > 0
          ? `${active} ${active === 1 ? 'wallet' : 'wallets'} active, ${
              input.wallets.length - active
            } archived.`
          : 'No wallet registered on this account yet.',
    });
  }

  if (input.backup === null) {
    rows.push(pendingRow('backup', 'Backup'));
  } else if (input.backup === undefined) {
    rows.push({
      key: 'backup',
      title: 'Backup',
      status: 'warning',
      line: BACKUP_NEVER,
      action: 'account',
    });
  } else {
    const outdated = input.backup.invalidatedAtMs !== undefined;
    rows.push({
      key: 'backup',
      title: 'Backup',
      status: outdated ? 'warning' : 'ok',
      line: outdated
        ? BACKUP_OUTDATED
        : `Backed up ${backupExportLabel(input.backup.lastExportAtMs)} on this device.`,
      action: outdated ? 'account' : undefined,
    });
  }

  if (input.recovery === null) {
    rows.push(pendingRow('recovery', 'Recovery'));
  } else if (input.recovery === 'configured') {
    rows.push({
      key: 'recovery',
      title: 'Recovery',
      status: 'ok',
      line: 'Recovery contact configured.',
    });
  } else if (input.recovery === 'unavailable') {
    rows.push({
      key: 'recovery',
      title: 'Recovery',
      status: 'unknown',
      line: RECOVERY_UNREADABLE,
    });
  } else {
    rows.push({
      key: 'recovery',
      title: 'Recovery',
      status: 'warning',
      line: RECOVERY_NOT_SET,
      action: 'account',
    });
  }

  if (input.unverifiedConversations === null) {
    rows.push(pendingRow('e2ee', 'E2EE'));
  } else {
    const count = input.unverifiedConversations;
    rows.push({
      key: 'e2ee',
      title: 'E2EE',
      status: count > 0 ? 'warning' : 'ok',
      line:
        count > 0
          ? `${count} ${
              count === 1 ? 'conversation needs' : 'conversations need'
            } ${E2EE_NEEDS_VERIFICATION}.`
          : 'No unacknowledged key changes in your direct conversations.',
      // The way through the warning is the conversation's own safety numbers, so the door opens
      // the first conversation that owes one.
      action: count > 0 ? 'review-conversation' : undefined,
    });
  }

  return rows;
}

/** A row whose source has not answered yet: the checkup's shape, with no claim in it. */
function pendingRow(key: CheckupRowModel['key'], title: string): CheckupRowModel {
  return { key, title, status: 'pending', line: 'Checking…' };
}

/**
 * The date a backup export is stated as, e.g. `3 Sep 2026`.
 *
 * A date, not a relative label: "Backed up 2d ago" quietly becomes "Backed up 5d ago" without
 * anything changing, while a dated line stays the fact it was written as.
 */
export function backupExportLabel(atMs: number): string {
  return new Date(atMs).toLocaleDateString(undefined, {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
  });
}
