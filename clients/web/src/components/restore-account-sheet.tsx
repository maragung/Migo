'use client';

/**
 * The restore side of the account file: pick the `.migo` container, give the credential, become
 * the founding device again.
 *
 * Opening a container yields the root, and the root reproduces the founding identity
 * deterministically — `KeyStore.founding` derives the same E2EE seeds the original device
 * published — so a restored browser is not an additional device reading only new messages; it is
 * the account again, with its history. The container reader cannot distinguish a wrong credential
 * from a tampered or foreign file (§182), so this sheet shows one honest line for every failure
 * and never guesses which.
 *
 * Signing in after a restore is unchanged — username and password as usual — which is why the
 * restored store is handed to the caller rather than signed in here: the password is the user's
 * proof to the server, the credential was only the file's lock.
 */

import { useState } from 'react';
import type { ChangeEvent, ReactNode } from 'react';

import { KeyStore, account } from '@migo/sdk';

import { RESTORE_FAILED } from '@/lib/account-file.js';

import { Spinner } from './spinner.js';

export function RestoreAccountSheet({
  onRestored,
  onCancel,
}: {
  /** Called with the founding-grade key store once the container has opened. */
  onRestored: (store: KeyStore) => void;
  /** Called when the user abandons the restore. */
  onCancel: () => void;
}): ReactNode {
  const [file, setFile] = useState<File | null>(null);
  const [credential, setCredential] = useState('');
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canOpen = !opening && file !== null && credential.length > 0;

  function onFileChange(event: ChangeEvent<HTMLInputElement>): void {
    setFile(event.target.files?.[0] ?? null);
    setError(null);
  }

  /** Opens the container and hands the rebuilt founding store to the caller. */
  function open(): void {
    if (file === null) {
      return;
    }
    setOpening(true);
    setError(null);
    void (async (): Promise<void> => {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const opened = await account.openContainer(credential, bytes);
        onRestored(KeyStore.founding(opened.rootSecret()));
      } catch {
        // One line for a wrong credential, a tampered byte, and a foreign file alike — the reader
        // cannot tell them apart, so neither does the screen (§182).
        setError(RESTORE_FAILED);
      } finally {
        setOpening(false);
      }
    })();
  }

  return (
    <div className="restore-account">
      <p className="hint">
        Pick the .migo account file you saved when you registered, and the recovery credential that
        seals it. Signing in after this uses your username and password as usual.
      </p>
      <label className="field-label">
        Account file
        <input type="file" accept=".migo,application/octet-stream" onChange={onFileChange} />
      </label>
      <label className="field-label">
        Recovery credential
        <input
          type="password"
          value={credential}
          onChange={(event) => {
            setCredential(event.target.value);
            setError(null);
          }}
          autoComplete="off"
        />
      </label>
      {error !== null ? <p className="form-error">{error}</p> : null}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={opening}>
          Cancel
        </button>
        <button type="button" className="btn btn-primary" disabled={!canOpen} onClick={open}>
          {opening ? <Spinner /> : 'Restore'}
        </button>
      </div>
    </div>
  );
}
