/**
 * X3DH — asynchronous session setup.
 *
 * The problem X3DH solves: Alice wants to send Bob an encrypted message, and Bob's phone is off.
 * A plain Diffie-Hellman handshake needs both parties online. X3DH lets Bob publish key material
 * in advance so Alice can establish a session with nobody but a server in the loop.
 *
 * Bob publishes, once per device:
 *
 * * **IK_B** — long-term identity key (see {@link module:identity}).
 * * **SPK_B** — a medium-term signed prekey, rotated on the order of days, signed by IK_B so
 *   Alice can tell it really came from Bob.
 * * **OPK_B** — a batch of one-time prekeys, each used at most once.
 *
 * Alice generates an ephemeral **EK_A** and computes up to four DH outputs:
 *
 * ```text
 * DH1 = DH(IK_A, SPK_B)     authenticates Alice to Bob
 * DH2 = DH(EK_A, IK_B)      authenticates Bob to Alice
 * DH3 = DH(EK_A, SPK_B)     forward secrecy from the medium-term key
 * DH4 = DH(EK_A, OPK_B)     forward secrecy from a key used exactly once
 * SK  = HKDF(0xFF×32 || DH1 || DH2 || DH3 || DH4)
 * ```
 *
 * Each output is there for a reason. Drop DH1 and Bob cannot tell who is talking to him. Drop DH2
 * and Alice cannot tell she is talking to Bob. Drop DH3 and compromising the identity key
 * retroactively decrypts everything. DH4 is what gives the *first* message forward secrecy even
 * before the ratchet starts — and it is optional because a device that has been offline long
 * enough may have run out of one-time prekeys. Running out degrades that one property; it does
 * not break the session, which is why the protocol keeps working rather than refusing to deliver.
 *
 * The `0xFF × 32` prefix is from the X3DH specification. It exists so the HKDF input can never be
 * confused with a raw curve point, which matters on curves where 32 bytes of DH output would
 * otherwise be a valid input to something else.
 *
 * # Associated data
 *
 * The identities of both parties are bound into the session's associated data: `IK_A || IK_B`.
 * Without that binding, a session key negotiated between Alice and Bob could be replayed into a
 * conversation with Carol — an unknown key-share attack. With it, a message only authenticates in
 * the conversation it was made for.
 *
 * This mirrors `server/crates/migo-crypto/src/x3dh.rs` step for step, so a web client and a
 * server-side responder derive the same secret from the same inputs.
 */

import { concatBytes } from '@noble/ciphers/utils.js';

import { CryptoError } from './errors.js';
import {
  IdentityPublic,
  IdentitySecret,
  KeyPair,
  PUBLIC_KEY_LEN,
  SignedPrekey,
} from './identity.js';
import * as kdf from './kdf.js';

/** The X3DH specification's domain-separation prefix. */
const F_PREFIX: Uint8Array = new Uint8Array(32).fill(0xff);

/** Length of an X3DH shared secret. */
const SHARED_SECRET_LEN = 32;

/** A one-time prekey as it travels in a bundle: its id and its public key. */
export interface OneTimePrekey {
  readonly keyId: number;
  readonly publicKey: Uint8Array;
}

/**
 * A prekey bundle as fetched from the server.
 *
 * This arrives over an untrusted channel: the server picks which bundle to serve.
 * {@link PrekeyBundle.verify} is therefore not optional, and {@link initiate} calls it before
 * doing any Diffie-Hellman.
 */
export class PrekeyBundle {
  /** The device's long-term identity. */
  readonly identity: IdentityPublic;
  /** The signed medium-term prekey. */
  readonly signedPrekey: SignedPrekey;
  /** A one-time prekey, if the device still has unused ones. */
  readonly oneTimePrekey: OneTimePrekey | null;

  constructor(
    identity: IdentityPublic,
    signedPrekey: SignedPrekey,
    oneTimePrekey: OneTimePrekey | null,
  ) {
    this.identity = identity;
    this.signedPrekey = signedPrekey;
    this.oneTimePrekey = oneTimePrekey;
  }

  /** Checks that the signed prekey really came from the claimed identity. */
  verify(): void {
    this.signedPrekey.verify(this.identity);
  }
}

/**
 * The material Alice must send Bob so he can derive the same secret.
 *
 * Travels in the first message's header. None of it is secret — it is public keys and key ids —
 * but all of it is required, and a message that arrives without it cannot be decrypted, which is
 * why it rides with the message rather than being fetched separately.
 */
export interface InitialMessage {
  /** Alice's long-term identity. */
  readonly identity: IdentityPublic;
  /** Alice's ephemeral public key for this session. */
  readonly ephemeralKey: Uint8Array;
  /** Which of Bob's signed prekeys was used. */
  readonly signedPrekeyId: number;
  /** Which one-time prekey was used, or `null` if none was available. */
  readonly oneTimePrekeyId: number | null;
}

/**
 * The output of a successful X3DH exchange.
 *
 * `sharedSecret` seeds the Double Ratchet root key; `associatedData` is authenticated in every
 * message of the session. The secret is held privately and is not printed or compared: the Rust
 * `SessionSeed` deliberately has no equality so nobody diffs secrets in non-constant time, and
 * {@link exposeSharedSecret} exists only for the ratchet, which copies it into a root key
 * immediately.
 */
