'use client';

/**
 * The offer a fresh registration sees: the account key file, sealed with the passphrase the user
 * just typed, and the way out to the sign-in page.
 *
 * Registration makes this browser the founding device — the root is minted here, the E2EE identity
 * is derived from it, and both are already sealed into this browser's IndexedDB. What no server
 * holds is the root itself, so the `.migo` container is the only way the account can ever appear
 * on another device (§182): one file, encrypted under the registration passphrase — the same one
 * that unlocks the account, because a second secret to keep straight is a second secret to lose.
 *
 * The container is sealed as soon as the sheet opens, and the sealed bytes are remembered in this
 * browser's key-file store on the spot — that is what the lead line's "saved to this browser
 * automatically" has meant since the login screen grew its account list. Downloading the file is
 * offered with the same bytes, no second Argon2id run, and stays re-pressable: a download the
 * browser swallowed silently is not a saved file. "Go to sign-in" ends the registration the way
 * every later visit begins — the account's owner signs in with this file and the passphrase
 * themselves — so the button waits for the seal, and declining the download is an honest choice
 * too: the lead line says what declining means.
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { account } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { containerFileName, downloadAccountFile } from '@/lib/account-file.js';
import { keyFileId, saveKeyFile } from '@/lib/storage/key-file-store.js';

import { Icon } from './icons.js';
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
  const [sealed, setSealed] = useState<Uint8Array | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const fileName = containerFileName(username);

  // Seal once as the sheet opens, then remember the bytes: the browser save and the offered
  // download are the same container, and the account lands on the login screen's list before
  // the user answers either button. The root is only in memory while registration is this
  // fresh — waiting for a button press is the version where a dismissed sheet means a file
  // that never existed.
  useEffect(() => {
    if (root === null) {
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      try {
        const file = account.AccountFile.forRoot(
          account.MigoRoot.fromBytes(root),
          Math.floor(Date.now() / 1000),
        ).forAccount(accountId);
        const bytes = await account.sealContainer(passphrase, file);
        if (cancelled) return;
        setSealed(bytes);
        await saveKeyFile({
          id: keyFileId(bytes),
          bytes,
          fileName,
          username,
          accountId: accountId as Id,
          savedAt: Date.now(),
        }).catch(() => {
          // Best-effort: the download still works, and the next file sign-in saves the row.
        });
      } catch (cause) {
        if (!cancelled) {
          setError(
            cause instanceof Error ? cause.message : 'The account file could not be sealed.',
          );
        }
      }
    })();
    return () => {
      cancelled = true;
    };
    // The seal runs once per sheet; the passphrase and root are fixed the moment it opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Offers the already-sealed bytes as a download. */
  function download(): void {
    if (sealed === null) {
      return;
    }
    downloadAccountFile(sealed, fileName);
    setSaved(true);
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
      <div className="save-account-badge" aria-hidden="true">
        <Icon name="file" size={24} />
      </div>
      <p className="save-account-lead">
        Your account is saved to this browser automatically. The key file is the only way to move it
        to another device — no server holds a copy of your keys — and the only way to sign in again
        after this browser forgets you. Download it and keep it somewhere safe.
      </p>
      <p className="save-account-sub">
        The file is sealed with your passphrase. Signing in means this file and that passphrase,
        nothing else — the next screen asks you for both.
      </p>
      {error !== null ? <p className="form-error">{error}</p> : null}
      {saved ? (
        <p className="save-account-done">Key file downloaded — keep it somewhere safe.</p>
      ) : null}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone} disabled={sealed === null}>
          {sealed === null ? <Spinner /> : null}
          Go to sign-in
        </button>
        <button
          type="button"
          className="btn btn-primary"
          disabled={sealed === null}
          onClick={download}
        >
          {sealed === null ? <Spinner /> : <Icon name="download" size={20} />}
          Download key file
        </button>
      </div>
    </div>
  );
}
