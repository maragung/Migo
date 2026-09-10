'use client';

/**
 * The "My Account" panel: the account's own identity, its recovery email, its passphrase, the
 * `.migo` key file, and the identity key's rotation — the five surfaces that are about *this
 * account* rather than this device.
 *
 * Four of the five are things a person changes rarely and carefully, so each lives behind its own
 * deliberate control rather than an always-live field:
 *
 *   - **Identity** is read-only on purpose. The username is chosen once and can never change (it is
 *     part of the account's public name, §182), so the panel states that plainly rather than
 *     offering an edit that would only ever error. The account's public id (`MGO-XXXXXXXXXXXX`) is shown
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
 *   - **Identity key rotation** (§2402) replaces only the ML-DSA signing identity — the key the login
 *     and add-device ceremonies verify against — and nothing else: the E2EE device identity, the
 *     ratchets, the safety numbers peers see, and this session are separate material the ceremony
 *     never touches. The confirmation states both halves honestly, the quiet half (nothing anyone
 *     has verified needs verifying again) and the costly one (the new key exists only here, so a
 *     backup made before the rotation is no longer enough on its own), because a rotation sold as a
 *     privacy control would be lying about what it does. A completed rotation does not stop at
 *     advice: the result's only way forward is the forced seal of a fresh key file, whose download
 *     is verified against the account's active key before it counts — and neither sheet can be
 *     dismissed until it has been.
 *
 * The presentational halves are exported as controlled components over plain data, so the rules
 * (the submit gates, the honest no-root state, the post-change key-file offer) are testable without
 * a live client — the same posture the settings and profile panels keep.
 */

import { useCallback, useState } from 'react';
import type { ReactNode } from 'react';

import { account } from '@migo/sdk';

import { containerFileName, credentialProblem, downloadAccountFile } from '@/lib/account-file.js';
import { rotateAccountIdentity } from '@/lib/migo/identity-rotation.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { saveSession } from '@/lib/storage/session-store.js';
import { recordBackupExport } from '@/lib/storage/backup-state-store.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile } from '@/lib/migo/use-profiles.js';

import { BottomSheet } from './bottom-sheet.js';
import { PassphraseInput } from './passphrase-input.js';
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

/** Lowercase hex for the seed bytes the container's `rotated_identity` field carries. */
function seedHex(seed: Uint8Array): string {
  let out = '';
  for (const byte of seed) {
    out += byte.toString(16).padStart(2, '0');
  }
  return out;
}

/**
 * Seals the account root into a `.migo` container under `credential`, ready to download.
 *
 * When this device holds a rotated identity seed, it rides the payload too: the successor is fresh
 * randomness that exists nowhere else, so a container without it restores a device whose add-device
 * signature the server will refuse. A device that holds no successor seals the plain root — the
 * format every backup made before a rotation already carries.
 */
async function sealKeyFileBytes(
  rootBytes: Uint8Array,
  accountId: string,
  credential: string,
  rotatedSeed: Uint8Array | null,
): Promise<Uint8Array> {
  const file = account.AccountFile.forRoot(
    account.MigoRoot.fromBytes(rootBytes),
    Math.floor(Date.now() / 1000),
  ).forAccount(accountId);
  return account.sealContainer(
    credential,
    rotatedSeed === null ? file : file.forRotatedIdentity(seedHex(rotatedSeed)),
  );
}

/**
 * Whether an opened container vouches for the identity key the account now answers with.
 *
 * The forced post-rotation seal's test: the sealed bytes are re-opened with the typed credential,
 * and the identity half they carry is compared byte-for-byte with the store's active key. An
 * absent half (a pre-rotation container), a half that will not even decode, or any mismatch
 * refuses — the flow never counts itself done on the strength of a download alone.
 */
export function sealedIdentityVouches(
  opened: account.AccountFile,
  activePublicKey: Uint8Array | null,
): boolean {
  try {
    if (activePublicKey === null) {
      return false;
    }
    const seed = opened.rotatedIdentitySeed();
    if (seed === null) {
      return false;
    }
    const sealed = account.IdentityKey.fromSeed(seed).publicKey();
    return (
      sealed.length === activePublicKey.length &&
      sealed.every((byte, index) => byte === activePublicKey[index])
    );
  } catch {
    // A half that will not decode cannot vouch for anything.
    return false;
  }
}

