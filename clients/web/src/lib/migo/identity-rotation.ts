'use client';

/**
 * The account identity-key rotation, as the web client runs it (§2402).
 *
 * Only the ML-DSA-65 signing identity rotates. The E2EE device identity, the ratchets, the safety
 * numbers peers see, and this device's own credential are untouched — the rotation replaces the key
 * that answers account ceremonies, nothing else. What makes it delicate is where the successor
 * lives: it is minted from *fresh randomness*, deliberately not derived from the root, so a `.migo`
 * container cannot reproduce it. From the moment the server accepts the rotation, the seed exists in
 * exactly two places — the server's record of the public key, and the device that rotated. On the
 * web client that device-home is the key-store snapshot (sealed at rest), mirrored into the device
 * record, which is the half that survives a sign-out.
 *
 * # The ordering contract
 *
 * This function follows the desktop client's order, which is the order that leaves no window where
 * an accepted successor exists nowhere:
 *
 *   1. Mint the successor.
 *   2. **Pre-commit**: install it as the active key in the key store and seal that state to
 *      IndexedDB — snapshot first, device record second — *before* the network call.
 *   3. Run the ceremony: ask for a challenge as this authenticated device, sign the payload with
 *      the key the server knows as active, introduce the successor.
 *
 * A pre-commit that cannot be persisted aborts before anything is sent: the in-memory store is
 * rolled back to the key it held, and nothing was rotated. A ceremony whose answer never arrives
 * (`TransportError`) keeps the pre-commit — the call may have landed with only its answer lost, and
 * rolling back then would strand an account whose active key exists nowhere. The next attempt
 * resolves it in either direction, which is the heal below.
 *
 * # The crash-window heal
 *
 * A predecessor rotation may itself have died mid-flight: this browser's store holds a successor
 * the server never accepted, and the signature above was made with it. The server answers that
 * with `INVALID_CREDENTIALS` — and a refused signature does not consume the challenge, which is
 * what makes healing possible. When the refusal names credentials *and* this store held a rotated
 * seed before the attempt, the ceremony is retried once with the root's derivation, the key the
 * server still knows. That retry is also the only path out of the split for a browser in that
 * state; if it is refused too, the store rolls back to the seed it held before the attempt and the
 * refusal is reported as-is rather than guessed at.
 */

import { RemoteError, TransportError, account } from '@migo/sdk';
import type { Id, MigoClient } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { loadDeviceRecord, saveDeviceRecord } from '@/lib/storage/device-record-store.js';
import { saveKeyStoreSnapshot } from '@/lib/storage/keystore-store.js';

/** How a rotation attempt ended, for the panel that started it. */
export type IdentityRotationOutcome =
  | { readonly state: 'done'; readonly successor: account.IdentityKey }
  | { readonly state: 'unfinished'; readonly message: string };

/** The server's answer never arrived; the sealed successor stays, and one more attempt settles it. */
export const ROTATION_UNFINISHED =
  "The server's answer never arrived; the new key is sealed on this device — rotate again to finish the change";

/** The successor could not be persisted, so nothing was sent. */
export const ROTATION_PERSIST_FAILED =
  'The new key could not be sealed on this device, so nothing was rotated';

/** This browser holds neither the root nor a rotated key, so it has nothing to answer with. */
export const ROTATION_NO_ROOT =
  'This browser does not hold the account backup, so it cannot rotate the identity key';

/**
 * The rotation-context line for a signature refusal. The generic table maps `INVALID_CREDENTIALS`
 * to a username-and-passphrase sentence, which is wrong here: no passphrase is involved, and the
 * likely cause is the one the desktop doc names — another root-holding device rotated first, so
 * the server's active key is one this browser never saw.
 */
const ROTATION_SIGNATURE_REFUSED =
  "the server did not accept this browser's signature for the account key — it may have been " +
  'rotated from another device';

/**
 * Seals the key store's current state — the successor after a pre-commit, the prior key after a
 * rollback — to both of the seed's homes: the snapshot (which a reload restores) and the device
 * record (which a sign-out spares).
 */
async function persistSeed(
  client: MigoClient,
  accountId: Id,
  seed: Uint8Array | null,
): Promise<void> {
  await saveKeyStoreSnapshot(client.keyStore.snapshot());
  const record = await loadDeviceRecord(accountId);
  if (record !== undefined) {
    await saveDeviceRecord({
      ...record,
      ...(seed !== null ? { rotatedIdentitySeed: seed } : {}),
      savedAt: Date.now(),
    });
  }
}

/**
 * Rotates the account's ML-DSA-65 identity key from this browser, and returns how it ended.
 *
 * A rotation that completed returns `{ state: 'done', successor }` — the caller owes the user the
 * fresh-backup advice (a `.migo` container sealed before the rotation carries only the retired root
 * derivation, so restoring from it will be refused until a fresh one is sealed). A rotation whose
 * answer was lost returns `{ state: 'unfinished' }` with the message to show. Everything else throws
 * an `Error` whose message is the sentence the panel shows: the server's refusal (after any heal
 * attempt and rollback), or the local persistence failure that stopped the attempt before it began.
 */
