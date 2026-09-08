'use client';

/**
 * The Security Checkup (§50): six standing checks on the account, each earning its ✓ from real
 * state and each warning naming the thing it is about.
 *
 * The row set — Identity, Devices, Wallets, Backup, Recovery, E2EE — is fixed cross-client; what
 * this panel owns is where each row's truth comes from:
 *
 *   - **Identity** this device answers synchronously from its own key store.
 *   - **Devices** and **Wallets** are the server's registries, re-read on every visit.
 *   - **Backup** is this browser's own record of when a `.migo` file last left it
 *     (lib/storage/backup-state-store.ts) — no server knows a download happened.
 *   - **Recovery** is one boolean the server answers; the address itself is write-only by design.
 *   - **E2EE** aggregates the peer-identity store the per-conversation safety surface keeps
 *     (§164): a conversation counts against this row only where a *previously acknowledged*
 *     fingerprint has changed, because that is the one state a person has seen and not resolved.
 *
 * The derivations live in lib/migo/checkup.ts as pure functions, and the rows here are a
 * controlled view over the models they return, so the rules (the fixed order, the honest
 * unknowns, the warning wording) are testable without a live client — the same posture every
 * other panel keeps. A failed source leaves its row pending and puts the failure in the panel's
 * own error line: a check that could not be read is never rendered as a check that passed.
 *
 * The read-only rule the E2EE aggregation keeps: this panel records *nothing* into the
 * peer-identity store. The read that detects a change is the one party that must never clear it
 * (see lib/migo/safety.ts), and a checkup that wrote baselines as a side effect of counting
 * would be exactly that party in a second place.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { DeviceSummary, Id, WalletSummary } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { conversationNeedsVerification, checkupRows } from '@/lib/migo/checkup.js';
import type { CheckupAction, CheckupRowModel } from '@/lib/migo/checkup.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import type { BackupState } from '@/lib/storage/backup-state-store.js';
import { loadBackupState } from '@/lib/storage/backup-state-store.js';
import { loadPeerIdentity } from '@/lib/storage/peer-identity-store.js';

import { Spinner } from './spinner.js';

/** The label each row's action door carries. */
const ACTION_LABEL: Readonly<Record<CheckupAction, string>> = {
  account: 'My Account',
  settings: 'Settings',
  'review-conversation': 'Review',
};

/**
 * One checkup row: the mark its status earns, the line it states, and the door out of a warning.
 *
 * The mark is `aria-hidden` because the line already carries the whole sentence — a screen reader
 * that heard "✓" and "Recovery contact not set" would be read a contradiction.
 */
export function CheckupRowView({
  row,
  onAction,
}: {
  /** The derived row: title, status, line, and optional action. */
  row: CheckupRowModel;
  /** Runs the row's action door, when it has one. */
  onAction: (action: CheckupAction) => void;
}): ReactNode {
  // Read once into a const so the narrowing survives into the button's onClick closure — TS does
  // not keep property narrowing through a callback, and `onAction(undefined)` is not a call this
  // component should ever make.
  const action = row.action;
  return (
    <div className={`checkup-row checkup-${row.status}`}>
      <span className="checkup-mark" aria-hidden="true">
        {row.status === 'pending' ? (
          <Spinner />
        ) : row.status === 'ok' ? (
          '✓'
        ) : row.status === 'warning' ? (
          '!'
        ) : (
          '—'
        )}
      </span>
      <span className="checkup-main">
        <span className="checkup-title">{row.title}</span>
        <span className="checkup-line">{row.line}</span>
      </span>
      {action !== undefined ? (
        <button
          type="button"
          className="btn btn-ghost"
          onClick={() => onAction(action)}
          aria-label={`${ACTION_LABEL[action]} — ${row.title}`}
        >
          {ACTION_LABEL[action]}
        </button>
      ) : null}
    </div>
  );
}

/** The six rows in their fixed order, as the panel draws them. */
export function CheckupRowsView({
  rows,
  onAction,
}: {
  /** The rows {@link checkupRows} derived, in the fixed cross-client order. */
  rows: readonly CheckupRowModel[];
  onAction: (action: CheckupAction) => void;
}): ReactNode {
  return (
    <div className="checkup-list" aria-label="Security checkup rows">
      {rows.map((row) => (
        <CheckupRowView key={row.key} row={row} onAction={onAction} />
      ))}
    </div>
  );
}

/**
 * The Security Checkup panel.
 *
 * Loads each row's source independently — a row's truth arriving late is the row staying pending,
 * never the panel waiting for its slowest fetch before showing anything. The E2EE count re-derives
 * whenever the conversation list changes (the SDK's enumeration is cached per peer, so a re-run
 * over an unchanged list costs no prekeys).
 */
