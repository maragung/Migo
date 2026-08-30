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
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';

import type { CaptchaProof, ServerEndpoint } from '@migo/sdk';

/** Sign in to an existing account, then hand off to the chat shell. */
export default function LoginPage(): ReactNode {
  const { status, error, login } = useMigo();
  const router = useRouter();

  const [identifier, setIdentifier] = useState('');
  const [password, setPassword] = useState('');
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

  // Pre-fill the server field from the persisted endpoint. Without this snapshot a fresh visit
  // sees the build's default, which is the env-supplied URL on production builds and a loopback on
  // dev builds.
  useEffect(() => {
    let cancelled = false;
    void loadServerEndpoint().then((stored) => {
      if (cancelled) return;
      setEndpoint(stored ?? null);
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
    // Persist the chosen endpoint *before* the bootstrap call, so a mid-flight failure can be retried
    // against the same server without the form losing the address the user just confirmed.
    try {
      await saveServerEndpoint(endpoint);
    } catch {
      // Best-effort: a failed local write is not a reason to refuse the in-flight attempt.
    }
    setValidationError(null);
    try {
      await login({ identifier, password }, endpoint, captcha);
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
        <p className="auth-sub">Sign in to your private, end-to-end encrypted account.</p>

        {endpointReady && endpoint !== null ? (
          <ServerForm value={endpoint} onCommit={onServerDisclosureCommit} />
        ) : null}

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

        <label className="field-label">
          Password
          <input
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="current-password"
            required
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
          {submitting ? <Spinner /> : 'Sign in'}
        </button>

        <p className="auth-alt">
          New to Migo? <Link href="/register">Create an account</Link>
        </p>
      </form>
    </main>
  );
}
