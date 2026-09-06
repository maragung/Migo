/**
 * Persistence for this device's {@link KeyStore} snapshot — sealed at rest under a key IndexedDB
 * holds as a `CryptoKey` object, never as bytes.
 *
 * The snapshot is the device's cryptographic identity: identity and prekey seeds as raw byte arrays.
 * It is written to IndexedDB and nowhere else, so the private key material never reaches localStorage,
 * a cookie, or the network. Restoring it on the next visit is what lets the device keep its identity —
 * and therefore its ability to decrypt history — across reloads without re-registering.
 *
 * # Sealed at rest (§9, §108, §164)
 *
 * The brief's rule is that key material at rest in the web client is a `CryptoKey` object, not a byte
 * array. This store honours it in two layers:
 *
 *   * A master key — a non-extractable HKDF `CryptoKey`, generated once on this device and stored
 *     under its own IndexedDB key *as a CryptoKey object*. WebCrypto refuses to export it because it
 *     was imported with `extractable: false`, so what the database can hand back is a key that can be
 *     used to derive but never read.
 *   * An AES-GCM wrapping key derived from that master under the label below (the one-label-per-
 *     derivation rule `packages/crypto/src/kdf.ts` states), which seals the snapshot bytes. The record
 *     holds `{ version, iv, ciphertext }`: the GCM tag makes it tamper-evident, and the seeds are
 *     plaintext only in memory, only while the crypto stack is using them.
 *
 * # The honest limit
 *
 * This is the bar §164 asks for: a copied IndexedDB file, a synced profile directory, a database the
 * origin's storage quota dumped to disk — none of them carry a usable identity any more. It cannot
 * bound a live XSS, which can call the same WebCrypto operations this module calls and unseal the
 * snapshot inside the page. That attack is why the Content-Security-Policy (sent by `tools/serve.mjs`
 * and carried by the root layout's meta policy) is part of the E2E security model rather than a
 * garnish — §108: one XSS is enough to misuse even a non-extractable key, so the two fronts are
 * paired, and neither pretends to do the other's job.
 *
 * # Legacy records, and failure that must not brick a session
 *
 * A snapshot written by an earlier build sits unsealed under the legacy key. The first load seals it,
 * re-stores it under the sealed key, and deletes the plaintext record. If sealing fails — no WebCrypto
 * in an embedder, a store that refused the CryptoKey — the legacy record is returned and kept
 * readable, and `save` falls back to writing the legacy shape: a session that survives its own
 * hardening beats a session its hardening destroyed.
 */

import type { KeyStoreSnapshot } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

/**
 * The legacy record: a plaintext snapshot, written by every build before sealing existed. Kept
 * readable as the migration source and the failure fallback; never written except by that fallback.
 */
const LEGACY_KEY = 'keystore-snapshot';

/** The sealed record, versioned so its shape can grow without silent corruption. */
const SEALED_KEY = 'keystore-snapshot:v1';

/** The master key, stored as the `CryptoKey` object itself. */
const MASTER_KEY = 'keystore-master';

/**
 * The HKDF label for the wrapping key, in the `migo-<purpose>-v1` shape `packages/crypto/src/kdf.ts`
 * states. The salt is a constant rather than a per-record value because the secret it salts is the
 * random, per-device master: HKDF's salt is a domain-separation input, not a second secret, and the
 * label is what keeps these derived bytes from ever being mistaken for another protocol's.
 */
const DERIVATION_LABEL = 'migo-web-keystore-seal-v1';
const DERIVATION_SALT = 'migo-web-keystore-master';

/** What the sealed record holds: an AES-GCM nonce and the sealed snapshot bytes. */
interface SealedSnapshot {
  version: 1;
  iv: Uint8Array<ArrayBuffer>;
  ciphertext: Uint8Array<ArrayBuffer>;
}

/**
 * The master key this device seals its snapshot under, creating it on first use.
 *
 * The 32 random bytes exist in one stack frame and are never persisted: what IndexedDB keeps is the
 * imported, non-extractable `CryptoKey`. Creation is held behind a single in-flight promise, and the
 * winner of a race with another context (a second tab mid-setup) is decided by re-reading the store,
 * because every sealed record must share the master that is actually on disk.
 */
let masterCreation: Promise<CryptoKey> | null = null;

async function loadMasterKey(): Promise<CryptoKey> {
  const existing = await idbGet<CryptoKey>(MASTER_KEY);
  if (existing !== undefined) {
    return existing;
  }
  if (masterCreation === null) {
    const create = async (): Promise<CryptoKey> => {
      const seed = crypto.getRandomValues(new Uint8Array(32));
      const candidate = await crypto.subtle.importKey('raw', seed, 'HKDF', false, ['deriveKey']);
      await idbSet(MASTER_KEY, candidate);
      const stored = await idbGet<CryptoKey>(MASTER_KEY);
      return stored ?? candidate;
    };
    masterCreation = create().finally(() => {
      masterCreation = null;
    });
  }
  return masterCreation;
}

/**
 * The AES-GCM wrapping key for one seal or unseal, derived fresh from the master.
 *
 * Non-extractable like the master: it exists to wrap this snapshot and never leaves WebCrypto.
 */
function deriveWrappingKey(master: CryptoKey): Promise<CryptoKey> {
  const encoder = new TextEncoder();
  return crypto.subtle.deriveKey(
    {
      name: 'HKDF',
      hash: 'SHA-256',
      salt: encoder.encode(DERIVATION_SALT),
      info: encoder.encode(DERIVATION_LABEL),
    },
    master,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt'],
  );
}

