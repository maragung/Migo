'use client';

import { useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { BottomSheet } from '@/components/bottom-sheet.js';
import { SaveAccountSheet } from '@/components/save-account-sheet.js';
import { ServerForm, transportLabel } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { ThemeToggle } from '@/components/theme-toggle.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { defaultServerEndpoint } from '@/lib/config.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';

import type { ServerEndpoint } from '@migo/sdk';

/** The gender options the profile accepts, in the server's numbering (1 male, 2 female, 3 other). */
const GENDERS = [
  { value: 1, label: 'Male' },
  { value: 2, label: 'Female' },
  { value: 3, label: 'Other' },
];

/**
 * Create a new account. Identity keys are generated on this device and never leave it.
 *
 * The one passphrase this form collects is the account's whole secret surface: it is the password
 * the server verifies at the founding registration, and it is the credential that seals the `.migo`
 * key file offered right after — which is why the file download needs no second passphrase to be
 * typed and why the sign-in screen asks for the file and this passphrase and nothing else.
 */
export default function RegisterPage(): ReactNode {
  const { status, error, register, client, accountId } = useMigo();
  const router = useRouter();

  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [email, setEmail] = useState('');
  const [gender, setGender] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
  const [serverSheetOpen, setServerSheetOpen] = useState(false);
  const [saveOfferOpen, setSaveOfferOpen] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const submitting = status === 'connecting';

  useEffect(() => {
    // The hand-off to the chat shell waits out the key-file offer: the root is only in memory
    // until this moment passes, and the offer is the one chance to seal it into a file.
    if (status === 'ready' && !saveOfferOpen) {
      router.replace('/chat');
    }
  }, [status, saveOfferOpen, router]);

  useEffect(() => {
    let cancelled = false;
    void loadServerEndpoint().then((stored) => {
      if (cancelled) return;
      // A fresh visitor (no stored endpoint) still gets a working form against the build's
      // default host, and the submit button is enabled; the user can open the server sheet to
      // point at a self-hosted server without ever leaving the page in a disabled state.
      setEndpoint(stored ?? defaultServerEndpoint());
      setEndpointReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onSubmit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (submitting || endpoint === null) {
      return;
    }
    try {
      await saveServerEndpoint(endpoint);
    } catch {
      // Best-effort, see login.
    }
    setValidationError(null);
    try {
      await register(
        {
          username,
          password,
          email: email || undefined,
          gender: gender === '' ? undefined : Number(gender),
        },
        endpoint,
        null,
      );
      // The account exists and this browser holds its founding root; offer the one-time key-file
      // download before the redirect carries the user away.
      setSaveOfferOpen(true);
    } catch {
      // The provider surfaces the reason through `error`; keep the form populated for a retry.
    }
  }

  // Same shape as the sign-in card: the server choice lives in a sheet behind a bottom-corner
  // link, and a commit from the sheet is the only thing that changes the endpoint here.
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
        <p className="auth-sub">Create an account — your keys are made here and never leave it.</p>

        <label className="field-label">
          Username
          <input
            type="text"
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            autoComplete="username"
            placeholder="the name friends will find you by"
            spellCheck={false}
            autoFocus
            required
          />
          <span className="field-hint">Your username can never be changed.</span>
        </label>

        <label className="field-label">
          Email <span className="muted">(optional)</span>
          <input
            type="email"
            value={email}
            onChange={(event) => setEmail(event.target.value)}
            autoComplete="email"
            placeholder="you@example.com"
          />
        </label>

        <label className="field-label">
          Passphrase
          <input
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="new-password"
            minLength={10}
            required
          />
          <span className="field-hint">
            At least 10 characters. This one passphrase unlocks your account and your key file —
            keep it somewhere safe.
          </span>
        </label>

        <label className="field-label">
          Gender <span className="muted">(optional)</span>
          <select value={gender} onChange={(event) => setGender(event.target.value)}>
            <option value="">Prefer not to say</option>
            {GENDERS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>

        {validationError ? <p className="form-error">{validationError}</p> : null}
        {error ? <p className="form-error">{error}</p> : null}

        <button
          type="submit"
          className="btn btn-primary btn-block"
          disabled={submitting || endpoint === null}
        >
          {submitting ? <Spinner /> : 'Create account'}
        </button>

        <p className="auth-alt">
          Already have an account? <Link href="/login">Sign in</Link>
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

      {saveOfferOpen ? (
        <BottomSheet title="Your account key file" onClose={() => setSaveOfferOpen(false)}>
          <SaveAccountSheet
            username={username.trim()}
            accountId={accountId ?? ''}
            root={client?.keyStore.root()?.asBytes() ?? null}
            passphrase={password}
            onDone={() => setSaveOfferOpen(false)}
          />
        </BottomSheet>
      ) : null}
    </main>
  );
}
