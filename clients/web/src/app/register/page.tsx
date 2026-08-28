'use client';

import { useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { ServerForm } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';

import type { ServerEndpoint } from '@migo/sdk';

/** Create a new account. Identity keys are generated on this device and never leave it. */
export default function RegisterPage(): ReactNode {
  const { status, error, register } = useMigo();
  const router = useRouter();

  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [email, setEmail] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
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
    try {
      await saveServerEndpoint(endpoint);
    } catch {
      // Best-effort, see login.
    }
    setValidationError(null);
    try {
      await register({ username, password, email: email || undefined }, endpoint);
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
      <form className="auth-card" onSubmit={(event) => void onSubmit(event)}>
        <div className="auth-brand">
          <span className="brand-mark">◆</span>
          <span className="brand-name">Migo</span>
        </div>
        <h1>Create your account</h1>
        <p className="auth-sub">
          Your encryption keys are generated on this device and never leave it.
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
