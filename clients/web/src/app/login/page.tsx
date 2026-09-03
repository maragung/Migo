'use client';

import { useEffect, useRef, useState } from 'react';
import type { ChangeEvent, FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { BottomSheet } from '@/components/bottom-sheet.js';
import { Icon } from '@/components/icons.js';
import { ServerForm, transportLabel } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { ThemeToggle } from '@/components/theme-toggle.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { defaultServerEndpoint } from '@/lib/config.js';
import { RESTORE_FAILED } from '@/lib/account-file.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';
import { loadKeyFiles, removeKeyFile } from '@/lib/storage/key-file-store.js';
import type { SavedKeyFile } from '@/lib/storage/key-file-store.js';

import type { ServerEndpoint } from '@migo/sdk';

/**
 * Sign in with the account key file, then hand off to the chat shell.
 *
 * The `.migo` file downloaded at registration and its passphrase are the whole sign-in: the file
 * carries the account root, the passphrase unseals it, and the ML-DSA identity ceremony (not a
 * passphrase) turns the root into a session. There is no username field — the file names the
 * account — and no server-side secret to type, because no server holds one that could open the
 * account.
 *
 * # What this browser remembers
 *
 * Every key file that has signed in here is remembered as its sealed bytes (ciphertext and
 * header, never the passphrase), so a returning user picks their account from a list and types
 * only the passphrase. A browser that has never imported a file offers the one big File tile
 * instead. The two sources meet at the same submit: a chosen row or a freshly imported file is
 * a byte array and a passphrase, and the ceremony does not care which door it came through.
 */
export default function LoginPage(): ReactNode {
  const { status, error, loginWithFile } = useMigo();
  const router = useRouter();

  // The sealed files this browser remembers; `null` until the store answers, so the first paint
  // does not flash the never-imported tile at someone whose accounts are already here.
  const [saved, setSaved] = useState<SavedKeyFile[] | null>(null);
  // The saved row currently chosen, by its salt id. `null` while a fresh import is in hand.
  const [chosenId, setChosenId] = useState<string | null>(null);
  // A file picked from disk this visit: its bytes and its name, not yet remembered.
  const [imported, setImported] = useState<{ name: string; bytes: Uint8Array } | null>(null);
  const [passphrase, setPassphrase] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
  const [serverSheetOpen, setServerSheetOpen] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const submitting = status === 'connecting';

  useEffect(() => {
    if (status === 'ready') {
      router.replace('/chat');
    }
  }, [status, router]);

  // The remembered files load once; the newest is pre-selected, because "sign in as me" is what
  // nearly every visit to this page is. A storage failure is not a sign-in failure: the page
  // falls back to the import tile and the ceremony works exactly as it did before lists existed.
  useEffect(() => {
    let cancelled = false;
    void loadKeyFiles()
      .then((rows) => {
        if (cancelled) return;
        setSaved(rows);
        // `rows[0]?.id` rather than a length guard: noUncheckedIndexedAccess narrows
        // nothing on `rows.length > 0`, and the fallback picks the same empty state.
        setChosenId(rows[0]?.id ?? null);
      })
      .catch(() => {
        if (!cancelled) setSaved([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Pre-fill the server from the persisted endpoint, falling back to the build's default — a first
  // visit has no stored endpoint, and without the fallback the card would render no server link
  // and a Sign-in button that can never be pressed.
  useEffect(() => {
    let cancelled = false;
    void loadServerEndpoint().then((stored) => {
      if (cancelled) return;
      setEndpoint(stored ?? defaultServerEndpoint());
      setEndpointReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  function onFileChange(event: ChangeEvent<HTMLInputElement>): void {
    const picked = event.target.files?.[0] ?? null;
    // A cancel out of the picker is not a choice: whatever was selected before stays selected.
    if (picked !== null) {
      void picked.arrayBuffer().then((buffer) => {
        setImported({ name: picked.name, bytes: new Uint8Array(buffer) });
        setChosenId(null);
        setValidationError(null);
      });
    }
    // Re-arming the input: picking the same file twice must still fire a change event.
    event.target.value = '';
  }

  /** The bytes and name the submit will use, from whichever door the account came through. */
  function selected(): { name: string; bytes: Uint8Array } | null {
    if (imported !== null) {
      return imported;
    }
    const row = saved?.find((file) => file.id === chosenId);
    return row === undefined ? null : { name: row.fileName, bytes: row.bytes };
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const source = selected();
    if (submitting || endpoint === null || source === null) {
      return;
    }
    // Persist the chosen endpoint *before* the ceremony, so a mid-flight failure can be retried
    // against the same server without the form losing the address the user just confirmed.
    try {
      await saveServerEndpoint(endpoint);
    } catch {
      // Best-effort: a failed local write is not a reason to refuse the in-flight attempt.
    }
    setValidationError(null);
    try {
      await loginWithFile(source.bytes, passphrase, endpoint, source.name);
    } catch {
      // The provider surfaces the reason through `error`; keep the form populated for a retry.
    }
  }

  /** Forgets one remembered file. Forgetting the chosen row falls back to the newest that remains. */
  function onForget(file: SavedKeyFile): void {
    void removeKeyFile(file.id)
      .then(() => loadKeyFiles())
      .then((rows) => {
        setSaved(rows);
        if (imported === null) {
          setChosenId((current) => (current === file.id ? (rows[0]?.id ?? null) : current));
        }
      })
      .catch(() => {
        // The store refused; the list stays as it was and the account stays remembered.
      });
  }

  // The server choice commits from the sheet, not from the card: the card keeps only the small
  // bottom-corner link that opens it, and every host/port/scheme change happens in the sheet and
  // lands here at once. A transport tap inside the sheet also lands here, immediately.
  function onServerCommit(next: ServerEndpoint): void {
    setEndpoint(next);
    setValidationError(null);
  }

  function onServerConfirmed(next: ServerEndpoint): void {
    onServerCommit(next);
    setServerSheetOpen(false);
  }

  const source = selected();
  const canSubmit = !submitting && source !== null && passphrase.length > 0 && endpoint !== null;
  const remembered = saved ?? [];

  return (
    <main className="auth-screen">
      <ThemeToggle className="auth-theme-toggle" />
      <form className="auth-card" onSubmit={(event) => void onSubmit(event)}>
        <div className="auth-brand">
          <span className="brand-mark" aria-hidden="true">
            ◆
          </span>
          <h1>Migo</h1>
        </div>
        <p className="auth-sub">
          Sign in with your account key file — your keys are yours alone, and no passphrase of yours
          is stored anywhere.
        </p>

        {/* The picker is a real input, because accessibility and mobile file sheets come free
            that way — but it is hidden: what the card shows is the File tile and the remembered
            list, never the browser's filename textbox. */}
        <input
          ref={fileInput}
          type="file"
          accept=".migo,application/octet-stream"
          onChange={onFileChange}
          hidden
        />

        {saved === null ? null : imported !== null ? (
          <div className="keyfile-imported">
            <Icon name="file" size={24} />
            <div className="keyfile-imported-text">
              <span className="keyfile-name">{imported.name}</span>
              <span className="keyfile-note">Signing in with this file for the first time.</span>
            </div>
            <button
              type="button"
              className="keyfile-clear"
              aria-label="Choose a different key file"
              onClick={() => {
                setImported(null);
                setChosenId(remembered[0]?.id ?? null);
              }}
            >
              <Icon name="close" size={16} />
            </button>
          </div>
        ) : remembered.length > 0 ? (
          <div className="keyfile-list" role="radiogroup" aria-label="Accounts on this browser">
            {remembered.map((file) => (
              <div key={file.id} className={`keyfile-row${file.id === chosenId ? ' chosen' : ''}`}>
                <button
                  type="button"
                  role="radio"
                  aria-checked={file.id === chosenId}
                  className="keyfile-pick"
                  onClick={() => setChosenId(file.id)}
                >
                  <Icon name="file" size={20} />
                  <span className="keyfile-name">{file.username || file.fileName}</span>
                  {file.username === '' ? (
                    <span className="keyfile-note">Never signed in here</span>
                  ) : null}
                </button>
                <button
                  type="button"
                  className="keyfile-clear"
                  aria-label={`Forget ${file.username || file.fileName}`}
                  onClick={() => onForget(file)}
                >
                  <Icon name="close" size={16} />
                </button>
              </div>
            ))}
            <button
              type="button"
              className="keyfile-add"
              onClick={() => fileInput.current?.click()}
            >
              <Icon name="file" size={20} />
              Use another key file
            </button>
          </div>
        ) : (
          <button type="button" className="keyfile-tile" onClick={() => fileInput.current?.click()}>
            <Icon name="file" size={24} />
            <span className="keyfile-tile-title">Import your key file</span>
            <span className="keyfile-note">The .migo file you downloaded when you registered.</span>
          </button>
        )}

        <label className="field-label">
          Passphrase
          <input
            type="passphrase"
            value={passphrase}
            onChange={(event) => setPassphrase(event.target.value)}
            autoComplete="current-passphrase"
            required
          />
          <span className="field-hint">The passphrase you chose when the file was saved.</span>
        </label>

        {validationError ? <p className="form-error">{validationError}</p> : null}
        {error ? (
          <p className="form-error" role="alert">
            {error === RESTORE_FAILED ? RESTORE_FAILED : error}
          </p>
        ) : null}

        <button type="submit" className="btn btn-primary btn-block" disabled={!canSubmit}>
          {submitting ? <Spinner /> : 'Sign in'}
        </button>

        <p className="auth-alt">
          New to Migo? <Link href="/register">Create an account</Link>
        </p>

        {endpointReady && endpoint !== null ? (
          <div className="auth-card-links">
            <button
              type="button"
              className="auth-server-link"
              onClick={() => setServerSheetOpen(true)}
            >
              Server · {endpoint.host}:{endpoint.port} · {transportLabel(endpoint.transport)}
            </button>
          </div>
        ) : null}
      </form>

      {serverSheetOpen && endpoint !== null ? (
        <BottomSheet title="Server" onClose={() => setServerSheetOpen(false)}>
          <ServerForm
            value={endpoint}
            onCommit={onServerConfirmed}
            onTransportPick={onServerCommit}
          />
        </BottomSheet>
      ) : null}
    </main>
  );
}