export async function rotateAccountIdentity(
  client: MigoClient,
  accountId: Id,
): Promise<IdentityRotationOutcome> {
  const root = client.keyStore.root();
  const priorSeed = client.keyStore.rotatedIdentitySeed();
  const active = client.keyStore.accountIdentityKey();
  if (root === null || active === null) {
    // Without the root this browser can neither mint a predecessor-bearing store nor answer for the
    // account — the rotation door is the founding device's, and the panel says so before we get here.
    throw new Error(ROTATION_NO_ROOT);
  }

  const successor = account.IdentityKey.generate();
  const seed = successor.exposeSeed();

  // The pre-commit: the successor becomes the active key in memory and on disk before the server is
  // asked anything, so a crash between here and the answer cannot lose it.
  client.keyStore.installRotatedIdentity(successor);
  try {
    await persistSeed(client, accountId, seed);
  } catch {
    const sealed = await rollbackToPrior(client, accountId, priorSeed);
    throw new Error(
      sealed
        ? ROTATION_PERSIST_FAILED
        : `${ROTATION_PERSIST_FAILED}, and the prior key could not be re-sealed either — the ` +
            'correct key is active for this session, and rotate again once this device can save',
    );
  }

  try {
    await client.rotateIdentity(active, successor);
    return { state: 'done', successor };
  } catch (cause) {
    // No answer is not a refusal: the call may have landed, and the pre-commit must survive the
    // ambiguity. The next attempt signs with the sealed successor, which the server accepts if the
    // call did land, and refuses otherwise — the heal below, which signs with the root.
    if (cause instanceof TransportError) {
      return { state: 'unfinished', message: ROTATION_UNFINISHED };
    }
    // The crash-window heal: this store already carried a successor the server may never have
    // accepted, and the signature above was made with it. The root's derivation is the key the
    // server still knows in that case, so answer again with it — same successor, fresh challenge.
    if (
      cause instanceof RemoteError &&
      cause.symbol === 'INVALID_CREDENTIALS' &&
      priorSeed !== null
    ) {
      try {
        await client.rotateIdentity(account.IdentityKey.fromRoot(root), successor);
        return { state: 'done', successor };
      } catch (healed) {
        if (healed instanceof TransportError) {
          return { state: 'unfinished', message: ROTATION_UNFINISHED };
        }
        throw await refusedAfterRollback(client, accountId, priorSeed, healed);
      }
    }
    throw await refusedAfterRollback(client, accountId, priorSeed, cause);
  }
}

/**
 * Rolls the store back to the seed it held before the attempt, re-seals that state, and builds the
 * `Error` the panel shows for the refusal that caused it.
 *
 * The sentence distinguishes the two ends a rollback can reach: both stores back to the prior key
 * (a refused rotation cost nothing), or the in-memory store restored while the sealed copy would
 * not take the write — the state Desktop reports the same way, because the honest report is the
 * only thing that keeps a person from trusting a sealed copy that is quietly stale.
 */
async function refusedAfterRollback(
  client: MigoClient,
  accountId: Id,
  priorSeed: Uint8Array | null,
  refused: unknown,
): Promise<Error> {
  if (priorSeed !== null) {
    client.keyStore.installRotatedIdentity(account.IdentityKey.fromSeed(priorSeed));
  } else {
    client.keyStore.clearRotatedIdentity();
  }
  try {
    await persistSeed(client, accountId, priorSeed);
  } catch {
    return new Error(
      `The identity key was not rotated (${rotationRefusalText(refused)}), and this device's ` +
        'sealed copy could not be restored — rotate again to retry the ceremony',
    );
  }
  return new Error(`The identity key was not rotated: ${rotationRefusalText(refused)}`);
}

/** The sentence for a refusal the heal did not rescue. */
function rotationRefusalText(refused: unknown): string {
  if (refused instanceof RemoteError && refused.symbol === 'INVALID_CREDENTIALS') {
    return ROTATION_SIGNATURE_REFUSED;
  }
  return friendlyError(refused);
}

/** The rollback half without a refusal to report: restores the prior key, and says whether it sealed. */
async function rollbackToPrior(
  client: MigoClient,
  accountId: Id,
  priorSeed: Uint8Array | null,
): Promise<boolean> {
  if (priorSeed !== null) {
    client.keyStore.installRotatedIdentity(account.IdentityKey.fromSeed(priorSeed));
  } else {
    client.keyStore.clearRotatedIdentity();
  }
  try {
    await persistSeed(client, accountId, priorSeed);
    return true;
  } catch {
    return false;
  }
}
