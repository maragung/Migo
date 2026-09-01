/**
 * Key material and the key-directory domain.
 *
 * Two things live here. {@link KeyStore} is this device's private key material — the identity, the
 * current signed prekey, and the pool of one-time prekeys — held in memory and never serialised to
 * anything but the caller's own secure storage. {@link KeysDomain} is the wire side: it publishes the
 * *public* halves via KEY_PUBLISH and fetches peers' bundles via KEY_BUNDLE_FETCH.
 *
 * The store deliberately satisfies both crypto-layer seams: {@link LocalKeyStore} (what the 1:1 layer
 * needs to answer a first message — resolve a signed or one-time prekey by id) and
 * {@link IdentityProvider} (what the group layer needs to sign). One object backs both, so the client
 * wires a single {@link KeyStore} into {@link SessionCrypto} and {@link GroupCrypto} alike.
 *
 * # What crosses the wire
 *
 * Only public keys, key ids, and a signature. {@link KeyStore.publish} builds a {@link KeyPublish}
 * from public halves; the seeds behind them stay in the private fields of the crypto objects. This is
 * the invariant that makes the server untrusted for confidentiality: it can pick which bundle to
 * serve, but every bundle it serves is verified against the claimed identity's signature on the
 * fetching device before any Diffie-Hellman ({@link SignedPrekey.verify}, called by {@link initiate}).
 */

import type { Id } from '@migo/wire';
import {
  account,
  IdentityPublic,
  IdentitySecret,
  KeyPair,
  PrekeyBundle,
  SignedPrekey,
} from '@migo/crypto';
import {
  OP,
  encodeKeyBundleRequest,
  encodeKeyPublish,
  decodeKeyBundleResponse,
  decodeKeyPublishResult,
} from '@migo/protocol';
import type { KeyBundle, KeyPublish, KeyPublishResult, PrekeyEntry } from '@migo/protocol';

import { SdkError } from '../errors.js';
import type { IdentityProvider } from '../group-crypto.js';
import type { LocalKeyStore, PeerBundleSource } from '../session-crypto.js';
import type { Rpc } from './rpc.js';

/** How many one-time prekeys a fresh store mints, matching the server's expected batch size. */
const DEFAULT_ONE_TIME_PREKEYS = 64;

/** One published signed prekey: the id, the pair behind it, and the identity's signature over it. */
interface SignedPrekeyEntry {
  keyId: number;
  pair: KeyPair;
  signed: SignedPrekey;
}

/**
 * A serialisable snapshot of a {@link KeyStore}: every private seed plus the id counters.
 *
 * This is raw secret material. It exists so a device can persist its identity to the caller's own
 * secure local storage and rebuild the store after a restart without re-registering — it is never
 * transmitted anywhere. Seeds are 32 bytes each ({@link SEED_LEN}); the store reconstructs the full
 * key pairs from them, so signatures and public keys never need to be stored.
 */
export interface KeyStoreSnapshot {
  identitySigningSeed: Uint8Array;
  identityExchangeSeed: Uint8Array;
  signedPrekeyId: number;
  signedPrekeySeed: Uint8Array;
  oneTimePrekeys: Array<{ keyId: number; seed: Uint8Array }>;
  nextSignedPrekeyId: number;
  nextOneTimePrekeyId: number;
  /**
   * The unified account root's 32 bytes, on the device that founded the account. Absent on every
   * additional device: they generate a fresh identity and never inherit the root, which is what
   * keeps the root the one secret a `.migo` container has to carry.
   */
  root?: Uint8Array;
  /** The tracked AVAX transactions, newest first. Absent rather than empty when there are none. */
  trackedTxs?: TrackedTx[];
}

/**
 * One tracked AVAX transaction: what was sent, and how the tracker ended (§184, spec #59).
 *
 * The chain has no "list transactions by sender" without an indexer, so the Activity list is a
 * client-side record. It rides the key-store snapshot because that is already the device's sealed
 * local state — nothing here is transmitted anywhere. Wei magnitudes are `bigint` by construction;
 * a `number` at a call site is a silent precision bug the type prevents.
 */
export interface TrackedTx {
  /** The transaction hash, the handle the chain knows it by. */
  txHash: Uint8Array;
  /** The chain the transaction was signed for — EIP-155's replay protection, restated. */
  chainId: number;
  /** The recipient. */
  to: Uint8Array;
  /** The amount, wei. AVAX has 18 decimals. */
  valueWei: bigint;
  /** The fee ceiling the user confirmed: `maxFeePerGas * gasLimit`, wei. */
  feeWei: bigint;
  /** The gas limit that was signed. */
  gasLimit: number;
  /** When the transaction was broadcast, unix seconds. */
  atUnix: number;
  /** Spec #41's own word for where the transaction stands: `PENDING` at broadcast, one of the
   *  tracker's endings once it settles. */
  outcome: string;
  /** The block that included the transaction, once one did. */
  block?: number;
  /** The gas the block actually spent on it, from the receipt — the ceiling's honest companion. */
  gasUsed?: bigint;
}

