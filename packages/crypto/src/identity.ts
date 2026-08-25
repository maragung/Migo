/**
 * Long-term identities and prekeys.
 *
 * A Migo device has two long-term key pairs, not one:
 *
 * * **Ed25519** for signatures — proving that a prekey really came from this device, and signing
 *   mesh frames between servers.
 * * **X25519** for Diffie-Hellman — the identity half of X3DH.
 *
 * Signal uses a single Curve25519 key for both via XEdDSA, converting between the Edwards and
 * Montgomery forms. That saves 32 bytes per published identity and costs a birational map that
 * has to be implemented correctly in three languages. Two separate keys is the boring choice,
 * and boring is the right default in cryptographic code: nothing here is a novel construction,
 * so there is nothing here to get subtly wrong. This is the same decision, and the same wire
 * form, as `server/crates/migo-crypto/src/identity.rs`.
 *
 * The wire form of a published identity is `signing || exchange`, 64 bytes, in that order. Both
 * halves are needed to talk to a device, so splitting them across two fields would only create
 * a state where one is present without the other.
 *
 * # What the server holds
 *
 * Public halves only. {@link IdentitySecret} never leaves the device that generated it and has
 * no serialisation to anything but the device's own encrypted storage. There is no server-side
 * key escrow, no "recover my messages" that works without device-held material, and therefore no
 * request an administrator or a court can serve on Migo that produces someone's plaintext.
 *
 * ## Secret keys as seeds
 *
 * The Ed25519 secret key is its 32-byte seed, matching `ed25519_dalek::SigningKey::to_bytes`.
 * The X25519 secret is stored as its 32-byte seed too. `@noble/curves` clamps the scalar (RFC
 * 7748) inside every `getPublicKey` and `getSharedSecret`, exactly as `x25519-dalek` does, so
 * the stored form never crosses the wire and never affects a public key or a DH output — two
 * devices agree on every shared secret whether either stored the raw or the clamped seed. The
 * seed is what {@link IdentitySecret.exposeExchangeSeed} returns for the device's own store, and
 * what {@link IdentitySecret.fromSeeds} reads back.
 */

import { ed25519, x25519 } from '@noble/curves/ed25519.js';
import { bytesToHex, equalBytes } from '@noble/ciphers/utils.js';

import { CryptoError } from './errors.js';
import * as kdf from './kdf.js';
import { randomBytes } from './random.js';

/** Length of an Ed25519 or X25519 public key. */
export const PUBLIC_KEY_LEN = 32;
/** Length of the published identity: signing key followed by exchange key. */
export const IDENTITY_PUBLIC_LEN = PUBLIC_KEY_LEN * 2;
/** Length of an Ed25519 signature. */
export const SIGNATURE_LEN = 64;
/** Length of a secret-key seed. */
export const SEED_LEN = 32;

/**
 * Domain separator for a signed prekey.
 *
 * Signatures are always over a label plus the data. Without the label, a signature produced for
 * one purpose could be presented as a signature for another — the classic cross-protocol
 * signature confusion. The bytes are `b"migo-signed-prekey-v1"`, character for character the
 * same as the Rust constant.
 */
const PREKEY_DOMAIN = new TextEncoder().encode('migo-signed-prekey-v1');

const ENCODER = new TextEncoder();

/**
 * The public half of a device identity.
 *
 * Holds the two 32-byte public keys. Instances made by {@link IdentityPublic.parse} have been
 * validated; instances built directly from trusted local material (via {@link fromParts}) are
 * assumed valid, which is why that constructor is not the public path for peer-supplied bytes.
 */
export class IdentityPublic {
  /** Ed25519 verifying key. */
  readonly signing: Uint8Array;
  /** X25519 public key. */
  readonly exchange: Uint8Array;

  private constructor(signing: Uint8Array, exchange: Uint8Array) {
    this.signing = signing;
    this.exchange = exchange;
  }

  /**
   * Builds a public identity from two keys already known to be valid.
   *
   * For material this device produced ({@link IdentitySecret.public}), where the keys came out of
   * a generator and cannot be malformed. Peer-supplied bytes must go through {@link parse}, which
   * checks them.
   */
  static fromParts(signing: Uint8Array, exchange: Uint8Array): IdentityPublic {
    return new IdentityPublic(signing.slice(), exchange.slice());
  }