export function SecurityCheckupPanel({
  onOpenAccount,
  onOpenSettings,
  onOpenConversation,
}: {
  /** Opens the "My Account" panel — the door for the backup, recovery, and rotation rows. */
  onOpenAccount: () => void;
  /** Opens Settings — the door for the old-device warning's removable rows. */
  onOpenSettings: () => void;
  /** Opens a conversation — the door for the E2EE warning's own safety surface. */
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const { items } = useConversations();

  const [devices, setDevices] = useState<DeviceSummary[] | null>(null);
  const [wallets, setWallets] = useState<WalletSummary[] | null>(null);
  const [backup, setBackup] = useState<BackupState | undefined | null>(null);
  const [recovery, setRecovery] = useState<'configured' | 'missing' | 'unavailable' | null>(null);
  const [unverified, setUnverified] = useState<number | null>(null);
  /** The first conversation the E2EE warning's Review door opens. */
  const [reviewId, setReviewId] = useState<Id | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Bumped by the retry door, so a failed source can be re-asked without a remount.
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (client === null || accountId === null) {
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      // Three independent sources, gathered rather than raced to one verdict: a failed wallet
      // read must not keep the devices row from telling the truth it already knows.
      void client
        .devices()
        .then((rows) => {
          if (!cancelled) {
            setDevices(rows);
          }
        })
        .catch((cause: unknown) => {
          if (!cancelled) {
            setError(friendlyError(cause));
          }
        });
      void client
        .wallets()
        .then((rows) => {
          if (!cancelled) {
            setWallets(rows);
          }
        })
        .catch((cause: unknown) => {
          if (!cancelled) {
            setError(friendlyError(cause));
          }
        });
      // The recovery read is a boolean from a route a server this old may not carry yet: a
      // refusal is the row's honest "could not read", not a panel-level failure.
      void client
        .recoveryContact()
        .then(({ configured }) => {
          if (!cancelled) {
            setRecovery(configured ? 'configured' : 'missing');
          }
        })
        .catch(() => {
          if (!cancelled) {
            setRecovery('unavailable');
          }
        });
      try {
        const state = await loadBackupState(accountId);
        if (!cancelled) {
          setBackup(state);
        }
      } catch (cause) {
        if (!cancelled) {
          setError(friendlyError(cause));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, accountId, attempt]);

  // The E2EE count: one pass over the known direct conversations, each peer's published devices
  // against the fingerprints this browser last acknowledged for that conversation.
  useEffect(() => {
    if (client === null || accountId === null) {
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      let count = 0;
      let first: Id | null = null;
      try {
        for (const summary of items) {
          if (summary.kind !== ConversationKind.Direct || summary.members === undefined) {
            continue;
          }
          const peer = summary.members.find((member) => member !== accountId);
          if (peer === undefined) {
            continue;
          }
          const identities = await client.peerIdentities(peer);
          const acknowledged = new Map<Id, Uint8Array>();
          for (const peerDevice of identities) {
            const stored = await loadPeerIdentity(summary.conversationId, peerDevice.deviceId);
            if (stored !== undefined) {
              acknowledged.set(peerDevice.deviceId, stored);
            }
          }
          // Read-only by contract (see the module doc): baselines are written where a person
          // looks at one conversation's numbers, never by the read that is counting them.
          const changed = conversationNeedsVerification(
            identities.map((peerDevice) => ({
              deviceId: peerDevice.deviceId,
              fingerprint: peerDevice.identity.fingerprint(),
            })),
            acknowledged,
          );
          if (changed) {
            count += 1;
            if (first === null) {
              first = summary.conversationId;
            }
          }
        }
        if (!cancelled) {
          setUnverified(count);
          setReviewId(first);
        }
      } catch (cause) {
        // A conversation whose identities could not be enumerated leaves the row pending rather
        // than counting as verified: "could not read" and "nothing changed" are different facts.
        if (!cancelled) {
          setUnverified(null);
          setError(friendlyError(cause));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, accountId, items]);

  const rows = useMemo(
    () =>
      checkupRows({
        nowMs: Date.now(),
        identityKeyHeld: client !== null && client.keyStore.accountIdentityKey() !== null,
        devices,
        wallets,
        backup,
        recovery,
        unverifiedConversations: unverified,
      }),
    [client, devices, wallets, backup, recovery, unverified],
  );

  const onAction = useCallback(
    (action: CheckupAction): void => {
      if (action === 'account') {
        onOpenAccount();
      } else if (action === 'settings') {
        onOpenSettings();
      } else if (reviewId !== null) {
        onOpenConversation(reviewId);
      }
    },
    [onOpenAccount, onOpenSettings, onOpenConversation, reviewId],
  );

  const retry = useCallback((): void => {
    setDevices(null);
    setWallets(null);
    setBackup(null);
    setRecovery(null);
    setUnverified(null);
    setError(null);
    setAttempt((value) => value + 1);
  }, []);

  return (
    <div className="panel">
      <h1 className="panel-title">Security Checkup</h1>
      <p className="hint">
        Six standing checks on this account. Every ✓ is read from real state; a check that cannot be
        read here says so.
      </p>
      {error !== null ? (
        <>
          <p className="form-error" role="alert">
            {error}
          </p>
          <button type="button" className="btn btn-ghost" onClick={retry}>
            Try again
          </button>
        </>
      ) : null}
      <section className="panel-section" aria-label="Security checkup">
        <CheckupRowsView rows={rows} onAction={onAction} />
      </section>
    </div>
  );
}
