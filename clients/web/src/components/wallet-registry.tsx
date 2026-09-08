'use client';

/**
 * The wallet registry (§21) and the replace flow (§22): the account-level truth of which
 * addresses are registered, and the one deliberate way an active address is succeeded.
 *
 * The registry is the *server's* list, not a derivation: this device's root can derive any
 * index, but only the registry says which addresses the account has actually registered — which
 * is why the list is fetched rather than computed, and why the AVAX panel's wallet-0 focus and
 * this section coexist without disagreeing (wallet 0 is what the root derives; the registry is
 * what the account claims).
 *
 * The replace flow is honest about its one irreversible fact before the button that commits it:
 * a replacement changes the account's Ethereum address, and the assets on the old address stay
 * where they are (§22's own sentence). The order of operations is register-then-archive, so the
 * account is never left without an active wallet; a registration that succeeds but cannot
 * archive is reported as exactly that half-finished state, never rolled back silently.
 *
 * The successor is derived client-side from the root this device holds — the next index past the
 * registry's highest (see {@link nextDerivationIndex}) — so the flow exists only where the root
 * is; a device without it says so and offers no control that could not work.
 *
 * The presentational halves are exported as controlled components over plain data, so the rules
 * (the address forms shown, the honesty sentence before the confirm, the label the successor
 * carries) are testable without a live client.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { account } from '@migo/sdk';
import type { Id, WalletSummary } from '@migo/sdk';

import { hexOf } from '@/lib/avax.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { nextDerivationIndex } from '@/lib/migo/checkup.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { BottomSheet } from './bottom-sheet.js';
import { Spinner } from './spinner.js';

/**
 * The label a successor wallet carries when the person does not type one: the replaced wallet's
 * own label when it had one (a "Primary" is succeeded by a "Primary"), and `Primary` otherwise.
 */
export const DEFAULT_WALLET_LABEL = 'Primary';

/** §22's own honesty sentence, stated before the confirm that commits the replacement. */
export const REPLACE_WALLET_HONESTY =
  'Replacing your wallet changes your Ethereum address. Assets on the old address are not automatically transferred.';

/** The register-then-archive half-done state, stated as the half it reached. */
export const ARCHIVE_FAILED =
  'The new wallet is registered, but the old one could not be archived.';

/** What a device without the root is told, wherever a replacement is asked for. */
export const REPLACE_NO_ROOT =
  'This device does not hold the account root, so it cannot derive a new wallet. Open the wallet on the device that holds the account backup.';

/** The refusal when the registry's indexes have drifted past the derivation cap. */
export const REPLACE_INDEX_CAPPED =
  'The wallet registry has run past the indexes this client will derive; no replacement can be offered here.';

/** A lowercase-hex registry address as the 20 bytes it names. */
function addressBytes(address: string): Uint8Array {
  const clean = address.replace(/^0x/, '');
  return new Uint8Array((clean.match(/../g) ?? []).map((pair) => parseInt(pair, 16)));
}

/**
 * A registry address as the form a person should read: EIP-55 checksummed, the only form a
 * mistyped copy of which every receiving tool rejects.
 */
export function displayAddress(address: string): string {
  return account.eip55(addressBytes(address));
}

/** One registered wallet as the registry row shows it. */
export function WalletRowView({
  wallet,
  canReplace,
  busy,
  onReplace,
}: {
  /** The wire row. */
  wallet: WalletSummary;
  /** Whether this device holds the root, so the Replace control is offered at all. */
  canReplace: boolean;
  /** True while this row's replacement is in flight. */
  busy: boolean;
  /** Requests this wallet's replacement (offered only for an active wallet). */
  onReplace: (wallet: WalletSummary) => void;
}): ReactNode {
  const archived = wallet.status === 'archived';
  return (
    <div className="person-row wallet-row">
      <div className="person-main">
        <span className="person-name">
          {wallet.label ?? `Wallet ${wallet.derivationIndex}`}
          <span className={`tag ${archived ? 'tag-revoked' : 'tag-current'}`}>
            {archived ? 'Archived' : 'Active'}
          </span>
        </span>
        <span className="person-sub">
          <code className="wallet-address">0x{wallet.address}</code> · index{' '}
          {wallet.derivationIndex}
        </span>
      </div>
      <div className="person-actions">
        {!archived && canReplace ? (
          <button
            type="button"
            className="btn btn-ghost"
            disabled={busy}
            onClick={() => onReplace(wallet)}
            aria-label={`Replace wallet ${wallet.label ?? wallet.address}`}
          >
            {busy ? <Spinner /> : 'Replace'}
          </button>
        ) : null}
      </div>
    </div>
  );
}

/**
 * The registry list: every wallet the account has registered, active and archived, in the
 * server's order. An empty registry is stated rather than hidden — a hidden list reads as a
 * broken one.
 */
