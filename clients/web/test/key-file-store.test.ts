/**
 * The key-file store: what the sign-in screen's account list is built on.
 *
 * The store's whole promise is one sentence — the browser remembers the sealed `.migo` file and
 * nothing that could open it. These tests pin the four invariants that sentence rests on:
 *
 *   1. a row's identity is the container's salt, read from the clear header with no Argon2id
 *      work — the same file is the same row however often it is imported, and a different salt
 *      is a different row even for the same account;
 *   2. saving upserts on that identity, so a row that finally learns its username at the first
 *      successful sign-in is updated rather than duplicated;
 *   3. the bytes round-trip verbatim — what the ceremony sealed is what the ceremony gets back;
 *   4. the list is newest first and forgetting a row leaves its siblings alone.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import type { Id } from '@migo/sdk';

import {
  keyFileId,
  loadKeyFiles,
  removeKeyFile,
  saveKeyFile,
} from '../src/lib/storage/key-file-store.js';
import type { SavedKeyFile } from '../src/lib/storage/key-file-store.js';
import { installFakeIndexedDb } from './support/dom-stubs.js';

/** Where the salt sits in a container header — mirrored here so the test can *build* one. */
const SALT_OFFSET = 26;

/**
 * A container-shaped byte array with the given salt. The store never opens the body, so only
 * the header's geometry has to be honest; the body is filler.
 */
function containerWith(salt: number[]): Uint8Array {
  const bytes = new Uint8Array(80);
  for (const [index, value] of salt.entries()) {
    bytes[SALT_OFFSET + index] = value;
  }
  return bytes;
}

function row(
  id: string,
  bytes: Uint8Array,
  fileName: string,
  username: string,
  savedAt: number,
): SavedKeyFile {
  return {
    id,
    bytes,
    fileName,
    username,
    accountId: username === '' ? null : ('01ARZ3NDEKTSV4RRFFQ69G5FAV' as Id),
    savedAt,
  };
}

test('a container is identified by its salt, and only by its salt', () => {
  const salt = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
  const sameSaltDifferentBytes = containerWith(salt);
  sameSaltDifferentBytes[70] = 0xff;
  assert.equal(
    keyFileId(containerWith(salt)),
    keyFileId(sameSaltDifferentBytes),
    'the body is not part of the identity — a re-read of the same file is the same row',
  );
  assert.notEqual(
    keyFileId(containerWith(salt)),
    keyFileId(containerWith([...salt.slice(0, 15), 17])),
    'a different salt (a re-sealed copy of the account) is a different row',
  );
  assert.throws(
    () => keyFileId(new Uint8Array(10)),
    'a file too short for a header is refused before any salt is read',
  );
});

test('saving upserts on the identity: the second import updates, it does not duplicate', async () => {
  const fake = installFakeIndexedDb();
  try {
    const bytes = containerWith([7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7]);
    const id = keyFileId(bytes);

    // The first import knows nothing but the file name; the sign-in that follows fills in who.
    await saveKeyFile(row(id, bytes, 'migo-alice.migo', '', 1_000));
    await saveKeyFile(row(id, bytes, 'migo-alice.migo', 'alice', 2_000));

    const files = await loadKeyFiles();
    const [first] = files;
    assert.ok(first !== undefined, 'the upserted row must read back');
    assert.equal(files.length, 1, 'an upsert, not a second row');
    assert.equal(first.username, 'alice', 'the learned username lands on the row');
    assert.equal(first.savedAt, 2_000, 'the row carries the newest write');
    assert.deepEqual(
      new Uint8Array(first.bytes),
      bytes,
      'the sealed bytes round-trip verbatim — ciphertext in, ciphertext out',
    );
  } finally {
    fake.restore();
  }
});

test('the list is newest first, and forgetting one row spares the rest', async () => {
  const fake = installFakeIndexedDb();
  try {
    const older = containerWith([1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    const newer = containerWith([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]);
    await saveKeyFile(row(keyFileId(older), older, 'migo-alice.migo', 'alice', 1_000));
    await saveKeyFile(row(keyFileId(newer), newer, 'migo-bekti.migo', 'bekti', 2_000));

    let files = await loadKeyFiles();
    assert.deepEqual(
      files.map((file) => file.username),
      ['bekti', 'alice'],
      'newest first — the account just used is the one first offered',
    );

    await removeKeyFile(keyFileId(newer));
    files = await loadKeyFiles();
    assert.deepEqual(
      files.map((file) => file.username),
      ['alice'],
      'forgetting one account leaves the other untouched',
    );
    await removeKeyFile(keyFileId(newer));
    assert.equal((await loadKeyFiles()).length, 1, 'forgetting twice is a no-op, not an error');
  } finally {
    fake.restore();
  }
});

test('an empty browser remembers nobody, and says so with an empty list', async () => {
  const fake = installFakeIndexedDb();
  try {
    assert.deepEqual(await loadKeyFiles(), [], 'no rows, no error — the import tile is the state');
  } finally {
    fake.restore();
  }
});
