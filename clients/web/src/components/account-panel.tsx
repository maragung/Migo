'use client';

/**
 * The "My Account" panel: the account's own identity, its recovery email, its passphrase, and the
 * `.migo` key file — the four surfaces that are about *this account* rather than this device.
 *
 * Three of the four are things a person changes rarely and carefully, so each lives behind its own
 * deliberate control rather than an always-live field:
 *
 *   - **Identity** is read-only on purpose. The username is chosen once and can never change (it is
 *     part of the account's public name, §182), so the panel states that plainly rather than
 *     offering an edit that would only ever error. The account's public id (`MGO-XXXXXXXX`) is shown
 *     beside it because it is the shareable handle a person hands out.
 *   - **Email** cannot be read back from the server — there is no "what is my email" call, by
 *     design — so the panel never claims to show the current one. It offers a single field that
 *     records a new address, validated locally before it is sent so an obvious typo never reaches
 *     the wire.
 *   - **Passphrase** returns a fresh grant that the SDK has already installed on the live client; the
 *     panel's extra duty is to persist that grant (a reload must resume, not sign out) and to be
 *     honest that the `.migo` key file the person may have saved still opens with the *old*
 *     passphrase — so it offers a fresh file sealed with the new one, in the same breath.
 *   - **Key file** is the only way onto a new device, because no server holds the account root
 *     (§182). The download is offered only on a device that actually holds the root; a device that
 *     does not says so and offers no button that could not work.
 *
 * The presentational halves are exported as controlled components over plain data, so the rules
 * (the submit gates, the honest no-root state, the post-change key-file offer) are testable without
 * a live client — the same posture the settings and profile panels keep.
 */

import { useCallback, useState } from 'react';
import type { ReactNode } from 'react';

import { account } from '@migo/sdk';

import { containerFileName, credentialProblem, downloadAccountFile } from '@/lib/account-file.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { saveSession } from '@/lib/storage/session-store.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile } from '@/lib/migo/use-profiles.js';

import { BottomSheet } from './bottom-sheet.js';
import { Spinner } from './spinner.js';

/** The house-rule minimum for a new account passphrase, in characters. */
export const MIN_PASSPHRASE_LENGTH = 10;

/**
 * A light, local judgement of whether a string looks like an email address.
 *
 * This is not RFC validation and does not try to be: it exists to catch the obvious typo (a missing
 * `@`, a stray space, a domain with no dot) before a request is spent on it. The server is the
 * authority on whether an address is real; this only keeps a plainly-broken value off the wire.
 */
export function isLikelyEmail(value: string): boolean {
  const trimmed = value.trim();
  if (trimmed.length === 0 || trimmed.length > 254) {
    return false;
  }
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(trimmed);
}

/** Seals the account root into a `.migo` container under `credential`, ready to download. */
async function sealKeyFileBytes(
  rootBytes: Uint8Array,
  accountId: string,
  credential: string,
): Promise<Uint8Array> {
  const file = account.AccountFile.forRoot(
    account.MigoRoot.fromBytes(rootBytes),
    Math.floor(Date.now() / 1000),
  ).forAccount(accountId);
  return account.sealContainer(credential, file);
}

/** The account's read-only identity: its `@username`, its public id, and the immutability note. */
export function AccountIdentityView({
  username,
  publicId,
}: {
  /** The account's username, or `null` while the profile is still resolving. */
  username: string | null;
  /** The account's public id (`MGO-XXXXXXXX`), or `null` while the profile is still resolving. */
  publicId: string | null;
}): ReactNode {
  return (
    <>
      <div className="profile-id">
        <span className="person-name">{username !== null ? `@${username}` : 'Your account'}</span>
        {publicId !== null ? <span className="person-note">{publicId}</span> : null}
      </div>
      <p className="field-hint">Your username can never be changed.</p>
    </>
  );
}