export function WalletRegistryView({
  wallets,
  canReplace,
  busyId,
  onReplace,
}: {
  wallets: readonly WalletSummary[];
  canReplace: boolean;
  /** The wallet whose replacement is in flight, so only its row shows the busy state. */
  busyId: Id | null;
  onReplace: (wallet: WalletSummary) => void;
}): ReactNode {
  if (wallets.length === 0) {
    return <p className="muted">No wallets registered on this account yet.</p>;
  }
  return (
    <div className="session-list wallet-registry">
      {wallets.map((wallet) => (
        <WalletRowView
          key={wallet.walletId}
          wallet={wallet}
          canReplace={canReplace}
          busy={busyId === wallet.walletId}
          onReplace={onReplace}
        />
      ))}
    </div>
  );
}

/** One OLD/NEW line of the replacement preview: the label, and the address exactly as derived. */
function AddressLine({ label, address }: { label: string; address: string }): ReactNode {
  return (
    <div className="avax-prepared-line">
      <span className="avax-prepared-label">{label}</span>
      <span className="avax-prepared-value">
        <code className="wallet-address">{address}</code>
      </span>
    </div>
  );
}

/**
 * The replace flow's sheet content: the old and new addresses before the confirm that commits
 * them, the label the successor carries, and the honesty sentence §22 requires the person to
 * have read first.
 *
 * A successor that could not be derived (`newAddress` null, `problem` naming why) shows the
 * problem and no confirm — the flow never proceeds on an address it never showed.
 */
export function ReplaceWalletView({
  oldAddress,
  newAddress,
  derivationIndex,
  label,
  problem,
  busy,
  error,
  onLabel,
  onConfirm,
  onClose,
}: {
  /** The replaced wallet's address, as the registry holds it. */
  oldAddress: string;
  /** The successor's derived address, or `null` when it could not be derived here. */
  newAddress: string | null;
  /** The index the successor was derived at, for the confirm's own lines. */
  derivationIndex: number | null;
  /** The successor's label draft, defaulting to the replaced wallet's own. */
  label: string;
  /** Why no successor could be derived, when it could not. */
  problem: string | null;
  busy: boolean;
  /** The flow's failure line, separate from the derivation problem above. */
  error: string | null;
  onLabel: (value: string) => void;
  onConfirm: () => void;
  onClose: () => void;
}): ReactNode {
  return (
    <div className="replace-wallet">
      <p className="hint">{REPLACE_WALLET_HONESTY}</p>
      <div className="avax-prepared">
        <AddressLine label="OLD" address={oldAddress} />
        {newAddress !== null ? <AddressLine label="NEW" address={newAddress} /> : null}
      </div>
      {newAddress !== null && derivationIndex !== null ? (
        <label className="field-label">
          New wallet label
          <input
            type="text"
            className="input"
            value={label}
            // The server's own wallet-label ceiling (MAX_WALLET_LABEL_CHARS): a label longer
            // than the registry's limit is refused there, so it is stopped here, at the field.
            maxLength={60}
            onChange={(event) => onLabel(event.target.value)}
            aria-label="New wallet label"
          />
          <span className="field-hint">
            Derivation index {derivationIndex} — the successor is a new key, not a copy.
          </span>
        </label>
      ) : null}
      {problem !== null ? <p className="form-error">{problem}</p> : null}
      {error !== null ? <p className="form-error">{error}</p> : null}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" disabled={busy} onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="btn btn-danger"
          disabled={busy || newAddress === null}
          onClick={() => {
            if (!busy) {
              onConfirm();
            }
          }}
        >
          {busy ? <Spinner /> : 'Replace wallet'}
        </button>
      </div>
    </div>
  );
}

/**
 * The registry section: loads the account's wallets, and owns the replace flow's sheet.
 *
 * The successor is derived the moment the sheet opens, from the root and the registry together:
 * the address on the confirm screen is the address that will be registered, derived once and
 * shown before anything is sent.
 */