/**
 * Tags for the JSON encoding below. The snapshot is a documented, plain interface, so a field
 * colliding with these names would have to be deliberate.
 */
type Tagged =
  { readonly t: 'u8'; readonly v: number[] } | { readonly t: 'big'; readonly v: string };

/** The snapshot as bytes, in a JSON encoding IndexedDB's structured clone would not need. */
function encodeSnapshot(snapshot: KeyStoreSnapshot): Uint8Array<ArrayBuffer> {
  const encoder = new TextEncoder();
  const json = JSON.stringify(snapshot, (_key, value: unknown) => {
    if (typeof value === 'bigint') {
      return { t: 'big', v: value.toString() } satisfies Tagged;
    }
    if (value instanceof Uint8Array) {
      return { t: 'u8', v: Array.from(value) } satisfies Tagged;
    }
    return value;
  });
  return encoder.encode(json);
}

/** The snapshot back from {@link encodeSnapshot}'s bytes. */
function decodeSnapshot(bytes: Uint8Array): KeyStoreSnapshot {
  const json = new TextDecoder().decode(bytes);
  return JSON.parse(json, (_key, value: unknown) => {
    if (typeof value === 'object' && value !== null && 't' in value && 'v' in value) {
      const tagged = value as Tagged;
      if (tagged.t === 'u8') {
        return Uint8Array.from(tagged.v);
      }
      if (tagged.t === 'big') {
        return BigInt(tagged.v);
      }
    }
    return value;
  }) as KeyStoreSnapshot;
}

/** Seals a snapshot under the master key: a fresh nonce, and ciphertext that carries its own tag. */
async function sealSnapshot(
  master: CryptoKey,
  snapshot: KeyStoreSnapshot,
): Promise<SealedSnapshot> {
  const wrapping = await deriveWrappingKey(master);
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ciphertext = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv },
    wrapping,
    encodeSnapshot(snapshot),
  );
  return { version: 1, iv, ciphertext: new Uint8Array(ciphertext) };
}

/** Opens a sealed record, or throws when the master does not fit or the bytes were tampered with. */
async function unsealSnapshot(
  master: CryptoKey,
  sealed: SealedSnapshot,
): Promise<KeyStoreSnapshot> {
  const wrapping = await deriveWrappingKey(master);
  const plaintext = await crypto.subtle.decrypt(
    { name: 'AES-GCM', iv: sealed.iv },
    wrapping,
    sealed.ciphertext,
  );
  return decodeSnapshot(new Uint8Array(plaintext));
}

/**
 * Loads the persisted key-store snapshot, or `undefined` on a first visit.
 *
 * A sealed record that cannot be opened (a master key the store lost, ciphertext that was tampered
 * with) is reported as absent rather than thrown: the provider's resume path answers a thrown load by
 * wiping the session, and destroying a session its own hardening could not read is the one failure
 * this module must never cause. The record itself stays, so a later build that can read it still may.
 */
export async function loadKeyStoreSnapshot(): Promise<KeyStoreSnapshot | undefined> {
  const sealed = await idbGet<SealedSnapshot>(SEALED_KEY);
  if (sealed !== undefined) {
    const master = await idbGet<CryptoKey>(MASTER_KEY);
    if (master !== undefined) {
      try {
        return await unsealSnapshot(master, sealed);
      } catch {
        // Fall through to the legacy record, for the rare case where one is still there.
      }
    }
  }

  const legacy = await idbGet<KeyStoreSnapshot>(LEGACY_KEY);
  if (legacy === undefined) {
    return undefined;
  }

  // First load after the sealing change: migrate the plaintext record away.
  try {
    const master = await loadMasterKey();
    await idbSet(SEALED_KEY, await sealSnapshot(master, legacy));
    await idbDelete(LEGACY_KEY);
  } catch {
    // Sealing failed. The legacy record stays exactly as it is — readable now, migratable later —
    // because the alternative is a session that cannot restore its own identity.
  }
  return legacy;
}

/**
 * Persists the current key-store snapshot, sealed.
 *
 * Falls back to the legacy plaintext record when sealing is impossible, for the reasons the module
 * doc gives: IndexedDB is still the one store the audit allows, so the fallback is the pre-sealing
 * behaviour rather than a new exposure, and a refused seal must not cost the device its identity.
 */
export async function saveKeyStoreSnapshot(snapshot: KeyStoreSnapshot): Promise<void> {
  try {
    const master = await loadMasterKey();
    await idbSet(SEALED_KEY, await sealSnapshot(master, snapshot));
  } catch {
    await idbSet(LEGACY_KEY, snapshot);
    return;
  }
  // Only once the sealed record is on disk: the plaintext one must never outlive a successful seal.
  await idbDelete(LEGACY_KEY);
}

/**
 * Removes the persisted key-store snapshot (on sign-out) — the sealed record, any legacy record that
 * a failed migration left, and the master key itself.
 *
 * The sealed record goes first: a failure partway through must never leave a sealed record whose
 * master was just deleted, because that orphan is unopenable forever, and ordering is the only
 * atomicity a key/value store offers.
 */
export async function clearKeyStoreSnapshot(): Promise<void> {
  await idbDelete(SEALED_KEY);
  await idbDelete(LEGACY_KEY);
  await idbDelete(MASTER_KEY);
}