  /** Serialises to the 64-byte wire form. */
  toBytes(): Uint8Array {
    const out = new Uint8Array(IDENTITY_PUBLIC_LEN);
    out.set(this.signing, 0);
    out.set(this.exchange, PUBLIC_KEY_LEN);
    return out;
  }

  /**
   * Parses the 64-byte wire form, rejecting keys that are not valid points.
   *
   * Rejects a signing key that is not on the curve here, at parse time, rather than at first use.
   * An invalid key that is stored and only fails later produces a session that cannot be
   * repaired. The exchange key is checked against the small-order set for the same reason
   * {@link isSmallOrder} exists.
   */
  static parse(bytes: Uint8Array): IdentityPublic {
    if (bytes.length !== IDENTITY_PUBLIC_LEN) {
      throw CryptoError.badLength('identity public key', IDENTITY_PUBLIC_LEN, bytes.length);
    }
    const signing = bytes.slice(0, PUBLIC_KEY_LEN);
    const exchange = bytes.slice(PUBLIC_KEY_LEN);

    if (!ed25519.utils.isValidPublicKey(signing)) {
      throw CryptoError.invalidPublicKey();
    }
    if (isSmallOrder(exchange)) {
      throw CryptoError.invalidPublicKey();
    }
    return new IdentityPublic(signing, exchange);
  }

  /**
   * Verifies a signature made by this identity over `label || message`.
   *
   * Throws {@link CryptoError} rather than returning a boolean, so that a caller cannot forget to
   * check the result — an ignored `false` is a forged signature accepted. The distinction between
   * an unusable key and a bad signature is preserved because they mean different things: the
   * former is a malformed identity, the latter is a message that does not belong.
   */
  verify(label: Uint8Array, message: Uint8Array, signature: Uint8Array): void {
    if (!ed25519.utils.isValidPublicKey(this.signing)) {
      throw CryptoError.invalidPublicKey();
    }
    if (signature.length !== SIGNATURE_LEN) {
      throw CryptoError.badLength('signature', SIGNATURE_LEN, signature.length);
    }
    const signed = concat(label, message);
    let ok: boolean;
    try {
      ok = ed25519.verify(signature, signed, this.signing);
    } catch {
      // A malformed signature encoding surfaces as a thrown error from the library; to a caller
      // it is the same outcome as a well-formed signature that does not verify — the message is
      // rejected — so it collapses to the same error rather than leaking which it was.
      throw CryptoError.badSignature();
    }
    if (!ok) {
      throw CryptoError.badSignature();
    }
  }

  /**
   * The 32-byte fingerprint users compare when verifying a contact in person.
   *
   * Derived from the full identity rather than one half, so a mismatch in either key shows up.
   * Rendered as safety numbers by the client.
   */
  fingerprint(): Uint8Array {
    return kdf.derive(
      this.toBytes(),
      ENCODER.encode('migo-fingerprint'),
      'migo-fingerprint-v1',
      32,
    );
  }

  /** Value equality over both keys, for tests and contact-change detection. */
  equals(other: IdentityPublic): boolean {
    return equalBytes(this.signing, other.signing) && equalBytes(this.exchange, other.exchange);
  }
}

/**
 * The private half of a device identity. Never leaves the device.
 *
 * The two seeds live in `#private` fields and neither {@link toString} nor Node's inspect hook
 * reveals them, for the same reason {@link SymmetricKey} hides its bytes: a secret that can be
 * printed is eventually printed into a log.
 */
export class IdentitySecret {
  readonly #signingSeed: Uint8Array;
  readonly #exchangeSeed: Uint8Array;

  private constructor(signingSeed: Uint8Array, exchangeSeed: Uint8Array) {
    this.#signingSeed = signingSeed;
    this.#exchangeSeed = exchangeSeed;
  }

  /** Generates a new device identity from the platform CSPRNG. */
  static generate(): IdentitySecret {
    return new IdentitySecret(randomBytes(SEED_LEN), randomBytes(SEED_LEN));
  }

  /**
   * Rebuilds an identity from its two 32-byte seeds.
   *
   * Used by the device's own encrypted storage and by test vectors. The order is `signing`, then
   * `exchange`, matching the public wire form.
   */
  static fromSeeds(signingSeed: Uint8Array, exchangeSeed: Uint8Array): IdentitySecret {
    if (signingSeed.length !== SEED_LEN) {
      throw CryptoError.badLength('signing seed', SEED_LEN, signingSeed.length);
    }
    if (exchangeSeed.length !== SEED_LEN) {
      throw CryptoError.badLength('exchange seed', SEED_LEN, exchangeSeed.length);
    }
    return new IdentitySecret(signingSeed.slice(), exchangeSeed.slice());
  }

