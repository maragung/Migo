'use client';

import { useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { Spinner } from '@/components/spinner.js';
import { useMigo } from '@/lib/migo/use-migo.js';

/** Sign in to an existing account, then hand off to the chat shell. */
export default function LoginPage(): ReactNode {
  const { status, error, login } = useMigo();
  const router = useRouter();

  const [identifier, setIdentifier] = useState('');
  const [password, setPassword] = useState('');
  const submitting = status === 'connecting';

  useEffect(() => {
    if (status === 'ready') {
      router.replace('/chat');
    }
  }, [status, router]);

  async function onSubmit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (submitting) {
      return;
    }
    try {
      await login({ identifier, password });
    } catch {
      // The provider surfaces the reason through `error`; keep the form populated for a retry.
    }
  }

  return (
    <main className="auth-screen">
      <form className="auth-card" onSubmit={(event) => void onSubmit(event)}>
        <div className="auth-brand">
          <span className="brand-mark">◆</span>
          <span className="brand-name">Migo</span>
        </div>
        <h1>Welcome back</h1>
        <p className="auth-sub">Sign in to your private, end-to-end encrypted account.</p>

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

        {error ? <p className="form-error">{error}</p> : null}

        <button type="submit" className="btn btn-primary btn-block" disabled={submitting}>
          {submitting ? <Spinner /> : 'Sign in'}
        </button>

        <p className="auth-alt">
          New to Migo? <Link href="/register">Create an account</Link>
        </p>
      </form>
    </main>
  );
}
