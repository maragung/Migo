'use client';

import { useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { CaptchaWidget } from '@/components/captcha-widget.js';
import { ServerForm } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { ThemeToggle } from '@/components/theme-toggle.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { defaultServerEndpoint } from '@/lib/config.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';

import type { CaptchaProof, ServerEndpoint } from '@migo/sdk';

/** Create a new account. Identity keys are generated on this device and never leave it. */
export default function RegisterPage(): ReactNode {
  const { status, error, register } = useMigo();
  const router = useRouter();

  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [email, setEmail] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
  const [captcha, setCaptcha] = useState<CaptchaProof | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);
  const submitting = status === 'connecting';

  useEffect(() => {
    if (status === 'ready') {
      router.replace('/chat');
    }
  }, [status, router]);

  useEffect(() => {
    let cancelled = false;
    void loadServerEndpoint().then((stored) => {
      if (cancelled) return;
      // A fresh visitor (no stored endpoint) still gets a working form
      // against the build's default host. The Server disclosure stays
      // collapsed, the captcha widget mounts, and the submit button is
      // enabled; the user can expand the disclosure to point at a
      // self-hosted server without ever leaving the page in a disabled
      // state.
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
    } catch {
      // The provider surfaces the reason through `error`; keep the form populated for a retry.
    }
  }

  function onServerDisclosureCommit(next: ServerEndpoint): void {
    setEndpoint(next);
    setValidationError(null);
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
        <p className="auth-sub">
          Create an account. Your encryption keys are generated on this device and never leave it.
        </p>

        {endpointReady && endpoint !== null ? (
          <ServerForm value={endpoint} onCommit={onServerDisclosureCommit} />
        ) : null}

        <label className="field-label">
          Username
          <input
            type="text"
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            autoComplete="username"
            autoFocus
            required
          />
        </label>

        <label className="field-label">
          Password
          <input
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="new-password"
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
          />
        </label>

        {endpoint !== null ? <CaptchaWidget endpoint={endpoint} onChange={setCaptcha} /> : null}

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
      </form>
    </main>
  );
}