/**
 * This device's private key material.
 *
 * Construct one with {@link KeyStore.create} for a new device, or {@link KeyStore.restore} to rebuild
 * from a {@link KeyStoreSnapshot} the caller persisted. Everything the two crypto layers ask of local
 * material is answered here; nothing here is sent anywhere except through {@link KeyStore.publish},
 * which emits public data.
 */
export class KeyStore implements LocalKeyStore, IdentityProvider {
  readonly #identity: IdentitySecret;
  #signedPrekey: SignedPrekeyEntry;
  readonly #oneTimePrekeys = new Map<number, KeyPair>();
  #nextSignedPrekeyId: number;
  #nextOneTimePrekeyId: number;
  readonly #root: account.MigoRoot | null;
  readonly #trackedTxs: TrackedTx[];

  private constructor(
    identity: IdentitySecret,
    signedPrekey: SignedPrekeyEntry,
    nextSignedPrekeyId: number,
    nextOneTimePrekeyId: number,
    root: account.MigoRoot | null,
    trackedTxs: TrackedTx[],
  ) {
    this.#identity = identity;
    this.#signedPrekey = signedPrekey;
    this.#nextSignedPrekeyId = nextSignedPrekeyId;
    this.#nextOneTimePrekeyId = nextOneTimePrekeyId;
    this.#root = root;
    this.#trackedTxs = trackedTxs;
  }

  /**
   * Mints a new device's key material: a fresh identity, one signed prekey, and a batch of one-time
   * prekeys. The caller persists the seeds ({@link KeyStore.snapshot}) and publishes the public
   * halves ({@link KeysDomain.publish}).
   */
  static create(oneTimePrekeyCount: number = DEFAULT_ONE_TIME_PREKEYS): KeyStore {
    const identity = IdentitySecret.generate();
    const signedPrekey = buildSignedPrekey(identity, 1);
    const store = new KeyStore(identity, signedPrekey, 2, 1, null, []);
    store.replenishOneTimePrekeys(oneTimePrekeyCount);
    return store;
  }

  /**
   * Mints the founding device of a brand-new account (§182): the E2EE identity is derived from the
   * root's E2EE domain rather than generated, which is what makes the account's E2EE history
   * recoverable from a `.migo` container. Additional devices never take this path — they call
   * {@link KeyStore.create} and hold no root.
   */
  static founding(
    root: account.MigoRoot,
    oneTimePrekeyCount: number = DEFAULT_ONE_TIME_PREKEYS,
  ): KeyStore {
    const seeds = account.foundingDeviceE2eeSeeds(root);
    const identity = IdentitySecret.fromSeeds(seeds.signing, seeds.exchange);
    const signedPrekey = buildSignedPrekey(identity, 1);
    const store = new KeyStore(identity, signedPrekey, 2, 1, root, []);
    store.replenishOneTimePrekeys(oneTimePrekeyCount);
    return store;
  }

  /**
   * Rebuilds a store from a {@link KeyStoreSnapshot} produced by {@link KeyStore.snapshot}.
   *
   * Key pairs are reconstructed from their seeds, and the signed prekey's signature is recomputed
   * over the restored pair, so a snapshot round-trips to a byte-identical published bundle. The
   * root and the tracked transactions ride along when the snapshot carries them, which is only on
   * the device that founded the account.
   */
  static restore(snapshot: KeyStoreSnapshot): KeyStore {
    const identity = IdentitySecret.fromSeeds(
      snapshot.identitySigningSeed,
      snapshot.identityExchangeSeed,
    );
    const signedPrekeyPair = KeyPair.fromSeed(snapshot.signedPrekeySeed);
    const signedPrekey: SignedPrekeyEntry = {
      keyId: snapshot.signedPrekeyId,
      pair: signedPrekeyPair,
      signed: SignedPrekey.create(identity, snapshot.signedPrekeyId, signedPrekeyPair),
    };
    const root = snapshot.root !== undefined ? account.MigoRoot.fromBytes(snapshot.root) : null;
    const store = new KeyStore(
      identity,
      signedPrekey,
      snapshot.nextSignedPrekeyId,
      snapshot.nextOneTimePrekeyId,
      root,
      snapshot.trackedTxs !== undefined ? [...snapshot.trackedTxs] : [],
    );
    for (const entry of snapshot.oneTimePrekeys) {
      store.#oneTimePrekeys.set(entry.keyId, KeyPair.fromSeed(entry.seed));
    }
    return store;
  }