export class SessionSeed {
  readonly #sharedSecret: Uint8Array;
  /** `IK_initiator || IK_responder`, 128 bytes. Public. */
  readonly associatedData: Uint8Array;

  constructor(sharedSecret: Uint8Array, associatedData: Uint8Array) {
    this.#sharedSecret = sharedSecret;
    this.associatedData = associatedData;
  }

  /** A copy of the 32-byte shared secret, for seeding a {@link RatchetSession}. */
  exposeSharedSecret(): Uint8Array {
    return this.#sharedSecret.slice();
  }

  /** `SessionSeed(shared_secret: ***, associated_data_len: N)`. Never the secret. */
  toString(): string {
    return `SessionSeed(shared_secret: ***, associated_data_len: ${this.associatedData.length})`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}

/** The result of {@link initiate}: the seed, the message to send, and the ephemeral pair. */
export interface Initiation {
  /** The session seed the initiator keeps. */
  readonly seed: SessionSeed;
  /** The header material the initiator sends in its first message. */
  readonly message: InitialMessage;
  /** The ephemeral pair, kept until the responder's first reply seeds the ratchet. */
  readonly ephemeral: KeyPair;
}

/**
 * Runs X3DH as the initiator.
 *
 * Verifies the bundle first: a bundle that does not verify means the server served something the
 * claimed device did not sign, and the correct response is to send nothing at all — the thrown
 * {@link CryptoError} is that refusal.
 *
 * The `ephemeral` pair is a parameter defaulting to a fresh one. Production always takes the
 * default; a test vector passes a pair from a known seed, which is the only way to make the
 * output deterministic without exposing a random source — the same shape as
 * {@link aead.sealWithNonce}.
 */
export function initiate(
  identity: IdentitySecret,
  bundle: PrekeyBundle,
  ephemeral: KeyPair = KeyPair.generate(),
): Initiation {
  bundle.verify();

  const spk = bundle.signedPrekey.publicKey;
  const chunks: Uint8Array[] = [
    F_PREFIX,
    // DH1: our identity to their signed prekey — proves who is speaking.
    identity.diffieHellman(spk),
    // DH2: our ephemeral to their identity — proves who is listening.
    ephemeral.diffieHellman(bundle.identity.exchange),
    // DH3: our ephemeral to their signed prekey — forward secrecy.
    ephemeral.diffieHellman(spk),
  ];
  // DH4: our ephemeral to a key they will never reuse — forward secrecy for the very first message.
  if (bundle.oneTimePrekey !== null) {
    chunks.push(ephemeral.diffieHellman(bundle.oneTimePrekey.publicKey));
  }

  const material = concatBytes(...chunks);
  const sharedSecret = kdf.derive(material, null, kdf.LABEL_X3DH, SHARED_SECRET_LEN);
  material.fill(0);

  const initiatorIdentity = identity.public();
  const seed = new SessionSeed(sharedSecret, associatedData(initiatorIdentity, bundle.identity));
  const message: InitialMessage = {
    identity: initiatorIdentity,
    ephemeralKey: ephemeral.public(),
    signedPrekeyId: bundle.signedPrekey.keyId,
    oneTimePrekeyId: bundle.oneTimePrekey !== null ? bundle.oneTimePrekey.keyId : null,
  };
  return { seed, message, ephemeral };
}

/**
 * Runs X3DH as the responder.
 *
 * `oneTimePrekey` must be the pair whose id the initial message names, and the caller must delete
 * it before returning — reusing a one-time prekey costs the forward secrecy it exists to provide.
 * Enforcing single use is the storage layer's job because only it knows what has already been
 * consumed; this function cannot tell a first use from a replay.
 */
export function respond(
  identity: IdentitySecret,
  signedPrekey: KeyPair,
  oneTimePrekey: KeyPair | null,
  message: InitialMessage,
): SessionSeed {
  if ((message.oneTimePrekeyId !== null) !== (oneTimePrekey !== null)) {
    // The initiator says it used a one-time prekey and we have not supplied one, or the reverse.
    // Deriving anyway would produce a key the other side does not have, and the failure would
    // surface as an undecryptable message with no explanation.
    throw CryptoError.noSession();
  }

  const chunks: Uint8Array[] = [
    F_PREFIX,
    signedPrekey.diffieHellman(message.identity.exchange),
    identity.diffieHellman(message.ephemeralKey),
    signedPrekey.diffieHellman(message.ephemeralKey),
  ];
  if (oneTimePrekey !== null) {
    chunks.push(oneTimePrekey.diffieHellman(message.ephemeralKey));
  }

  const material = concatBytes(...chunks);
  const sharedSecret = kdf.derive(material, null, kdf.LABEL_X3DH, SHARED_SECRET_LEN);
  material.fill(0);

  return new SessionSeed(sharedSecret, associatedData(message.identity, identity.public()));
}

/**
 * Builds the session's associated data: initiator identity, then responder.
 *
 * The order is fixed by role rather than by who is computing it, so both sides produce identical
 * bytes.
 */
function associatedData(initiator: IdentityPublic, responder: IdentityPublic): Uint8Array {
  const out = new Uint8Array(PUBLIC_KEY_LEN * 4);
  out.set(initiator.toBytes(), 0);
  out.set(responder.toBytes(), PUBLIC_KEY_LEN * 2);
  return out;
}
