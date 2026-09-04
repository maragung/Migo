'use client';

import { useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import { BottomSheet } from '@/components/bottom-sheet.js';
import { CaptchaWidget } from '@/components/captcha-widget.js';
import { PassphraseInput } from '@/components/passphrase-input.js';
import { SaveAccountSheet } from '@/components/save-account-sheet.js';
import { ServerForm, transportLabel } from '@/components/server-form.js';
import { Spinner } from '@/components/spinner.js';
import { ThemeToggle } from '@/components/theme-toggle.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { defaultServerEndpoint } from '@/lib/config.js';
import { loadServerEndpoint, saveServerEndpoint } from '@/lib/storage/server-endpoint-store.js';
import { loadKeyFiles } from '@/lib/storage/key-file-store.js';

import type { CaptchaChallenge, CaptchaProof, ServerEndpoint } from '@migo/sdk';
import { RemoteError } from '@migo/sdk';

/** The gender options the profile accepts, in the server's numbering (1 male, 2 female, 3 other). */
const GENDERS = [
  { value: 1, label: 'Male' },
  { value: 2, label: 'Female' },
  { value: 3, label: 'Other' },
];

/**
 * Create a new account. Identity keys are generated on this device and never leave it.
 *
 * The one passphrase this form collects is the account's whole secret surface: it is the passphrase
 * the server verifies at the founding registration, and it is the credential that seals the `.migo`
 * key file offered right after — which is why the file download needs no second passphrase to be
 * typed and why the sign-in screen asks for the file and this passphrase and nothing else.
 *
 * A successful registration ends at the sign-in door, not inside the app: the key-file offer is
 * honoured, the session registration opened is closed, and the user signs back in with the file
 * and the passphrase themselves — the same steps every later visit takes, rehearsed once while
 * the passphrase is still fresh.
 */
export default function RegisterPage(): ReactNode {
  const { status, error, register, client, accountId, logout } = useMigo();
  const router = useRouter();

  const [username, setUsername] = useState('');
  const [passphrase, setPassphrase] = useState('');
  const [email, setEmail] = useState('');
  const [gender, setGender] = useState('');
  const [endpoint, setEndpoint] = useState<ServerEndpoint | null>(null);
  const [endpointReady, setEndpointReady] = useState(false);
  const [serverSheetOpen, setServerSheetOpen] = useState(false);
  const [saveOfferOpen, setSaveOfferOpen] = useState(false);
  const [captcha, setCaptcha] = useState<CaptchaProof | null>(null);
  // The replacement challenge the server attached to the last refused submit, when it
  // attached one. A submitted proof is spent whatever the verdict, so this is the live
  // challenge the next attempt must answer — the widget swaps to it without a round trip.
  const [freshCaptcha, setFreshCaptcha] = useState<CaptchaChallenge | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);
  const submitting = status === 'connecting';

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
          passphrase,
          email: email || undefined,
          gender: gender === '' ? undefined : Number(gender),
        },
        endpoint,
        captcha,
      );
      // The account exists and this browser holds its founding root; offer the one-time key-file
      // download before the redirect carries the user away.
      setSaveOfferOpen(true);
    } catch (cause) {
      // The provider surfaces the reason through `error`; keep the form populated for a
      // retry. A refusal that carries the replacement captcha swaps the widget's picture
      // on the spot — the proof this attempt spent is gone either way, and the user's
      // next step should be reading the new challenge, not finding the refresh control.
      if (cause instanceof RemoteError && cause.captcha !== undefined) {
        setFreshCaptcha(cause.captcha);
      }
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

  /**
   * Ends a successful registration: back to the sign-in page, with the session registration
   * opened closed behind the user, so the account is entered the way it will be entered every
   * later visit — the key file and the passphrase, typed by its owner.
   *
   * The sign-out is only safe once the sealed container is remembered somewhere: logging out also
   * clears this browser's key-store snapshot, and the key file is then the account's only key.
   * The key-file *store* is what gets checked, not the sheet's state, so the corner X is judged
   * by the same rule as the button — and a registration whose file never sealed (or never got
   * saved) keeps its session and lands in the app instead of at a door that cannot open.
   */
  async function finishRegistration(): Promise<void> {
    setSaveOfferOpen(false);
    let hasFile = false;
    try {
      const rows = await loadKeyFiles();
      hasFile = rows.some((row) => row.accountId === accountId);
    } catch {
      hasFile = false;
    }
    if (!hasFile) {
      router.replace('/chat');
      return;
    }
    await logout();
    router.replace('/login');
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
          <PassphraseInput
            value={passphrase}
            onChange={(event) => setPassphrase(event.target.value)}
            autoComplete="new-passphrase"
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

        {/* The captcha challenge, asked inline where the fields are. It renders as glass on
            the card — no white panel — and only its proof (or a null while the user has not
            answered) rides along to the register call. The widget fetches from the endpoint
            the form is about to authenticate against, so it appears once the endpoint is. */}
        {endpoint !== null ? (
          <CaptchaWidget endpoint={endpoint} onChange={setCaptcha} replacement={freshCaptcha} />
        ) : null}

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
        <BottomSheet
          title="Your account key file"
          variant="auth"
          onClose={() => void finishRegistration()}
        >
          <SaveAccountSheet
            username={username.trim()}
            accountId={accountId ?? ''}
            root={client?.keyStore.root()?.asBytes() ?? null}
            passphrase={passphrase}
            onDone={() => void finishRegistration()}
          />
        </BottomSheet>
      ) : null}
    </main>
  );
}
