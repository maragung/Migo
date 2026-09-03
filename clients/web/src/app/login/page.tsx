'use client';

import { useEffect, useState } from 'react';
import type { ChangeEvent, FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { BottomSheet } from '@/components/bottom-sheet.js';
import { ServerForm, transportLabel } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { ThemeToggle } from '@/components/theme-toggle.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { defaultServerEndpoint } from '@/lib/config.js';
import { RESTORE_FAILED } from '@/lib/account-file.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';

import type { ServerEndpoint } from '@migo/sdk';

/**
 * Sign in with the account key file, then hand off to the chat shell.
 *
 * The `.migo` file downloaded at registration and its passphrase are the whole sign-in: the file
 * carries the account root, the passphrase unseals it, and the ML-DSA identity ceremony (not a
 * password) turns the root into a session. There is no username field — the file names the
 * account — and no server-side secret to type, because no server holds one that could open the
 * account.
 */
export default function LoginPage(): ReactNode {
  const { status, error, loginWithFile } = useMigo();
  const router = useRouter();

  const [file, setFile] = useState<File | null>(null);
  const [passphrase, setPassphrase] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
  const [serverSheetOpen, setServerSheetOpen] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const submitting = status === 'connecting';

  useEffect(() => {
    if (status === 'ready') {
      router.replace('/chat');
    }
  }, [status, router]);

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
    setFile(event.target.files?.[0] ?? null);
    setValidationError(null);
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (submitting || endpoint === null || file === null) {
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
      const bytes = new Uint8Array(await file.arrayBuffer());
      await loginWithFile(bytes, passphrase, endpoint);
    } catch {
      // The provider surfaces the reason through `error`; keep the form populated for a retry.
    }
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

  const canSubmit = !submitting && file !== null && passphrase.length > 0 && endpoint !== null;

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
          Sign in with your account key file — your keys are yours alone, and no password of yours
          is stored anywhere.
        </p>

        <label className="field-label">
          Account key file
          <input
            type="file"
            accept=".migo,application/octet-stream"
            onChange={onFileChange}
            required
            autoFocus
          />
          <span className="field-hint">The .migo file you downloaded when you registered.</span>
        </label>

        <label className="field-label">
          Passphrase
          <input
            type="password"
            value={passphrase}
            onChange={(event) => setPassphrase(event.target.value)}
            autoComplete="current-password"
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
