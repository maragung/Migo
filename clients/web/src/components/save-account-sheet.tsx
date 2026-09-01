'use client';

/**
 * The offer a fresh registration sees: the account file, sealed and downloaded.
 *
 * Registration makes this browser the founding device — the root is minted here, the E2EE identity
 * is derived from it, and both are already sealed into this browser's IndexedDB. What no server
 * holds is the root itself, so the `.migo` container is the only way the account can ever appear
 * on another device: one file, encrypted under a recovery credential the user chooses now (§182 —
 * not their password, not an e-mail, nothing the server could reset).
 *
 * The credential is judged locally before any Argon2id work is spent on it; the sealing itself is
 * the crypto package's {@link account.sealContainer}, and a successful download says so in one
 * line and stays re-pressable, because a download the browser swallowed silently is not a saved
 * file. "Later" is an honest choice — the sheet can be declined — but it is the only moment the
 * offer is made, so the lead line says what declining means.
 */

import { useState } from 'react';
import type { ReactNode } from 'react';

import { account } from '@migo/sdk';

import { containerFileName, credentialProblem, downloadAccountFile } from '@/lib/account-file.js';

import { Spinner } from './spinner.js';

export function SaveAccountSheet({
  username,
  accountId,
  root,
  onDone,
}: {
  /** The account's username, for the file name. */
  username: string;
  /** The server-side account id, sealed into the container so a restoring device can name the account. */
  accountId: string;
  /** The account root as raw bytes, from the live key store; `null` on a device that somehow holds none. */
  root: Uint8Array | null;
  /** Called when the user is finished with the offer, either way. */
  onDone: () => void;
}): ReactNode {
  const [credential, setCredential] = useState('');
  const [confirm, setConfirm] = useState('');
  const [sealing, setSealing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const problem = credentialProblem(credential, confirm);
  const canSeal = !sealing && credential.length > 0 && problem === null;

  /** Seals the root into a container and offers it as a download. */
  function seal(): void {
    if (root === null) {
      return;
    }
    const judged = credentialProblem(credential, confirm);
    if (judged !== null) {
      setError(judged);
      return;
    }
    setError(null);
    setSealing(true);
    void (async (): Promise<void> => {
      try {
        const file = account.AccountFile.forRoot(
          account.MigoRoot.fromBytes(root),
          Math.floor(Date.now() / 1000),
        ).forAccount(accountId);
        const bytes = await account.sealContainer(credential, file);
        downloadAccountFile(bytes, containerFileName(username));
        setSaved(true);
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : 'The account file could not be sealed.');
      } finally {
        setSealing(false);
      }
    })();
  }

  if (root === null) {
    return (
      <div className="save-account">
        <p className="hint">
          This device does not hold the account root, so there is no account file to save.
        </p>
        <div className="form-actions">
          <button type="button" className="btn btn-ghost" onClick={onDone}>
            Later
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="save-account">
      <p className="hint">
        Your account is saved to this browser automatically. The account file is the only way to
        move it to another device — no server holds a copy of your keys.
      </p>
      <label className="field-label">
        Recovery credential
        <input
          type="password"
          value={credential}
          onChange={(event) => {
            setCredential(event.target.value);
            setSaved(false);
          }}
          autoComplete="off"
          placeholder="a phrase only you know"
        />
        <span className="field-hint">
          At least 8 characters. This unlocks the file — it is not your Migo password.
        </span>
      </label>
      <label className="field-label">
        Confirm recovery credential
        <input
          type="password"
          value={confirm}
          onChange={(event) => {
            setConfirm(event.target.value);
            setSaved(false);
          }}
          autoComplete="off"
        />
      </label>
      {credential.length > 0 && problem !== null ? <p className="form-error">{problem}</p> : null}
      {error !== null ? <p className="form-error">{error}</p> : null}
      {saved ? <p className="hint">Account file downloaded — keep it somewhere safe.</p> : null}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone}>
          Later
        </button>
        <button type="button" className="btn btn-primary" disabled={!canSeal} onClick={seal}>
          {sealing ? <Spinner /> : 'Download account file'}
        </button>
      </div>
    </div>
  );
}
