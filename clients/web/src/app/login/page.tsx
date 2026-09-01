'use client';

import { useEffect, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { BottomSheet } from '@/components/bottom-sheet.js';
import { RestoreAccountSheet } from '@/components/restore-account-sheet.js';
import { ServerForm, transportLabel } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { ThemeToggle } from '@/components/theme-toggle.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { defaultServerEndpoint } from '@/lib/config.js';
import { loadAccountRecord } from '@/lib/storage/account-record-store.js';
import type { AccountRecord } from '@/lib/storage/account-record-store.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';

import type { KeyStore, ServerEndpoint } from '@migo/sdk';

/** Sign in to an existing account, then hand off to the chat shell. */
export default function LoginPage(): ReactNode {
  const { status, error, login } = useMigo();
  const router = useRouter();

  const [identifier, setIdentifier] = useState('');
  const [password, setPassword] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
  const [serverSheetOpen, setServerSheetOpen] = useState(false);
  const [restoreSheetOpen, setRestoreSheetOpen] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const submitting = status === 'connecting';

  // The remembered account: when this browser has signed in before, the card collapses to a chip
  // and a password. `full` is the both-fields form — the first visit, a shared browser, or the
  // user's own choice against the remembered name.
  const [record, setRecord] = useState<AccountRecord | null>(null);
  const [mode, setMode] = useState<'saved' | 'full'>('full');
  // The founding-grade store a restored `.migo` container produced, handed to login so the
  // session runs as the account's founding device rather than a fresh additional one.
  const [restoredStore, setRestoredStore] = useState<KeyStore | null>(null);
  // The auto-prefill happens once: a user who chose "Use a different account" before the record
  // finished loading must not have their typing overwritten by a late arrival.
  const offeredRef = useRef(false);

  useEffect(() => {
    if (status === 'ready') {
      router.replace('/chat');
    }
  }, [status, router]);

  // Pre-fill the server from the persisted endpoint, falling back to the build's default the
  // register screen uses — a first visit has no snapshot, and without the fallback the card
  // would render no server link and a Sign-in button that can never be pressed.
  useEffect(() => {
    let cancelled = false;
    void loadServerEndpoint().then((stored) => {
      if (cancelled) return;
      setEndpoint(stored ?? defaultServerEndpoint());
      setEndpointReady(true);
    });
    void loadAccountRecord().then((remembered) => {
      if (cancelled || remembered === undefined || offeredRef.current) return;
      offeredRef.current = true;
      setRecord(remembered);
      // Only collapse to the chip when the identifier field is still untouched — typing in it is
      // the user's own answer to "which account?", and it wins over memory.
      setMode((current) => (current === 'full' && identifier === '' ? 'saved' : current));
    });
    return () => {
      cancelled = true;
    };
    // `identifier` is deliberately not a dependency: the prefill decision is made once, when the
    // record arrives, against the field's value at that moment.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function onSubmit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (submitting || endpoint === null) {
      return;
    }
    const who = mode === 'saved' && record !== null ? record.username : identifier;
    if (who.trim() === '') {
      return;
    }
    // Persist the chosen endpoint *before* the bootstrap call, so a mid-flight failure can be retried
    // against the same server without the form losing the address the user just confirmed.
    try {
      await saveServerEndpoint(endpoint);
    } catch {
      // Best-effort: a failed local write is not a reason to refuse the in-flight attempt.
    }
    setValidationError(null);
    try {
      await login(
        { identifier: who, password },
        endpoint,
        null,
        restoredStore === null ? undefined : restoredStore,
      );
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
        <p className="auth-sub">Sign in to your private, end-to-end encrypted account.</p>

        {mode === 'saved' && record !== null ? (
          <div className="auth-account-chip">
            Continue as <strong>{record.username}</strong>
            <button
              type="button"
              className="auth-restore-link"
              onClick={() => {
                setMode('full');
                setIdentifier('');
              }}
            >
              Use a different account
            </button>
          </div>
        ) : null}

        {mode === 'full' ? (
          <label className="field-label">
            Username, email, or phone
            <input
              type="text"
              value={identifier}
              onChange={(event) => setIdentifier(event.target.value)}
              autoComplete="username"
              autoFocus
              required
            />
          </label>
        ) : null}

        <label className="field-label">
          Password
          <input
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="current-password"
            required
            autoFocus={mode === 'saved'}
          />
        </label>

        {restoredStore !== null ? (
          <p className="hint" role="status">
            Account file restored — sign in with your username and password.
          </p>
        ) : null}
        {validationError ? <p className="form-error">{validationError}</p> : null}
        {error ? <p className="form-error">{error}</p> : null}

        <button
          type="submit"
          className="btn btn-primary btn-block"
          disabled={submitting || endpoint === null}
        >
          {submitting ? <Spinner /> : 'Sign in'}
        </button>

        <p className="auth-alt">
          New to Migo? <Link href="/register">Create an account</Link>
        </p>

        {endpointReady && endpoint !== null ? (
          <div className="auth-card-links">
            {restoredStore === null ? (
              <button
                type="button"
                className="auth-restore-link"
                onClick={() => setRestoreSheetOpen(true)}
              >
                Restore from account file
              </button>
            ) : null}
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

      {restoreSheetOpen ? (
        <BottomSheet title="Restore your account" onClose={() => setRestoreSheetOpen(false)}>
          <RestoreAccountSheet
            onRestored={(store) => {
              setRestoredStore(store);
              setRestoreSheetOpen(false);
              // The username lives on the server, not in the file — the full form is where it is
              // typed, so the restore always ends at the both-fields sign-in.
              setMode('full');
            }}
            onCancel={() => setRestoreSheetOpen(false)}
          />
        </BottomSheet>
      ) : null}
    </main>
  );
}