  /** The public half, for publishing to the server. */
  public(): IdentityPublic {
    return IdentityPublic.fromParts(
      ed25519.getPublicKey(this.#signingSeed),
      x25519.getPublicKey(this.#exchangeSeed),
    );
  }

  /** Signs `label || message`. */
  sign(label: Uint8Array, message: Uint8Array): Uint8Array {
    return ed25519.sign(concat(label, message), this.#signingSeed);
  }

  /**
   * Diffie-Hellman between this identity and a peer's X25519 public key.
   *
   * Not part of the published surface in Rust (`pub(crate)`); exposed here because the X3DH and
   * ratchet modules live in sibling files rather than the same module, and TypeScript has no
   * crate-private visibility. Application code has no reason to call it directly.
   */
  diffieHellman(peer: Uint8Array): Uint8Array {
    return diffieHellman(this.#exchangeSeed, peer);
  }

  /** Exposes the signing seed, for writing to the device's encrypted store. */
  exposeSigningSeed(): Uint8Array {
    return this.#signingSeed.slice();
  }

  /** Exposes the exchange seed, for writing to the device's encrypted store. */
  exposeExchangeSeed(): Uint8Array {
    return this.#exchangeSeed.slice();
  }

  /** `IdentitySecret(***)`. Never the seeds. */
  toString(): string {
    return 'IdentitySecret(***)';
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return 'IdentitySecret(***)';
  }
}

/**
 * An X25519 key pair used as a prekey or a ratchet key.
 *
 * The seed is private; the public key is not. {@link toString} shows the public key in hex and
 * nothing else, matching the Rust `Debug` that prints `public` and hides the seed.
 */
export class KeyPair {
  readonly #secretSeed: Uint8Array;
  readonly #public: Uint8Array;

  private constructor(secretSeed: Uint8Array, publicKey: Uint8Array) {
    this.#secretSeed = secretSeed;
    this.#public = publicKey;
  }

  /** Generates a fresh pair from the platform CSPRNG. */
  static generate(): KeyPair {
    return KeyPair.fromSeed(randomBytes(SEED_LEN));
  }

  /** Rebuilds a pair from its 32-byte seed. */
  static fromSeed(seed: Uint8Array): KeyPair {
    if (seed.length !== SEED_LEN) {
      throw CryptoError.badLength('key pair seed', SEED_LEN, seed.length);
    }
    const secretSeed = seed.slice();
    return new KeyPair(secretSeed, x25519.getPublicKey(secretSeed));
  }

  /** The public half. */
  public(): Uint8Array {
    return this.#public.slice();
  }

  /** Diffie-Hellman with a peer's public key. */
  diffieHellman(peer: Uint8Array): Uint8Array {
    return diffieHellman(this.#secretSeed, peer);
  }

  /** Exposes the seed, for the device's encrypted store. */
  exposeSeed(): Uint8Array {
    return this.#secretSeed.slice();
  }

  /** `KeyPair(<public hex>)`. Never the seed. */
  toString(): string {
    return `KeyPair(${bytesToHex(this.#public)})`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}

/**
 * A prekey with the signature that binds it to an identity.
 *
 * Constructed directly from parts when received from the server (the fields are public wire
 * data), or via {@link SignedPrekey.create} when this device publishes one.
 */
export class SignedPrekey {
  /** Identifier the publisher assigned, so a bundle can name which prekey it used. */
  readonly keyId: number;
  /** X25519 public key. */
  readonly publicKey: Uint8Array;
  /** Ed25519 signature over the domain label, the key id, and the key. */
  readonly signature: Uint8Array;

  constructor(keyId: number, publicKey: Uint8Array, signature: Uint8Array) {
    this.keyId = keyId;
    this.publicKey = publicKey;
    this.signature = signature;
  }

  /** Signs `pair` with `identity`. */
  static create(identity: IdentitySecret, keyId: number, pair: KeyPair): SignedPrekey {
    const publicKey = pair.public();
    return new SignedPrekey(
      keyId,
      publicKey,
      identity.sign(PREKEY_DOMAIN, signedBytes(keyId, publicKey)),
    );
  }

  /**
   * Verifies that this prekey was signed by `identity`.
   *
   * This is the check that makes the server untrusted. The server chooses which bundle to serve,
   * so without it the server could substitute a prekey it controls and read everything sent to
   * that device. With it, a substituted prekey fails verification on the sender's device before
   * any message is composed. A verification failure of any kind becomes
   * {@link CryptoError.invalidPrekeyBundle}, since to the caller the bundle is simply unusable.
   */
  verify(identity: IdentityPublic): void {
    try {
      identity.verify(PREKEY_DOMAIN, signedBytes(this.keyId, this.publicKey), this.signature);
    } catch {
      throw CryptoError.invalidPrekeyBundle();
    }
  }
}

/**
 * The bytes covered by a prekey signature: key id (big-endian) then key.
 *
 * The id is inside the signature so that a valid signature cannot be moved onto a different id
 * and cause the two sides to disagree about which prekey was used.
 */
function signedBytes(keyId: number, publicKey: Uint8Array): Uint8Array {
  const out = new Uint8Array(4 + PUBLIC_KEY_LEN);
  new DataView(out.buffer).setUint32(0, keyId, false);
  out.set(publicKey, 4);
  return out;
}

/**
 * Diffie-Hellman with the small-order guard the Rust side applies.
 *
 * `@noble/curves` already rejects low-order inputs by throwing rather than returning the all-zero
 * shared secret, but the explicit check comes first so the error is {@link CryptoError} with the
 * `InvalidPublicKey` kind — the same word the Rust crate answers with — rather than a library
 * error whose text is written for a different audience. An all-zero secret that is silently
 * accepted means both sides derive the same key from nothing, which is indistinguishable from a
 * working session until someone notices the ciphertext is decryptable by anyone.
 */
function diffieHellman(secretSeed: Uint8Array, peer: Uint8Array): Uint8Array {
  if (peer.length !== PUBLIC_KEY_LEN || isSmallOrder(peer)) {
    throw CryptoError.invalidPublicKey();
  }
  try {
    return x25519.getSharedSecret(secretSeed, peer);
  } catch {
    throw CryptoError.invalidPublicKey();
  }
}

/**
 * The known small-order X25519 points, from RFC 7748 section 6.1 and Curve25519 analysis.
 *
 * The three high-value points are written out; the three near the field prime differ only in
 * their first byte (236, 237, 238) followed by thirty `0xFF` bytes and a trailing `0x7F`, so they
 * are built rather than transcribed to keep the run of identical bytes from hiding a typo. The
 * set is identical to the `SMALL_ORDER` constant in `identity.rs`.
 */
const SMALL_ORDER: readonly Uint8Array[] = [
  new Uint8Array(32),
  Uint8Array.of(1, ...new Array<number>(31).fill(0)),
  Uint8Array.of(
    224,
    235,
    122,
    124,
    59,
    65,
    184,
    174,
    22,
    86,
    227,
    250,
    241,
    159,
    196,
    106,
    218,
    9,
    141,
    235,
    156,
    50,
    177,
    253,
    134,
    98,
    5,
    22,
    95,
    73,
    184,
    0,
  ),
  Uint8Array.of(
    95,
    156,
    149,
    188,
    163,
    80,
    140,
    36,
    177,
    208,
    177,
    85,
    156,
    131,
    239,
    91,
    4,
    68,
    92,
    196,
    88,
    28,
    142,
    134,
    216,
    34,
    78,
    221,
    208,
    159,
    17,
    87,
  ),
  nearPrimePoint(236),
  nearPrimePoint(237),
  nearPrimePoint(238),
];

/** Builds one of the three near-prime small-order points: `first`, thirty `0xFF`, then `0x7F`. */
function nearPrimePoint(first: number): Uint8Array {
  const point = new Uint8Array(32).fill(0xff);
  point[0] = first;
  point[31] = 0x7f;
  return point;
}

/**
 * True if `publicKey` is one of the small-order points.
 *
 * Every candidate is compared even after a match is found, so the time taken does not reveal
 * which point matched. The input is a peer's public key — already public — so the leak would be
 * minor, but there is no reason to accept it, and {@link equalBytes} is constant-time per
 * comparison.
 */
function isSmallOrder(publicKey: Uint8Array): boolean {
  let matched = false;
  for (const candidate of SMALL_ORDER) {
    if (equalBytes(publicKey, candidate)) {
      matched = true;
    }
  }
  return matched;
}

/** Concatenates two byte strings into a fresh buffer. */
function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}
