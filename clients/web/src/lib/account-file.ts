/**
 * The account-file side of the auth screens: naming the container, judging a recovery credential,
 * and handing sealed bytes to the browser as a download.
 *
 * The container itself is {@link account.sealContainer} / {@link account.openContainer} in
 * `@migo/crypto` — Argon2id plus XChaCha20-Poly1305, one honest error for a wrong credential and
 * a tampered file alike (§182). What lives here is everything around it that a screen needs: the
 * file name a person will look for in their drive, the one-line judgement of a typed credential
 * before any expensive hashing runs, and the browser plumbing that turns bytes into a download.
 */

/** Lowercase alphanumerics and hyphens are what a filename can carry without surprise. */
const SAFE_NAME = /[^a-z0-9-]+/g;

/**
 * The name a saved account file downloads as: `migo-<username>.migo`.
 *
 * The username is lowercased and everything else becomes a hyphen, so "Íñigo M!" cannot produce a
 * path separator or an unclickable filename; a username that sanitises to nothing still gets a
 * name, because a file called `.migo` is invisible in most file managers.
 */
export function containerFileName(username: string): string {
  const sanitized = username
    .toLowerCase()
    .replace(SAFE_NAME, '-')
    .replace(/^-+|-+$/g, '');
  return `migo-${sanitized === '' ? 'account' : sanitized}.migo`;
}

/** The shortest recovery credential the container accepts, in bytes. */
export const MIN_CREDENTIAL_BYTES = 8;
/** The longest recovery credential the container accepts, in bytes. */
export const MAX_CREDENTIAL_BYTES = 1024;

/**
 * Judges a typed recovery credential pair before any Argon2id work is spent on it.
 *
 * Returns `null` when the pair is sealable, or the one-line problem to show otherwise. Length is
 * the only composition rule — the credential is not an account password, and pattern rules push
 * people towards dictionary words rather than away from them.
 */
export function credentialProblem(credential: string, confirm: string): string | null {
  const length = new TextEncoder().encode(credential).length;
  if (length < MIN_CREDENTIAL_BYTES) {
    return `The recovery credential needs at least ${MIN_CREDENTIAL_BYTES} characters.`;
  }
  if (length > MAX_CREDENTIAL_BYTES) {
    return 'The recovery credential is too long to use.';
  }
  if (credential !== confirm) {
    return 'The two credentials do not match.';
  }
  return null;
}

/** The honest line for every way an open can fail: §182 forbids naming which one it was. */
export const RESTORE_FAILED =
  'That credential does not open this file, or the file is not an account file.';

/**
 * Offers sealed container bytes to the browser as a download.
 *
 * A no-op outside a browser (the test renderer has no `document`), and best-effort inside one:
 * a refused download is not a reason to treat the save flow as failed, because the bytes can be
 * re-sealed and re-offered at any time while the sheet is open.
 */
export function downloadAccountFile(bytes: Uint8Array, filename: string): void {
  if (typeof document === 'undefined') {
    return;
  }
  // `slice()` re-backs the bytes on a fresh ArrayBuffer, which is what the DOM's BlobPart asks
  // for under TS 5.7's typed-array generics; the copy is a few hundred bytes, once per download.
  const blob = new Blob([bytes.slice()], { type: 'application/octet-stream' });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}
