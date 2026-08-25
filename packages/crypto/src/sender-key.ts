/**
 * Sender keys — group messaging without quadratic cost.
 *
 * A pairwise Double Ratchet in a 200-member group means encrypting every message 199 times. At
 * mig33-era group sizes that is the difference between a message that sends and a message that
 * times out on a 2G connection.
 *
 * Instead, each sender keeps one symmetric chain per group. The chain key is distributed once to
 * each member *over the pairwise E2E channels*, so the server never sees it, and after that a
 * message is encrypted once and fanned out to everyone. Cost per message becomes O(1) in the
 * sender's work and O(1) in bandwidth, with the O(n) cost paid only when the key distribution
 * changes.
 *
 * # What this gives up, and what replaces it
 *
 * A sender key has forward secrecy — chain keys advance and old ones are deleted — but no
 * post-compromise security. Stealing a member's current chain key lets the thief read that sender's
 * future messages until the key is replaced. The ratchet cannot heal on its own here, because there
 * is no pairwise DH exchange to mix in.
 *
 * The replacement is rotation, and it is not optional:
 *
 * * When a member **leaves or is removed**, every remaining sender distributes a fresh chain.
 *   Otherwise the departed member keeps reading the group.
 * * After {@link MAX_MESSAGES_PER_CHAIN} messages, so a compromise has a bounded window even in a
 *   group where nobody ever leaves.
 *
 * Rotation on removal is a correctness requirement, not a policy knob. A group implementation that
 * skips it has a member who left in March still reading messages in August.
 *
 * # Signing
 *
 * Symmetric keys prove only that *somebody in the group* wrote the message — every member holds the
 * chain key, so any member could forge another's message. Each message therefore carries an
 * Ed25519 signature from the sender's identity key. Without it, group authorship is unverifiable,
 * which in a moderation context means a member can fabricate a message attributed to someone else.
 *
 * This mirrors `server/crates/migo-crypto/src/sender_key.rs`, down to verifying the signature
 * before deriving any key.
 */

import { NONCE_LEN, SymmetricKey } from './aead.js';
import * as aead from './aead.js';
import { CryptoError } from './errors.js';
import { IdentityPublic, IdentitySecret } from './identity.js';
import * as kdf from './kdf.js';
import { randomBytes } from './random.js';

/** Messages a single chain may produce before it must be rotated. */
export const MAX_MESSAGES_PER_CHAIN = 2_000;

/** How far ahead of the receiver a message may claim to be. */
export const MAX_CHAIN_GAP = 1_000;

/** Length of a group chain key. */
const CHAIN_KEY_LEN = 32;

/** Domain separator for a group message signature. */
const GROUP_DOMAIN = new TextEncoder().encode('migo-sender-key-v1');

/**
 * The distribution message a sender hands to each group member.
 *
 * Travels inside the pairwise E2E channel, never in the clear, and never through a code path that
 * could log it. The chain key is secret material held in a private field, exposed only through
 * {@link exposeChainKey}; everything else in this type is not secret.
 */
export class SenderKeyDistribution {
  /** Which chain this is, so a rotation can be distinguished from a resend. */
  readonly chainId: number;
  /**
   * The message number the chain key corresponds to.
   *
   * A member who joins mid-conversation receives the chain key as of *now*, not from the
   * beginning. That is deliberate: a new member must not be able to decrypt history they were not
   * present for.
   */
  readonly messageNumber: number;
  readonly #chainKey: Uint8Array;
  /** The sender's identity, for verifying its signatures. */
  readonly identity: IdentityPublic;

  constructor(
    chainId: number,
    messageNumber: number,
    chainKey: Uint8Array,
    identity: IdentityPublic,
  ) {
    this.chainId = chainId;
    this.messageNumber = messageNumber;
    this.#chainKey = chainKey;
    this.identity = identity;
  }

  /**
   * Borrows the chain key. The greppable audit point for this secret leaving the type.
   *
   * Returns the live buffer, as {@link SymmetricKey.expose} does; the only caller,
   * {@link ReceiverKeyState.accept}, copies it into its own state immediately.
   */
  exposeChainKey(): Uint8Array {
    return this.#chainKey;
  }

  /** `SenderKeyDistribution(chain_id: N, message_number: M, chain_key: ***)`. Never the key. */
  toString(): string {
    return `SenderKeyDistribution(chain_id: ${this.chainId}, message_number: ${this.messageNumber}, chain_key: ***)`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}

/** The header on a group message. */
export class SenderKeyHeader {
  /** Which chain of the sender's this message belongs to. */
  readonly chainId: number;
  /** Index within that chain. */
  readonly messageNumber: number;