/** The single email field as a controlled view; the panel owns the draft. */
export function EmailFormView({
  value,
  busy,
  error,
  saved,
  onChange,
  onSubmit,
}: {
  value: string;
  busy: boolean;
  error: string | null;
  saved: boolean;
  onChange: (value: string) => void;
  onSubmit: () => void;
}): ReactNode {
  const canSubmit = !busy && isLikelyEmail(value);
  return (
    <form
      className="password-form"
      onSubmit={(event) => {
        event.preventDefault();
        if (canSubmit) {
          onSubmit();
        }
      }}
    >
      <label className="field-label">
        Email address
        <input
          type="email"
          className="input"
          autoComplete="email"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder="you@example.com"
          aria-label="Email address"
        />
        <span className="field-hint">
          Used for account recovery. Your current email is never shown here.
        </span>
      </label>
      {error ? <p className="form-error">{error}</p> : null}
      {saved ? <p className="hint">Email saved.</p> : null}
      <button type="submit" className="btn btn-primary" disabled={!canSubmit}>
        {busy ? <Spinner /> : 'Save email'}
      </button>
    </form>
  );
}

/**
 * The passphrase form: current, new, and confirm, gated on a complete matching draft.
 *
 * After a successful change (`saved`) the form stops offering the submit and instead states the one
 * thing the person now needs to act on: the saved `.migo` file still opens with the *old*
 * passphrase. It offers a fresh file sealed with the new one — but only when this device holds the
 * root, because a device without it has no key file to reseal and must say so.
 */
export function PassphraseFormView({
  current,
  next,
  confirm,
  busy,
  error,
  saved,
  onChange,
  onSubmit,
  hasRoot,
  refreshSealing,
  refreshError,
  refreshSaved,
  onDownloadUpdated,
}: {
  current: string;
  next: string;
  confirm: string;
  busy: boolean;
  error: string | null;
  saved: boolean;
  onChange: (field: 'current' | 'next' | 'confirm', value: string) => void;
  onSubmit: () => void;
  /** Whether this device holds the account root, so a fresh key file can be sealed here. */
  hasRoot: boolean;
  /** True while the fresh key file is being sealed. */
  refreshSealing: boolean;
  refreshError: string | null;
  refreshSaved: boolean;
  onDownloadUpdated: () => void;
}): ReactNode {
  const canSubmit =
    current.length > 0 && next.length >= MIN_PASSPHRASE_LENGTH && next === confirm && !busy;
  return (
    <form
      className="password-form"
      onSubmit={(event) => {
        event.preventDefault();
        if (canSubmit && !saved) {
          onSubmit();
        }
      }}
    >
      <label className="field-label">
        Current passphrase
        <input
          type="password"
          className="input"
          autoComplete="current-password"
          value={current}
          onChange={(event) => onChange('current', event.target.value)}
          aria-label="Current passphrase"
        />
      </label>
      <label className="field-label">
        New passphrase
        <input
          type="password"
          className="input"
          autoComplete="new-password"
          value={next}
          onChange={(event) => onChange('next', event.target.value)}
          aria-label="New passphrase"
        />
        <span className="field-hint">At least {MIN_PASSPHRASE_LENGTH} characters.</span>
      </label>
      <label className="field-label">
        Confirm new passphrase
        <input
          type="password"
          className="input"
          autoComplete="new-password"
          value={confirm}
          onChange={(event) => onChange('confirm', event.target.value)}
          aria-label="Confirm new passphrase"
        />
      </label>
      {!saved ? (
        <>
          {error ? <p className="form-error">{error}</p> : null}
          <button type="submit" className="btn btn-primary" disabled={!canSubmit}>
            {busy ? <Spinner /> : 'Change passphrase'}
          </button>
        </>
      ) : (
        <section className="panel-section" aria-label="Update your key file">
          <p className="hint">Passphrase changed.</p>
          <p className="hint">
            Your account key file (.migo) still opens with your old passphrase. Download a fresh
            one, sealed with your new passphrase, to keep them in step.
          </p>
          {hasRoot ? (
            <>
              <button
                type="button"
                className="btn btn-primary"
                disabled={refreshSealing}
                onClick={onDownloadUpdated}
              >
                {refreshSealing ? <Spinner /> : 'Download updated key file'}
              </button>
              {refreshError ? <p className="form-error">{refreshError}</p> : null}
              {refreshSaved ? (
                <p className="hint">Updated key file downloaded — keep it somewhere safe.</p>
              ) : null}
            </>
          ) : (
            <p className="hint">
              This device does not hold the account root, so there is no key file to download.
            </p>
          )}
        </section>
      )}
    </form>
  );
}

