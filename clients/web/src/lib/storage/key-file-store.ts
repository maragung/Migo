/**
 * Persistence for the `.migo` key files this browser has imported: the sealed containers,
 * ciphertext and nothing else.
 *
 * What is stored is the file exactly as it came off the disk — Argon2id salt and XChaCha20
 * nonce in the clear header, the account root sealed under the passphrase in the body. The
 * passphrase itself is never written here, so a browser that remembers an account still
 * needs the human who owns it at every sign-in: the row spares them the file picker, not the
 * secret. This is the same bargain the container already makes with a cloud drive — the file
 * is safe to store precisely because it holds only ciphertext.
 *
 * A row's identity is the container's salt, read from the clear header without any Argon2id
 * work. Re-importing the same file updates its row rather than duplicating it, and a
 * re-sealed copy of the same account (fresh salt, fresh nonce) is a new row beside the old —
 * both still open the same account, and the newer `savedAt` sorts first.
 *
 * The username and account id arrive only after a successful open, so an imported-but-never-
 * opened file is known by its file name alone and the row is honest about that: the fields
 * stay empty until a sign-in fills them in.
 */

import type { Id } from '@migo/sdk';

import { idbGet, idbSet } from './idb.js';

const KEY = 'key-files';

/** Where the Argon2id salt sits in a container header — see the header layout in `@migo/crypto`. */
const SALT_OFFSET = 9 + 2 + 2 + 1 + 4 + 4 + 4;
/** Salt width in bytes. */
const SALT_LEN = 16;

/** One key file this browser remembers. The bytes are the sealed container, verbatim. */
export interface SavedKeyFile {
  /** The container's salt as hex — the row's identity. Same file, same id. */
  id: string;
  /** The sealed `.migo` container bytes, ciphertext and header exactly as imported. */
  bytes: Uint8Array;
  /** The file's name as chosen on disk (`migo-alice.migo`), shown until a username is learned. */
  fileName: string;
  /** The account's username, learned at the first successful open; empty until then. */
  username: string;
  /** The server-side account id, learned with the username; `null` until then. */
  accountId: Id | null;
  /** When this row was written, Unix milliseconds. Display material, not security material. */
  savedAt: number;
}

/** Reads a container's salt as hex — its identity — without any key-derivation work. */
export function keyFileId(bytes: Uint8Array): string {
  if (bytes.length < SALT_OFFSET + SALT_LEN) {
    throw new Error('not a .migo container: too short for a header');
  }
  return Array.from(bytes.subarray(SALT_OFFSET, SALT_OFFSET + SALT_LEN))
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');
}

/** Every key file this browser remembers, newest first. */
export async function loadKeyFiles(): Promise<SavedKeyFile[]> {
  const rows = await idbGet<SavedKeyFile[]>(KEY);
  return rows === undefined ? [] : [...rows].sort((a, b) => b.savedAt - a.savedAt);
}

/**
 * Writes one row, upserting on the salt id: importing a file twice, or signing in with a
 * stored file that finally learns its username, replaces the row rather than adding one.
 */
export async function saveKeyFile(file: SavedKeyFile): Promise<void> {
  const rows = await loadKeyFiles();
  const next = rows.filter((row) => row.id !== file.id);
  next.unshift(file);
  await idbSet(KEY, next);
}

/** Forgets one key file; a no-op when it was never stored. */
export async function removeKeyFile(id: string): Promise<void> {
  const rows = await loadKeyFiles();
  await idbSet(
    KEY,
    rows.filter((row) => row.id !== id),
  );
}