  constructor(chainId: number, messageNumber: number) {
    this.chainId = chainId;
    this.messageNumber = messageNumber;
  }

  /** Encoded length: two big-endian `u32`s. */
  static readonly ENCODED_LEN = 8;

  /** Serialises the header. Fixed-width, because it is authenticated. */
  toBytes(): Uint8Array {
    const out = new Uint8Array(SenderKeyHeader.ENCODED_LEN);
    const view = new DataView(out.buffer);
    view.setUint32(0, this.chainId, false);
    view.setUint32(4, this.messageNumber, false);
    return out;
  }

  /** Parses a header, rejecting anything not exactly {@link ENCODED_LEN} bytes. */
  static parse(bytes: Uint8Array): SenderKeyHeader {
    if (bytes.length !== SenderKeyHeader.ENCODED_LEN) {
      throw CryptoError.malformedHeader();
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    return new SenderKeyHeader(view.getUint32(0, false), view.getUint32(4, false));
  }
}

/** A sealed group message. */
export interface SenderKeyMessage {
  /** Chain and position. */
  readonly header: SenderKeyHeader;
  /** AEAD output, without a nonce prefix — the nonce is derived. */
  readonly ciphertext: Uint8Array;
  /** The sender's signature over the header and the ciphertext. */
  readonly signature: Uint8Array;
}

/** The sending half: one per group, held by the sender. */
export class SenderKeyState {
  readonly #chainId: number;
  #chainKey: Uint8Array;
  #messageNumber: number;

  private constructor(chainId: number, chainKey: Uint8Array, messageNumber: number) {
    this.#chainId = chainId;
    this.#chainKey = chainKey;
    this.#messageNumber = messageNumber;
  }

  /** Starts a fresh chain with a random chain key from the platform CSPRNG. */
  static create(chainId: number): SenderKeyState {
    return new SenderKeyState(chainId, randomBytes(CHAIN_KEY_LEN), 0);
  }

  /** Which chain this state represents. */
  chainId(): number {
    return this.#chainId;
  }

  /** How many messages this chain has produced. */
  messageNumber(): number {
    return this.#messageNumber;
  }

  /**
   * True once the chain has reached its rotation bound.
   *
   * Callers check this and rotate. It is a bound on the blast radius of a compromise, so ignoring
   * it means the window is the lifetime of the group.
   */
  needsRotation(): boolean {
    return this.#messageNumber >= MAX_MESSAGES_PER_CHAIN;
  }

  /**
   * Builds the distribution message for the chain's current position.
   *
   * The chain key is copied into the distribution, not shared: this state's key advances with every
   * message, and a distribution that aliased it would silently change under the recipient.
   */
  distribution(identity: IdentitySecret): SenderKeyDistribution {
    return new SenderKeyDistribution(
      this.#chainId,
      this.#messageNumber,
      this.#chainKey.slice(),
      identity.public(),
    );
  }