/**
 * The key-file download form: a passphrase and its confirmation, judged locally before any Argon2id
 * work is spent, then the download control. Modelled on the registration save offer, but framed as
 * a replacement — re-downloading seals a new file under the passphrase typed here.
 */
export function KeyFileFormView({
  credential,
  confirm,
  sealing,
  error,
  saved,
  onChange,
  onSubmit,
}: {
  credential: string;
  confirm: string;
  sealing: boolean;
  error: string | null;
  saved: boolean;
  onChange: (field: 'credential' | 'confirm', value: string) => void;
  onSubmit: () => void;
}): ReactNode {
  const problem = credentialProblem(credential, confirm);
  const canSeal = !sealing && credential.length > 0 && problem === null;
  return (
    <div className="save-account">
      <p className="hint">
        Your account key file (.migo), with the passphrase you set here, is the only way to sign in
        on a new device — no server holds a copy of your keys.
      </p>
      <p className="hint">
        Re-downloading replaces your previous file — the passphrase you type here seals the new one.
      </p>
      <label className="field-label">
        Passphrase
        <input
          type="password"
          className="input"
          autoComplete="new-password"
          value={credential}
          onChange={(event) => onChange('credential', event.target.value)}
          aria-label="Key file passphrase"
        />
        <span className="field-hint">
          At least 8 characters. This unlocks the file — it need not be your Migo passphrase.
        </span>
      </label>
      <label className="field-label">
        Confirm passphrase
        <input
          type="password"
          className="input"
          autoComplete="new-password"
          value={confirm}
          onChange={(event) => onChange('confirm', event.target.value)}
          aria-label="Confirm key file passphrase"
        />
      </label>
      {credential.length > 0 && problem !== null ? <p className="form-error">{problem}</p> : null}
      {error !== null ? <p className="form-error">{error}</p> : null}
      {saved ? <p className="hint">Key file downloaded — keep it somewhere safe.</p> : null}
      <button type="button" className="btn btn-primary" disabled={!canSeal} onClick={onSubmit}>
        {sealing ? <Spinner /> : 'Download key file'}
      </button>
    </div>
  );
}

/**
 * The "My Account" panel: identity, email, passphrase, and the account key file.
 *
 * The panel owns every draft and every in-flight flag; the four exported views above are the
 * presentation, controlled entirely from here. The two flows that touch the key file — the
 * post-passphrase refresh and the standalone download — seal the same root bytes through the same
 * helper, differing only in which passphrase they seal under.
 */