  /**
   * The unified account root, on the device that founded the account, or `null` on every additional
   * device. This is the one secret the wallet and the `.migo` container both derive from — hand it
   * to nothing but the `account` surface's own functions.
   */
  root(): account.MigoRoot | null {
    return this.#root;
  }

  /**
   * The tracked AVAX transactions, newest first — the wallet surface's live list.
   *
   * The array returned is the store's own: the wallet flow mutates it (a record inserted at
   * broadcast, its ending written at settle) and the next {@link snapshot} seals the result, which
   * is the same trade the one-time prekey pool makes.
   */
  trackedTxs(): TrackedTx[] {
    return this.#trackedTxs;
  }

  /** This device's long-term identity secret. Backs both crypto layers. */
  identity(): IdentitySecret {
    return this.#identity;
  }

  /** The pair for a published signed prekey id, or `null` once that id has been rotated away. */
  signedPrekeyPair(signedPrekeyId: number): KeyPair | null {
    return this.#signedPrekey.keyId === signedPrekeyId ? this.#signedPrekey.pair : null;
  }

  /**
   * The pair for a published one-time prekey id *without* consuming it, or `null` if we do not hold
   * it.
   *
   * Peeking rather than consuming is what lets the 1:1 layer attempt a responder handshake for a
   * broadcast first message and only spend the prekey once the decrypt proves the message was ours;
   * see {@link LocalKeyStore.oneTimePrekeyPair}.
   */
  oneTimePrekeyPair(keyId: number): KeyPair | null {
    return this.#oneTimePrekeys.get(keyId) ?? null;
  }

  /**
   * Permanently removes a one-time prekey from the pool, after a first message using it has opened.
   *
   * Idempotent: consuming an id that is already gone is a no-op, so a lost race cannot throw. A
   * replayed first message finds the prekey gone and can never derive a second session from it.
   */
  consumeOneTimePrekey(keyId: number): void {
    this.#oneTimePrekeys.delete(keyId);
  }

  /** How many unused one-time prekeys remain, so the client knows when to replenish. */
  oneTimePrekeyCount(): number {
    return this.#oneTimePrekeys.size;
  }

  /** Adds `count` fresh one-time prekeys to the pool and returns their public entries to publish. */
  replenishOneTimePrekeys(count: number): PrekeyEntry[] {
    const added: PrekeyEntry[] = [];
    for (let i = 0; i < count; i += 1) {
      const keyId = this.#nextOneTimePrekeyId;
      this.#nextOneTimePrekeyId += 1;
      const pair = KeyPair.generate();
      this.#oneTimePrekeys.set(keyId, pair);
      added.push({ keyId, publicKey: pair.public() });
    }
    return added;
  }

  /** Rotates in a new signed prekey, retiring the old one. The client republishes afterward. */
  rotateSignedPrekey(): void {
    const keyId = this.#nextSignedPrekeyId;
    this.#nextSignedPrekeyId += 1;
    this.#signedPrekey = buildSignedPrekey(this.#identity, keyId);
  }

  /**
   * The public key material to publish to the server.
   *
   * The identity is its 64-byte wire form; the signed prekey carries the signature that binds it to
   * the identity; every current one-time prekey is offered so peers can start forward-secret sessions.
   */
  publish(): KeyPublish {
    const oneTimePrekeys: PrekeyEntry[] = [];
    for (const [keyId, pair] of this.#oneTimePrekeys) {
      oneTimePrekeys.push({ keyId, publicKey: pair.public() });
    }
    return {
      identityKey: this.#identity.public().toBytes(),
      signedPrekeyId: this.#signedPrekey.keyId,
      signedPrekey: this.#signedPrekey.signed.publicKey,
      signedPrekeySignature: this.#signedPrekey.signed.signature,
      oneTimePrekeys,
    };
  }

  /** The public identity, for fingerprint display and contact-change detection. */
  publicIdentity(): IdentityPublic {
    return this.#identity.public();
  }

