'use client';

import { useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { BottomSheet } from '@/components/bottom-sheet.js';
import { CaptchaWidget } from '@/components/captcha-widget.js';
import { SaveAccountSheet } from '@/components/save-account-sheet.js';
import { ServerForm, transportLabel } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { ThemeToggle } from '@/components/theme-toggle.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { defaultServerEndpoint } from '@/lib/config.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';

import type { CaptchaProof, ServerEndpoint } from '@migo/sdk';

/** Create a new account. Identity keys are generated on this device and never leave it. */
export default function RegisterPage(): ReactNode {
  const { status, error, register, client, accountId } = useMigo();
  const router = useRouter();

  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [email, setEmail] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
  const [serverSheetOpen, setServerSheetOpen] = useState(false);
  const [saveOfferOpen, setSaveOfferOpen] = useState(false);
  const [captcha, setCaptcha] = useState<CaptchaProof | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);
  const submitting = status === 'connecting';

  useEffect(() => {
    // The hand-off to the chat shell waits out the save-account offer: the root is only in
    // memory until this moment passes, and the offer is the one chance to seal it into a file.
    if (status === 'ready' && !saveOfferOpen) {
      router.replace('/chat');
    }
  }, [status, saveOfferOpen, router]);

  useEffect(() => {
    let cancelled = false;
    void loadServerEndpoint().then((stored) => {
      if (cancelled) return;
      // A fresh visitor (no stored endpoint) still gets a working form
      // against the build's default host. The card stays sparse — just the
      // fields, the captcha widget, and the bottom-corner server link — and
      // the submit button is enabled; the user can open the server sheet to
      // point at a self-hosted server without ever leaving the page in a
      // disabled state.
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
      await register({ username, password, email: email || undefined }, endpoint, captcha);
      // The account exists and this browser holds its founding root; offer the one-time file save
      // before the redirect carries the user away.
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
      <form className="auth-card auth-card-wide" onSubmit={(event) => void onSubmit(event)}>
        <div className="auth-brand">
          <span className="brand-mark" aria-hidden="true">
            ◆
          </span>
          <h1>Migo</h1>
        </div>
        <p className="auth-sub">Create an account — your keys are made here and never leave it.</p>

        {/* On a wide screen the fields take the left column and the captcha the right, so a PC
            user reads the form and solves the challenge in one eye-width; on a phone the grid
            collapses and the captcha follows the fields as before. */}
        <div className="register-grid">
          <div className="register-fields">
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
              Password
              <input
                type="password"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                autoComplete="new-password"
                minLength={10}
                required
              />
              <span className="field-hint">At least 10 characters.</span>
            </label>
          </div>

          {endpoint !== null ? <CaptchaWidget endpoint={endpoint} onChange={setCaptcha} /> : null}
        </div>

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
        <BottomSheet title="Save your account" onClose={() => setSaveOfferOpen(false)}>
          <SaveAccountSheet
            username={username.trim()}
            accountId={accountId ?? ''}
            root={client?.keyStore.root()?.asBytes() ?? null}
            onDone={() => setSaveOfferOpen(false)}
          />
        </BottomSheet>
      ) : null}
    </main>
  );
}