export function AccountPanel(): ReactNode {
  const { client, accountId } = useMigo();
  const self = useProfile(accountId);

  // The root is present only on a founding device (one that registered or restored from the file).
  const root = client ? client.keyStore.root() : null;
  const hasRoot = root !== null;
  const fileName = containerFileName(self?.username ?? '');

  // --- email ---
  const [email, setEmail] = useState('');
  const [emailBusy, setEmailBusy] = useState(false);
  const [emailError, setEmailError] = useState<string | null>(null);
  const [emailSaved, setEmailSaved] = useState(false);

  // --- passphrase sheet ---
  const [passphraseOpen, setPassphraseOpen] = useState(false);
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [confirm, setConfirm] = useState('');
  const [changing, setChanging] = useState(false);
  const [passphraseError, setPassphraseError] = useState<string | null>(null);
  const [passphraseSaved, setPassphraseSaved] = useState(false);
  // The fresh key file offered after a successful change, sealed under the new passphrase.
  const [refreshSealing, setRefreshSealing] = useState(false);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [refreshSaved, setRefreshSaved] = useState(false);

  // --- key-file sheet ---
  const [keyFileOpen, setKeyFileOpen] = useState(false);
  const [credential, setCredential] = useState('');
  const [credentialConfirm, setCredentialConfirm] = useState('');
  const [sealing, setSealing] = useState(false);
  const [keyFileError, setKeyFileError] = useState<string | null>(null);
  const [keyFileSaved, setKeyFileSaved] = useState(false);

  const saveEmail = useCallback((): void => {
    if (!client || emailBusy) {
      return;
    }
    const value = email.trim();
    if (!isLikelyEmail(value)) {
      setEmailError('Enter a valid email address.');
      setEmailSaved(false);
      return;
    }
    setEmailBusy(true);
    setEmailError(null);
    setEmailSaved(false);
    // The SDK sends a single `email_or_phone` field on the wire; its typed API takes the address
    // under the `email` key and does that mapping itself.
    client
      .updateContact({ email: value })
      .then(() => {
        setEmailSaved(true);
      })
      .catch((cause: unknown) => {
        setEmailError(friendlyError(cause));
      })
      .finally(() => {
        setEmailBusy(false);
      });
  }, [client, email, emailBusy]);

  const onPassphraseField = (field: 'current' | 'next' | 'confirm', value: string): void => {
    setPassphraseSaved(false);
    setRefreshSaved(false);
    setRefreshError(null);
    if (field === 'current') {
      setCurrent(value);
    } else if (field === 'next') {
      setNext(value);
    } else {
      setConfirm(value);
    }
  };

  const changePassphrase = useCallback((): void => {
    if (!client || changing) {
      return;
    }
    setChanging(true);
    setPassphraseError(null);
    setPassphraseSaved(false);
    setRefreshError(null);
    setRefreshSaved(false);
    client
      .changePassword({ current_password: current, new_password: next })
      .then(async (grant) => {
        // The SDK installed the fresh tokens on the live client (every other session was revoked);
        // persist the replacement grant so a reload resumes this session rather than dropping to
        // the sign-in screen.
        await saveSession({ grant }).catch(() => {});
        setPassphraseSaved(true);
      })
      .catch((cause: unknown) => {
        setPassphraseError(friendlyError(cause));
      })
      .finally(() => {
        setChanging(false);
      });
  }, [client, changing, current, next]);

  // Seals a fresh key file under the *new* passphrase, so the saved file and the account agree.
  const downloadUpdatedKeyFile = useCallback((): void => {
    if (!client || refreshSealing || accountId === null) {
      return;
    }
    const live = client.keyStore.root();
    if (live === null) {
      return;
    }
    setRefreshSealing(true);
    setRefreshError(null);
    setRefreshSaved(false);
    void (async (): Promise<void> => {
      try {
        const bytes = await sealKeyFileBytes(live.asBytes(), String(accountId), next);
        downloadAccountFile(bytes, fileName);
        setRefreshSaved(true);
      } catch (cause) {
        setRefreshError(
          cause instanceof Error ? cause.message : 'The key file could not be sealed.',
        );
      } finally {
        setRefreshSealing(false);
      }
    })();
  }, [client, accountId, next, fileName, refreshSealing]);

  const onKeyFileField = (field: 'credential' | 'confirm', value: string): void => {
    setKeyFileSaved(false);
    setKeyFileError(null);
    if (field === 'credential') {
      setCredential(value);
    } else {
      setCredentialConfirm(value);
    }
  };

  // Seals a fresh key file under a passphrase typed just for the file (see KeyFileFormView).
  const downloadKeyFile = useCallback((): void => {
    if (!client || sealing || accountId === null) {
      return;
    }
    const live = client.keyStore.root();
    if (live === null) {
      return;
    }
    const problem = credentialProblem(credential, credentialConfirm);
    if (problem !== null) {
      setKeyFileError(problem);
      return;
    }
    setSealing(true);
    setKeyFileError(null);
    setKeyFileSaved(false);
    void (async (): Promise<void> => {
      try {
        const bytes = await sealKeyFileBytes(live.asBytes(), String(accountId), credential);
        downloadAccountFile(bytes, fileName);
        setKeyFileSaved(true);
      } catch (cause) {
        setKeyFileError(
          cause instanceof Error ? cause.message : 'The key file could not be sealed.',
        );
      } finally {
        setSealing(false);
      }
    })();
  }, [client, accountId, credential, credentialConfirm, fileName, sealing]);

  return (
    <div className="panel">
      <h1 className="panel-title">Account</h1>

      <section className="panel-section" aria-label="Account">
        <h2 className="panel-heading">Account</h2>
        <AccountIdentityView username={self?.username ?? null} publicId={self?.publicId ?? null} />
      </section>

      <section className="panel-section" aria-label="Email">
        <h2 className="panel-heading">Email</h2>
        <p className="hint">Add or change the email used for account recovery.</p>
        <EmailFormView
          value={email}
          busy={emailBusy}
          error={emailError}
          saved={emailSaved}
          onChange={(value) => {
            setEmail(value);
            setEmailError(null);
            setEmailSaved(false);
          }}
          onSubmit={saveEmail}
        />
      </section>

      <section className="panel-section" aria-label="Passphrase">
        <h2 className="panel-heading">Passphrase</h2>
        <p className="hint">Your account passphrase, changed in its own screen.</p>
        <button type="button" className="btn btn-primary" onClick={() => setPassphraseOpen(true)}>
          Change passphrase
        </button>
      </section>

      <section className="panel-section" aria-label="Account key file">
        <h2 className="panel-heading">Account key file (.migo)</h2>
        <p className="hint">
          This file, with its passphrase, is the only way to sign in on a new device — no server
          holds a copy of your keys.
        </p>
        {hasRoot ? (
          <button type="button" className="btn btn-primary" onClick={() => setKeyFileOpen(true)}>
            Download key file
          </button>
        ) : (
          <>
            <p className="hint">
              This device does not hold the account root, so there is no key file to download.
            </p>
            <button type="button" className="btn btn-primary" disabled>
              Download key file
            </button>
          </>
        )}
      </section>

      {passphraseOpen ? (
        <BottomSheet
          title="Change passphrase"
          onClose={() => {
            setPassphraseOpen(false);
            setCurrent('');
            setNext('');
            setConfirm('');
            setPassphraseError(null);
            setPassphraseSaved(false);
            setRefreshError(null);
            setRefreshSaved(false);
          }}
        >
          <PassphraseFormView
            current={current}
            next={next}
            confirm={confirm}
            busy={changing}
            error={passphraseError}
            saved={passphraseSaved}
            onChange={onPassphraseField}
            onSubmit={changePassphrase}
            hasRoot={hasRoot}
            refreshSealing={refreshSealing}
            refreshError={refreshError}
            refreshSaved={refreshSaved}
            onDownloadUpdated={downloadUpdatedKeyFile}
          />
        </BottomSheet>
      ) : null}

      {keyFileOpen ? (
        <BottomSheet
          title="Download key file"
          onClose={() => {
            setKeyFileOpen(false);
            setCredential('');
            setCredentialConfirm('');
            setKeyFileError(null);
            setKeyFileSaved(false);
          }}
        >
          <KeyFileFormView
            credential={credential}
            confirm={credentialConfirm}
            sealing={sealing}
            error={keyFileError}
            saved={keyFileSaved}
            onChange={onKeyFileField}
            onSubmit={downloadKeyFile}
          />
        </BottomSheet>
      ) : null}
    </div>
  );
}