  /**
   * A snapshot of every private seed and id counter, for the caller to persist to secure local
   * storage. Never transmit this — it is the device's full secret state.
   */
  snapshot(): KeyStoreSnapshot {
    const oneTimePrekeys: Array<{ keyId: number; seed: Uint8Array }> = [];
    for (const [keyId, pair] of this.#oneTimePrekeys) {
      oneTimePrekeys.push({ keyId, seed: pair.exposeSeed() });
    }
    return {
      identitySigningSeed: this.#identity.exposeSigningSeed(),
      identityExchangeSeed: this.#identity.exposeExchangeSeed(),
      signedPrekeyId: this.#signedPrekey.keyId,
      signedPrekeySeed: this.#signedPrekey.pair.exposeSeed(),
      oneTimePrekeys,
      nextSignedPrekeyId: this.#nextSignedPrekeyId,
      nextOneTimePrekeyId: this.#nextOneTimePrekeyId,
      // The root is present only on a device that holds it, and the tx list only when it has
      // entries: a field that exists to say "nothing here" costs every reader a skip for no
      // information.
      ...(this.#root !== null ? { root: this.#root.asBytes() } : {}),
      ...(this.#trackedTxs.length > 0 ? { trackedTxs: [...this.#trackedTxs] } : {}),
    };
  }
}

/** Rebuilds a signed prekey entry for `keyId` from a fresh pair signed by `identity`. */
function buildSignedPrekey(identity: IdentitySecret, keyId: number): SignedPrekeyEntry {
  const pair = KeyPair.generate();
  return { keyId, pair, signed: SignedPrekey.create(identity, keyId, pair) };
}

/** A peer device's id paired with its verified bundle, for enumerating a user's devices. */
export interface DeviceBundle {
  deviceId: Id;
  bundle: PrekeyBundle;
}

/**
 * The key-directory domain: publish our public keys, fetch peers'.
 *
 * Implements {@link PeerBundleSource} so the 1:1 layer can fetch a single device's bundle on demand
 * when it first needs to become the initiator. Also exposes {@link fetchDeviceBundles} for the
 * messaging layer, which needs the *set* of a user's devices to distribute a sender key to each.
 */
export class KeysDomain implements PeerBundleSource {
  readonly #rpc: Rpc;
  readonly #store: KeyStore;

  constructor(rpc: Rpc, store: KeyStore) {
    this.#rpc = rpc;
    this.#store = store;
  }

  /** Publishes this device's current public key material. Call after create, rotate, or replenish. */
  async publish(): Promise<KeyPublishResult> {
    return this.#rpc.call(
      OP.KEY_PUBLISH,
      encodeKeyPublish,
      decodeKeyPublishResult,
      this.#store.publish(),
    );
  }

  /**
   * Fetches and verifies the bundle for one device of one user.
   *
   * Rejects if the server returns no bundle for the named device. The returned bundle is not yet
   * verified here — {@link initiate} verifies it before any key agreement, which is the single place
   * verification must happen so it cannot be forgotten.
   */
  async fetchBundle(userId: Id, deviceId: Id): Promise<PrekeyBundle> {
    const request = { userId, deviceId };
    const response = await this.#rpc.call(
      OP.KEY_BUNDLE_FETCH,
      encodeKeyBundleRequest,
      decodeKeyBundleResponse,
      request,
    );
    const wire = response.bundles.find((entry) => entry.deviceId === deviceId);
    if (wire === undefined) {
      throw new SdkError(`keys: server returned no bundle for device ${deviceId}`);
    }
    return toPrekeyBundle(wire);
  }

  /**
   * Fetches every device bundle a user currently publishes.
   *
   * The messaging layer uses this to learn which devices to seal a sender-key distribution for.
   * Fetching consumes one one-time prekey per device server-side, so callers fetch once per
   * distribution round, not per message.
   */
  async fetchDeviceBundles(userId: Id): Promise<DeviceBundle[]> {
    const request = { userId };
    const response = await this.#rpc.call(
      OP.KEY_BUNDLE_FETCH,
      encodeKeyBundleRequest,
      decodeKeyBundleResponse,
      request,
    );
    return response.bundles.map((wire) => ({
      deviceId: wire.deviceId,
      bundle: toPrekeyBundle(wire),
    }));
  }
}

/** Rebuilds a crypto-layer {@link PrekeyBundle} from the wire {@link KeyBundle}. */
function toPrekeyBundle(wire: KeyBundle): PrekeyBundle {
  const identity = IdentityPublic.parse(wire.identityKey);
  const signedPrekey = new SignedPrekey(
    wire.signedPrekeyId,
    wire.signedPrekey,
    wire.signedPrekeySignature,
  );
  const oneTimePrekey =
    wire.oneTimePrekeyId !== undefined && wire.oneTimePrekey !== undefined
      ? { keyId: wire.oneTimePrekeyId, publicKey: wire.oneTimePrekey }
      : null;
  return new PrekeyBundle(identity, signedPrekey, oneTimePrekey);
}