/** The account's read-only identity: its `@username`, its public id, and the immutability note. */
export function AccountIdentityView({
  username,
  publicId,
}: {
  /** The account's username, or `null` while the profile is still resolving. */
  username: string | null;
  /** The account's public id (`MGO-XXXXXXXXXXXX`), or `null` while the profile is still resolving. */
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
      className="passphrase-form"
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
      className="passphrase-form"
      onSubmit={(event) => {
        event.preventDefault();
        if (canSubmit && !saved) {
          onSubmit();
        }
      }}
    >
      <label className="field-label">
        Current passphrase
        <PassphraseInput
          className="input"
          value={current}
          onChange={(event) => onChange('current', event.target.value)}
          autoComplete="current-passphrase"
          ariaLabel="Current passphrase"
        />
      </label>
      <label className="field-label">
        New passphrase
        <PassphraseInput
          className="input"
          value={next}
          onChange={(event) => onChange('next', event.target.value)}
          autoComplete="new-passphrase"
          ariaLabel="New passphrase"
        />
        <span className="field-hint">At least {MIN_PASSPHRASE_LENGTH} characters.</span>
      </label>
      <label className="field-label">
        Confirm new passphrase
        <PassphraseInput
          className="input"
          value={confirm}
          onChange={(event) => onChange('confirm', event.target.value)}
          autoComplete="new-passphrase"
          ariaLabel="Confirm new passphrase"
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
 *
 * In forced mode — the seal a completed identity rotation owes — the download is only half the
 * ceremony: the sealed bytes are re-opened and their identity half verified before the file is
 * offered at all, so the plain "downloaded" hint never appears and the only success state is
 * `validated`, which is also the only state that offers a way out of the sheet.
 */
export function KeyFileFormView({
  credential,
  confirm,
  sealing,
  error,
  saved,
  forced = false,
  validated = false,
  onChange,
  onSubmit,
  onDone,
}: {
  credential: string;
  confirm: string;
  sealing: boolean;
  error: string | null;
  saved: boolean;
  /** Forced mode: the download is only half the ceremony — `validated` is the success state. */
  forced?: boolean;
  /** True once the sealed bytes were re-opened and vouched for the account's active identity key. */
  validated?: boolean;
  /** Forced mode's exit, offered only once validated. */
  onDone?: () => void;
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
      {forced ? (
        <p className="hint">{ROTATED_SEAL_EXPLANATION}</p>
      ) : (
        <p className="hint">
          Re-downloading replaces your previous file — the passphrase you type here seals the new
          one.
        </p>
      )}
      <label className="field-label">
        Passphrase
        <PassphraseInput
          className="input"
          value={credential}
          onChange={(event) => onChange('credential', event.target.value)}
          autoComplete="new-passphrase"
          ariaLabel="Key file passphrase"
        />
        <span className="field-hint">
          At least 8 characters. This unlocks the file — it need not be your Migo passphrase.
        </span>
      </label>
      <label className="field-label">
        Confirm passphrase
        <PassphraseInput
          className="input"
          value={confirm}
          onChange={(event) => onChange('confirm', event.target.value)}
          autoComplete="new-passphrase"
          ariaLabel="Confirm key file passphrase"
        />
      </label>
      {credential.length > 0 && problem !== null ? <p className="form-error">{problem}</p> : null}
      {error !== null ? <p className="form-error">{error}</p> : null}
      {validated ? (
        <>
          <p className="hint">{ROTATED_SEAL_VALIDATED}</p>
          {onDone !== undefined ? (
            <button type="button" className="btn btn-primary" onClick={onDone}>
              Done
            </button>
          ) : null}
        </>
      ) : (
        <>
          {!forced && saved ? (
            <p className="hint">Key file downloaded — keep it somewhere safe.</p>
          ) : null}
          <button type="button" className="btn btn-primary" disabled={!canSeal} onClick={onSubmit}>
            {sealing ? <Spinner /> : forced ? 'Download and verify key file' : 'Download key file'}
          </button>
        </>
      )}
    </div>
  );
}

/** The rotation section's one-sentence scope: what the identity key is, and is not. */
export const ROTATE_EXPLANATION =
  'The identity key signs this account in — it is not the key behind your conversations or the safety numbers your contacts see, which rotation does not touch.';

/** What a device without the root is told, mirroring the key-file section's honest no-root state. */
export const ROTATE_NO_ROOT =
  "Only a device that holds the account root can rotate the account's identity key. Seal or restore a backup on this device first.";

/** The confirmation's quiet half: what does not change, which is most of it. */
export const ROTATE_QUIET_HALF =
  "A new ML-DSA signing identity key is generated on this device and becomes the account's; the old one is retired on the server. Your sessions, conversations, encryption keys and safety numbers continue unchanged — nothing anyone has verified needs verifying again.";

/** The confirmation's costly half: where the new key lives, and what that costs an old backup. */
export const ROTATE_COSTLY_HALF =
  'The new key lives only on this device. Every other device that holds the account root — and any backup made before now — still carries the old key: a restore from an old backup will be refused until a fresh one is sealed here.';

/** The success notice, the desktop client's own sentence. */
export const ROTATED_NOTICE =
  'Identity key rotated; sessions, chats and safety numbers continue unchanged.';

/**
 * The fresh-backup advice a completed rotation owes, in the same breath as the notice.
 *
 * Now that the container can carry the successor's seed, the sentence states the real rule: a file
 * sealed after the rotation restores a device the server will accept; one sealed before does not.
 */
export const ROTATED_BACKUP_ADVICE =
  'Seal a fresh key file now: the new identity key lives only on this device, and only a file sealed after the rotation carries it — a backup made before restores the retired key and will be refused.';

/** Why the post-rotation seal is forced and verified, in the sheet that runs it. */
export const ROTATED_SEAL_EXPLANATION =
  'The file sealed here carries the new identity key — the download is verified against the key your account now answers with before it counts as done.';

/** The forced seal's success line: the sealed bytes were re-opened and vouched for the new key. */
export const ROTATED_SEAL_VALIDATED =
  'Key file downloaded and verified — it carries the new identity key and opens with the passphrase you set.';

/** The forced seal's refusal: the file's identity half does not vouch for the account's active key. */
export const ROTATED_SEAL_MISMATCH =
  'The sealed file does not carry the identity key this account answers with, so it was not downloaded. Nothing was faked.';

/**
 * The honest state when the forced seal has nothing to seal: this browser holds no successor seed,
 * so no file sealed here could carry the new key. The rotation cannot be undone into a success.
 */
export const ROTATED_SEAL_NO_SEED =
  'This browser no longer holds the new identity key, so no key file sealed here can carry it. Nothing was downloaded — reload this page and try again.';

/** How a rotation attempt ended, for the view that reports it. */
export interface RotationResult {
  kind: 'done' | 'unfinished';
  message: string;
}

/**
 * The rotation sheet's content: the confirmation, the in-flight state, and every way the attempt
 * can end.
 *
 * The confirmation states both halves before the button is pressed — the quiet half and the costly
 * one — because the pane says what the button *is* and the sheet says what it *does*, and nobody
 * should reach the button without both. The `unfinished` end (the server's answer was lost, the new
 * key is sealed here) is reported as its own state rather than an error, which is what it is: the
 * next attempt settles it either way. The `done` end does not stop at advice: its only way forward
 * is the forced seal of a fresh key file, and there is no plain Done to skip past it with — the
 * successor exists only on this device, and the panel's whole duty is to see it onto a file that
 * has been verified to carry it.
 */
export function RotateIdentityView({
  busy,
  result,
  error,
  onConfirm,
  onSealKeyFile,
  onClose,
}: {
  busy: boolean;
  result: RotationResult | null;
  error: string | null;
  onConfirm: () => void;
  /** Opens the key-file sheet the completed rotation owes — forced, and verified before it counts. */
  onSealKeyFile: () => void;
  onClose: () => void;
}): ReactNode {
  if (result !== null) {
    return (
      <div className="rotate-result">
        <p className="hint">{result.message}</p>
        {result.kind === 'done' ? (
          <>
            <p className="hint">{ROTATED_BACKUP_ADVICE}</p>
            <button type="button" className="btn btn-primary" onClick={onSealKeyFile}>
              Seal new key file
            </button>
          </>
        ) : (
          <button type="button" className="btn btn-primary" onClick={onClose}>
            Done
          </button>
        )}
      </div>
    );
  }
  return (
    <div className="rotate-form">
      <p className="hint">{ROTATE_QUIET_HALF}</p>
      <p className="hint">{ROTATE_COSTLY_HALF}</p>
      {error !== null ? <p className="form-error">{error}</p> : null}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" disabled={busy} onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="btn btn-danger"
          disabled={busy}
          onClick={() => {
            if (!busy) {
              onConfirm();
            }
          }}
        >
          {busy ? <Spinner /> : 'Rotate identity key'}
        </button>
      </div>
    </div>
  );
}

/**
 * The "My Account" panel: identity, email, passphrase, the account key file, and the identity key.
 *
 * The panel owns every draft and every in-flight flag; the four exported views above are the
 * presentation, controlled entirely from here. The three flows that touch the key file — the
 * post-passphrase refresh, the standalone download, and the forced post-rotation seal — seal the
 * same root bytes through the same helper, differing only in which passphrase they seal under;
 * all three embed the rotated identity seed whenever this device holds one, so a fresh file
 * always carries the key the account actually answers with.
 */
export function AccountPanel(): ReactNode {
  const { client, accountId } = useMigo();
  const self = useProfile(accountId);

  // The root is present only on a founding device (one that registered or restored from the file).
  const root = client ? client.keyStore.root() : null;
  const hasRoot = root !== null;
  const fileName = containerFileName(self?.username ?? '');
  // The successor seed, held from a rotation's pre-commit on this browser; what a fresh seal embeds.
  const rotatedSeedHeld = client ? client.keyStore.rotatedIdentitySeed() : null;

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
  // Forced mode — the seal a completed rotation owes. The sheet cannot be dismissed until the
  // download has been verified, and the standalone flow's plain "downloaded" hint never applies.
  const [keyFileForced, setKeyFileForced] = useState(false);
  const [forcedValidated, setForcedValidated] = useState(false);
  const [credential, setCredential] = useState('');
  const [credentialConfirm, setCredentialConfirm] = useState('');
  const [sealing, setSealing] = useState(false);
  const [keyFileError, setKeyFileError] = useState<string | null>(null);
  const [keyFileSaved, setKeyFileSaved] = useState(false);

  // --- identity-key rotation sheet ---
  const [rotateOpen, setRotateOpen] = useState(false);
  const [rotating, setRotating] = useState(false);
  const [rotateResult, setRotateResult] = useState<RotationResult | null>(null);
  const [rotateError, setRotateError] = useState<string | null>(null);

  /**
   * Runs the rotation. The ordering — pre-commit, ceremony, heal, rollback — is
   * `rotateAccountIdentity`'s; the panel's own duty is only to report each end honestly: the
   * notice and its fresh-backup advice, the unfinished state's "rotate again to finish", or the
   * refusal's sentence.
   */
  const rotateIdentity = useCallback((): void => {
    if (!client || rotating || accountId === null) {
      return;
    }
    setRotating(true);
    setRotateError(null);
    setRotateResult(null);
    void (async (): Promise<void> => {
      try {
        const outcome = await rotateAccountIdentity(client, accountId);
        setRotateResult(
          outcome.state === 'done'
            ? { kind: 'done', message: ROTATED_NOTICE }
            : { kind: 'unfinished', message: outcome.message },
        );
      } catch (cause) {
        setRotateError(cause instanceof Error ? cause.message : friendlyError(cause));
      } finally {
        setRotating(false);
      }
    })();
  }, [client, accountId, rotating]);

  /**
   * Closes the rotation sheet — except after a completed rotation, which is not dismissable until
   * its key file is sealed and verified. The successor exists only on this device, and the one act
   * the result asks for cannot be skipped past with the sheet's own close control.
   */
  const closeRotateSheet = useCallback((): void => {
    if (rotateResult !== null && rotateResult.kind === 'done' && !forcedValidated) {
      return;
    }
    setRotateOpen(false);
    setRotateResult(null);
    setRotateError(null);
  }, [rotateResult, forcedValidated]);

  /** The done result's only button: hand the sheet over to the forced, verified seal. */
  const openForcedSeal = useCallback((): void => {
    setRotateOpen(false);
    setKeyFileForced(true);
    setForcedValidated(false);
    setCredential('');
    setCredentialConfirm('');
    setKeyFileError(null);
    setKeyFileSaved(false);
    setKeyFileOpen(true);
  }, []);

  /**
   * Closes the key-file sheet — except in forced mode before validation, where there is no way out.
   * A browser that holds no successor seed is the one exception: it has nothing to seal, and
   * locking an honest dead end behind a sheet would be forcing for its own sake.
   */
  const closeKeyFileSheet = useCallback((): void => {
    if (keyFileForced && !forcedValidated && rotatedSeedHeld !== null) {
      return;
    }
    setKeyFileOpen(false);
    setKeyFileForced(false);
    setForcedValidated(false);
    setCredential('');
    setCredentialConfirm('');
    setKeyFileError(null);
    setKeyFileSaved(false);
  }, [keyFileForced, forcedValidated, rotatedSeedHeld]);

  /** The validated forced seal's Done: the whole rotation ceremony finally closes. */
  const finishForcedSeal = useCallback((): void => {
    setKeyFileOpen(false);
    setKeyFileForced(false);
    setForcedValidated(false);
    setCredential('');
    setCredentialConfirm('');
    setKeyFileError(null);
    setKeyFileSaved(false);
    setRotateResult(null);
    setRotateError(null);
  }, []);

  /** The panel's own download control: the standalone seal, never the forced one. */
  const openStandaloneKeyFile = useCallback((): void => {
    setKeyFileForced(false);
    setForcedValidated(false);
    setKeyFileOpen(true);
  }, []);

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
      .changePassphrase({ current_passphrase: current, new_passphrase: next })
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
        const bytes = await sealKeyFileBytes(
          live.asBytes(),
          String(accountId),
          next,
          client.keyStore.rotatedIdentitySeed(),
        );
        downloadAccountFile(bytes, fileName);
        // The export is what the checkup's Backup row vouches for, so the download is the moment
        // the record is written — best-effort, because a failed bookkeeping write must not turn a
        // completed download into a reported failure.
        await recordBackupExport(accountId).catch(() => {});
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
        const bytes = await sealKeyFileBytes(
          live.asBytes(),
          String(accountId),
          credential,
          client.keyStore.rotatedIdentitySeed(),
        );
        if (keyFileForced) {
          // The download is only half the ceremony: the sealed bytes are re-opened with the typed
          // credential and their identity half compared against the key this account now answers
          // with. A refusal means no file is offered at all — the forced seal never counts a
          // download it cannot vouch for, and the sheet stays open.
          const opened = await account.openContainer(credential, bytes);
          if (
            !sealedIdentityVouches(
              opened,
              client.keyStore.accountIdentityKey()?.publicKey() ?? null,
            )
          ) {
            throw new Error(ROTATED_SEAL_MISMATCH);
          }
        } else {
          setKeyFileSaved(true);
        }
        downloadAccountFile(bytes, fileName);
        if (keyFileForced) {
          // Only now, with the bytes on disk, is the ceremony's success state
          // entered — a download control that threw must not leave the sheet
          // claiming a file it never handed over.
          setForcedValidated(true);
        }
        // Same rule as the post-passphrase re-seal: the export is the event the Backup row records.
        await recordBackupExport(accountId).catch(() => {});
      } catch (cause) {
        if (keyFileForced && cause instanceof account.AccountError) {
          // A container that will not re-open with the credential it was just sealed under cannot
          // vouch for anything; the refusal gets the honest sentence, not a raw variant name.
          setKeyFileError(ROTATED_SEAL_MISMATCH);
        } else {
          setKeyFileError(
            cause instanceof Error ? cause.message : 'The key file could not be sealed.',
          );
        }
      } finally {
        setSealing(false);
      }
    })();
  }, [client, accountId, credential, credentialConfirm, fileName, sealing, keyFileForced]);

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
          <button type="button" className="btn btn-primary" onClick={openStandaloneKeyFile}>
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

      <section className="panel-section" aria-label="Identity key">
        <h2 className="panel-heading">Identity key</h2>
        <p className="hint">{ROTATE_EXPLANATION}</p>
        {hasRoot ? (
          <button type="button" className="btn btn-danger" onClick={() => setRotateOpen(true)}>
            Rotate identity key
          </button>
        ) : (
          <p className="hint">{ROTATE_NO_ROOT}</p>
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
          title={keyFileForced ? 'Seal new key file' : 'Download key file'}
          onClose={closeKeyFileSheet}
        >
          {keyFileForced && rotatedSeedHeld === null ? (
            // The honest dead end: a browser with no successor seed cannot seal what the rotation
            // owes, and the forced flow says so rather than faking a validated download.
            <div className="save-account">
              <p className="hint">{ROTATED_SEAL_NO_SEED}</p>
            </div>
          ) : (
            <KeyFileFormView
              credential={credential}
              confirm={credentialConfirm}
              sealing={sealing}
              error={keyFileError}
              saved={keyFileSaved}
              forced={keyFileForced}
              validated={forcedValidated}
              onChange={onKeyFileField}
              onSubmit={downloadKeyFile}
              onDone={keyFileForced ? finishForcedSeal : undefined}
            />
          )}
        </BottomSheet>
      ) : null}

      {rotateOpen ? (
        <BottomSheet title="Rotate identity key" onClose={closeRotateSheet}>
          <RotateIdentityView
            busy={rotating}
            result={rotateResult}
            error={rotateError}
            onConfirm={rotateIdentity}
            onSealKeyFile={openForcedSeal}
            onClose={closeRotateSheet}
          />
        </BottomSheet>
      ) : null}
    </div>
  );
}
