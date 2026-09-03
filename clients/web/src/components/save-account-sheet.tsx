'use client';

/**
 * The offer a fresh registration sees: the account key file, sealed with the passphrase the user
 * just typed, and the choice to continue into the app.
 *
 * Registration makes this browser the founding device — the root is minted here, the E2EE identity
 * is derived from it, and both are already sealed into this browser's IndexedDB. What no server
 * holds is the root itself, so the `.migo` container is the only way the account can ever appear
 * on another device (§182): one file, encrypted under the registration passphrase — the same one
 * that unlocks the account, because a second secret to keep straight is a second secret to lose.
 *
 * That is also why the sheet asks for nothing: the passphrase is handed in by the register screen
 * that just collected it, the sealing is the crypto package's {@link account.sealContainer}, and a
 * successful download says so in one line and stays re-pressable, because a download the browser
 * swallowed silently is not a saved file. "Continue" is the sign-in-now choice — the registration
 * already opened the session, so continuing is simply walking through the door it opened — but
 * declining the download is an honest choice too, and the lead line says what declining means.
 */

import { useState } from 'react';
import type { ReactNode } from 'react';

import { account } from '@migo/sdk';

import { containerFileName, downloadAccountFile } from '@/lib/account-file.js';

import { Spinner } from './spinner.js';

export function SaveAccountSheet({
  username,
  accountId,
  root,
  passphrase,
  onDone,
}: {
  /** The account's username, for the file name. */
  username: string;
  /** The server-side account id, sealed into the container so a restoring device can name the account. */
  accountId: string;
  /** The account root as raw bytes, from the live key store; `null` on a device that somehow holds none. */
  root: Uint8Array | null;
  /** The registration passphrase, which seals the file and later opens it on the sign-in screen. */
  passphrase: string;
  /** Called when the user is finished with the offer, either way. */
  onDone: () => void;
}): ReactNode {
  const [sealing, setSealing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  /** Seals the root into a container and offers it as a download. */
  function seal(): void {
    if (root === null) {
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
        const bytes = await account.sealContainer(passphrase, file);
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
            Continue
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="save-account">
      <p className="hint">
        Your account is saved to this browser automatically. The key file is the only way to move it
        to another device — no server holds a copy of your keys — and the only way to sign in again
        after this browser forgets you. Download it and keep it somewhere safe.
      </p>
      <p className="hint">
        The file is sealed with your passphrase. Signing in later means this file and that
        passphrase, nothing else.
      </p>
      {error !== null ? <p className="form-error">{error}</p> : null}
      {saved ? <p className="hint">Key file downloaded — keep it somewhere safe.</p> : null}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone}>
          Continue
        </button>
        <button type="button" className="btn btn-primary" disabled={sealing} onClick={seal}>
          {sealing ? <Spinner /> : 'Download key file'}
        </button>
      </div>
    </div>
  );
}