export function WalletRegistrySection(): ReactNode {
  const { client } = useMigo();

  const [wallets, setWallets] = useState<WalletSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // The replace flow: the target wallet, the derived successor, the label draft, and its state.
  const [target, setTarget] = useState<WalletSummary | null>(null);
  const [successor, setSuccessor] = useState<{
    address: string;
    derivationIndex: number;
  } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [label, setLabel] = useState('');
  const [replacing, setReplacing] = useState(false);
  const [replaceError, setReplaceError] = useState<string | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const rows = await client.wallets();
      setWallets(rows);
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // The root is read per attempt rather than cached in state: it is the store's own synchronous
  // answer, and a stale copy would offer a replacement this device can no longer derive.
  const root = client ? client.keyStore.root() : null;
  const hasRoot = root !== null;

  /** Opens the replace sheet for one wallet, deriving the successor before it is shown. */
  const beginReplace = useCallback(
    (wallet: WalletSummary): void => {
      if (client === null) {
        return;
      }
      setTarget(wallet);
      setReplaceError(null);
      setProblem(null);
      setSuccessor(null);
      // The successor's label defaults to the label being replaced: a "Primary" is succeeded by
      // a "Primary", and an unnamed wallet by the name the registry can at least sort by.
      setLabel(wallet.label ?? DEFAULT_WALLET_LABEL);
      if (root === null) {
        setProblem(REPLACE_NO_ROOT);
        return;
      }
      const index = wallets === null ? null : nextDerivationIndex(wallets);
      if (index === null) {
        setProblem(REPLACE_INDEX_CAPPED);
        return;
      }
      // Derived here, once: the address on the confirm is the address that will be registered.
      const derived = account.EvmWallet.fromRoot(root, index);
      setSuccessor({ address: derived.addressChecksummed(), derivationIndex: index });
    },
    [client, root, wallets],
  );

  const closeReplace = useCallback((): void => {
    setTarget(null);
    setSuccessor(null);
    setProblem(null);
    setLabel('');
    setReplaceError(null);
  }, []);

  /**
   * Runs the replacement in the only safe order: register the successor first, archive the old
   * wallet second — the account is never without an active wallet between the two calls. Each
   * half's failure is reported as the half it reached; nothing is rolled back silently, because
   * a registered successor is a fact the server already holds.
   */
  const confirmReplace = useCallback((): void => {
    if (!client || target === null || successor === null || replacing) {
      return;
    }
    const rootNow = client.keyStore.root();
    if (rootNow === null) {
      setReplaceError(REPLACE_NO_ROOT);
      return;
    }
    // The address on screen must be one this device can derive: a sheet carried over from
    // another device, or a stale derivation, is refused rather than registered blind.
    const derived = account.EvmWallet.fromRoot(rootNow, successor.derivationIndex);
    if (derived.addressChecksummed() !== successor.address) {
      setReplaceError('The derived address changed; close this and start the replacement again.');
      return;
    }
    const trimmed = label.trim();
    setReplacing(true);
    setReplaceError(null);
    void (async (): Promise<void> => {
      try {
        await client.registerWallet({
          // The registry's canonical form: lowercase hex, no 0x prefix (the wire contract the
          // SDK's WalletSummary documents); the checksummed form is for the person, not the wire.
          address: hexOf(derived.address()),
          derivationIndex: successor.derivationIndex,
          ...(trimmed.length > 0 ? { label: trimmed } : {}),
        });
      } catch (cause) {
        setReplaceError(friendlyError(cause));
        setReplacing(false);
        return; // nothing was archived; the account is exactly as it was
      }
      try {
        await client.archiveWallet({ wallet_id: target.walletId });
      } catch (cause) {
        // The half-done state, stated as itself: the successor is registered and active, the
        // predecessor is still active beside it, and the registry below shows both.
        setReplaceError(`${ARCHIVE_FAILED} ${friendlyError(cause)}`);
        setReplacing(false);
        void reload();
        return;
      }
      setNotice(
        `Wallet replaced — the active address is now ${successor.address}. Assets on the old address did not move.`,
      );
      setReplacing(false);
      closeReplace();
      void reload();
    })();
  }, [client, target, successor, label, replacing, closeReplace, reload]);

  return (
    <section className="panel-section" aria-label="Wallet registry">
      <h2 className="panel-heading">Wallet registry</h2>
      <p className="hint">
        Every address this account has registered. The registry is the account-level truth; the AVAX
        panel above keeps its wallet-0 focus.
      </p>
      {error !== null ? <p className="form-error">{error}</p> : null}
      {notice !== null ? <p className="hint">{notice}</p> : null}
      {wallets === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : (
        <>
          <WalletRegistryView
            wallets={wallets}
            canReplace={hasRoot}
            busyId={target !== null ? target.walletId : null}
            onReplace={beginReplace}
          />
          {!hasRoot ? <p className="hint">{REPLACE_NO_ROOT}</p> : null}
        </>
      )}

      {target !== null ? (
        <BottomSheet title="Replace wallet" onClose={closeReplace}>
          <ReplaceWalletView
            oldAddress={displayAddress(target.address)}
            newAddress={successor?.address ?? null}
            derivationIndex={successor?.derivationIndex ?? null}
            label={label}
            problem={problem}
            busy={replacing}
            error={replaceError}
            onLabel={setLabel}
            onConfirm={confirmReplace}
            onClose={closeReplace}
          />
        </BottomSheet>
      ) : null}
    </section>
  );
}