  /** Encrypts and signs a group message. */
  encrypt(
    identity: IdentitySecret,
    groupContext: Uint8Array,
    plaintext: Uint8Array,
  ): SenderKeyMessage {
    if (this.needsRotation()) {
      // Refusing rather than silently continuing: the caller has a rotation path, and a chain that
      // runs past its bound is exactly the state the bound exists to prevent.
      throw CryptoError.keyAlreadyUsed();
    }
    const header = new SenderKeyHeader(this.#chainId, this.#messageNumber);
    const { key, nonce } = advanceChain(this.#chainKey);
    this.#messageNumber += 1;

    const aad = associatedData(groupContext, header);
    const sealed = aead.sealWithNonce(key, nonce, aad, plaintext);
    const ciphertext = sealed.slice(NONCE_LEN);

    // Sign header and ciphertext together, so neither can be moved onto the other. Group authorship
    // depends on this signature and nothing else.
    const signed = concat(aad, ciphertext);
    return { header, ciphertext, signature: identity.sign(GROUP_DOMAIN, signed) };
  }
}

/** The receiving half: one per (group, sender) pair. */
export class ReceiverKeyState {
  readonly #chainId: number;
  #chainKey: Uint8Array;
  #nextMessageNumber: number;
  readonly #identity: IdentityPublic;
  /** Keys derived for messages that have not arrived yet, oldest first. */
  readonly #skipped: Array<{ number: number; key: Uint8Array; nonce: Uint8Array }> = [];

  private constructor(
    chainId: number,
    chainKey: Uint8Array,
    nextMessageNumber: number,
    identity: IdentityPublic,
  ) {
    this.#chainId = chainId;
    this.#chainKey = chainKey;
    this.#nextMessageNumber = nextMessageNumber;
    this.#identity = identity;
  }

  /** Accepts a distribution message and starts tracking the sender's chain. */
  static accept(distribution: SenderKeyDistribution): ReceiverKeyState {
    return new ReceiverKeyState(
      distribution.chainId,
      distribution.exposeChainKey().slice(),
      distribution.messageNumber,
      distribution.identity,
    );
  }

  /** Which chain this state tracks. */
  chainId(): number {
    return this.#chainId;
  }

  /** How many out-of-order keys are retained. */
  skippedCount(): number {
    return this.#skipped.length;
  }

  /**
   * Verifies and decrypts a group message.
   *
   * The signature is checked *before* any key derivation. A forged message should cost the receiver
   * one signature verification, not a thousand KDF steps, and checking the cheap authentication
   * first is what makes that true.
   */
  decrypt(groupContext: Uint8Array, message: SenderKeyMessage): Uint8Array {
    if (message.header.chainId !== this.#chainId) {
      // A different chain means a rotation this receiver has not been told about. The caller
      // fetches the new distribution message and retries.
      throw CryptoError.noSession();
    }
    const aad = associatedData(groupContext, message.header);
    const signed = concat(aad, message.ciphertext);
    this.#identity.verify(GROUP_DOMAIN, signed, message.signature);

    const number = message.header.messageNumber;
    const index = this.#skipped.findIndex((entry) => entry.number === number);
    if (index !== -1) {
      const [entry] = this.#skipped.splice(index, 1);
      // `entry` is defined: `index` came from this array and nothing mutated it since.
      const found = entry as { number: number; key: Uint8Array; nonce: Uint8Array };
      const key = SymmetricKey.fromBytes(found.key);
      return aead.openWithNonce(key, found.nonce, aad, message.ciphertext);
    }
    if (number < this.#nextMessageNumber) {
      throw CryptoError.keyAlreadyUsed();
    }
    const gap = number - this.#nextMessageNumber;
    if (gap > MAX_CHAIN_GAP) {
      throw CryptoError.chainGapTooLarge();
    }

    const pending: Array<{ number: number; key: Uint8Array; nonce: Uint8Array }> = [];
    for (let offset = 0; offset < gap; offset += 1) {
      const { key, nonce } = advanceChain(this.#chainKey);
      pending.push({ number: this.#nextMessageNumber + offset, key: key.expose().slice(), nonce });
    }
    const { key, nonce } = advanceChain(this.#chainKey);
    const plaintext = aead.openWithNonce(key, nonce, aad, message.ciphertext);

    for (const entry of pending) {
      this.#skipped.push(entry);
    }
    // Bounded, oldest evicted first, for the same reason as the pairwise ratchet: a sender who
    // never fills the gaps must not grow this forever.
    while (this.#skipped.length > MAX_CHAIN_GAP) {
      this.#skipped.shift();
    }
    this.#nextMessageNumber = number + 1;
    return plaintext;
  }
}

/**
 * Group context and header, authenticated on every message.
 *
 * The group id is in here so a ciphertext cannot be lifted from one group and replayed into another
 * where the same sender is also a member.
 */
function associatedData(groupContext: Uint8Array, header: SenderKeyHeader): Uint8Array {
  return concat(groupContext, header.toBytes());
}

/**
 * Advances the chain and yields the message key and nonce.
 *
 * Mutates `chain` in place, as the pairwise ratchet's equivalent does: the previous chain key is
 * overwritten, which is the mechanism of forward secrecy.
 */
function advanceChain(chain: Uint8Array): { key: SymmetricKey; nonce: Uint8Array } {
  const { first: next, second: material } = kdf.derivePair(
    chain,
    null,
    kdf.LABEL_SENDER_CHAIN,
    32,
    56,
  );
  chain.set(next);
  next.fill(0);
  const key = material.slice(0, 32);
  const nonce = material.slice(32);
  material.fill(0);
  return { key: SymmetricKey.fromBytes(key), nonce };
}

/** Concatenates two byte strings into a fresh buffer. */
function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}
