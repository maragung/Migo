/**
 * Double Ratchet — per-message keys for a 1:1 conversation.
 *
 * X3DH produces one shared secret. If that secret encrypted every message, then stealing a phone
 * once would decrypt the entire conversation history, and every future message too. The Double
 * Ratchet turns that one secret into a fresh key per message, with two properties that matter to a
 * real user:
 *
 * * **Forward secrecy** — a key compromised today does not decrypt yesterday's messages, because
 *   yesterday's keys were deleted after use.
 * * **Post-compromise security** — once the attacker loses access, the ratchet heals. New
 *   Diffie-Hellman material from the other side is mixed in on every turn of the conversation, and
 *   the attacker cannot follow.
 *
 * Two ratchets combine:
 *
 * 1. **The DH ratchet.** Each side attaches a fresh public key to its messages. When a new one
 *    arrives, both root keys advance by mixing in a new DH output. This is what heals a compromise,
 *    and it only turns when the conversation turns — one side sending ten messages in a row does
 *    not advance it.
 * 2. **The symmetric chain.** Within one DH step, each message advances a chain key by a KDF. This
 *    is cheap and gives forward secrecy between consecutive messages without a round trip.
 *
 * # Out-of-order and lost messages
 *
 * Messages arrive out of order and get lost. Message 5 can arrive before message 3, so the
 * receiver derives and stores the keys it skipped. Two bounds keep that from becoming an attack:
 *
 * * {@link MAX_CHAIN_GAP} caps how far ahead one message may claim to be. Without it, a message
 *   numbered four billion makes the receiver derive four billion keys — a one-frame CPU exhaustion.
 * * {@link MAX_SKIPPED_KEYS} caps how many skipped keys are retained. Without it, a sender who
 *   sends only odd-numbered messages grows the receiver's state forever — a one-session memory leak.
 *
 * Reaching either bound loses messages, which is the correct trade: a lost message is visible and
 * recoverable, and an exhausted server is neither.
 *
 * A stored key is deleted the moment it is used. That is what makes a replayed frame fail rather
 * than deliver the same message twice.
 *
 * This mirrors `server/crates/migo-crypto/src/ratchet.rs` step for step — including where the chain
 * key is advanced in place versus on a local copy, which is the difference between a forged frame
 * that is merely rejected and one that corrupts the session.
 */

import { bytesToHex, equalBytes } from '@noble/ciphers/utils.js';

import { NONCE_LEN, SymmetricKey } from './aead.js';
import * as aead from './aead.js';
import { CryptoError } from './errors.js';
import { KeyPair, PUBLIC_KEY_LEN } from './identity.js';
import * as kdf from './kdf.js';
import type { SessionSeed } from './x3dh.js';

/** Maximum number of messages a single header may claim to have skipped. */
export const MAX_CHAIN_GAP = 2_000;

/** Maximum number of skipped message keys retained across a session. */
export const MAX_SKIPPED_KEYS = 2_000;

/** The 32-bit unsigned ceiling, for saturating arithmetic that mirrors Rust's `u32`. */
const U32_MAX = 0xffff_ffff;

/**
 * The plaintext header attached to every ratchet message.
 *
 * All three fields are public and all three are authenticated as associated data, so tampering
 * with them makes decryption fail rather than succeed differently.
 */
export class RatchetHeader {
  /** The sender's current ratchet public key. */
  readonly ratchetKey: Uint8Array;
  /**
   * How many messages the sender sent in its previous chain.
   *
   * Lets the receiver derive the keys it never saw from the *old* chain before moving to the new
   * one. Without it, a DH step would silently drop any message still in flight from the previous
   * chain.
   */
  readonly previousChainLength: number;
  /** Index of this message within the sender's current chain. */
  readonly messageNumber: number;

  constructor(ratchetKey: Uint8Array, previousChainLength: number, messageNumber: number) {
    this.ratchetKey = ratchetKey;
    this.previousChainLength = previousChainLength;
    this.messageNumber = messageNumber;
  }

  /** Encoded length: key, then two big-endian `u32`s. */
  static readonly ENCODED_LEN = PUBLIC_KEY_LEN + 8;

  /**
   * Serialises the header.
   *
   * Fixed-width big-endian rather than varints, deliberately. This byte string is authenticated,
   * so it must be canonical: a varint with two valid encodings would give one header two valid
   * authentication tags.
   */
  toBytes(): Uint8Array {
    const out = new Uint8Array(RatchetHeader.ENCODED_LEN);
    out.set(this.ratchetKey, 0);
    const view = new DataView(out.buffer);
    view.setUint32(PUBLIC_KEY_LEN, this.previousChainLength, false);
    view.setUint32(PUBLIC_KEY_LEN + 4, this.messageNumber, false);
    return out;
  }

  /** Parses a header, rejecting anything not exactly {@link ENCODED_LEN} bytes. */
  static parse(bytes: Uint8Array): RatchetHeader {
    if (bytes.length !== RatchetHeader.ENCODED_LEN) {
      throw CryptoError.malformedHeader();
    }
    const ratchetKey = bytes.slice(0, PUBLIC_KEY_LEN);
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const previousChainLength = view.getUint32(PUBLIC_KEY_LEN, false);
    const messageNumber = view.getUint32(PUBLIC_KEY_LEN + 4, false);
    return new RatchetHeader(ratchetKey, previousChainLength, messageNumber);
  }
}

/**
 * A message key and the nonce derived alongside it.
 *
 * The nonce comes from the KDF rather than the wire, so it is never transmitted and cannot be
 * tampered with. Each message key is used exactly once, so a derived nonce carries no reuse risk.
 */
interface MessageKey {
  readonly key: SymmetricKey;
  readonly nonce: Uint8Array;
}

/** Key material for one skipped message, awaiting late delivery. */
interface SkippedKey {
  readonly key: Uint8Array;
  readonly nonce: Uint8Array;
}

/**
 * A Double Ratchet session for one device pair.
 *
 * There is no clone: two copies of a ratchet would each advance independently and each believe it
 * had already used a key the other had not. In Rust the type is simply not `Clone`; here the
 * private fields and the absence of a copy path serve the same purpose.
 */
export class RatchetSession {
  #rootKey: Uint8Array;
  /** Our current ratchet pair. `null` for a responder that has not yet sent. */
  #sendingPair: KeyPair | null = null;
  /** The peer's latest ratchet key, once seen. */
  #receivingKey: Uint8Array | null = null;
  #sendingChain: Uint8Array | null = null;
  #receivingChain: Uint8Array | null = null;
  #sentCount = 0;
  #receivedCount = 0;
  #previousSendingCount = 0;
  /** Keys for skipped messages, keyed by `hex(ratchetKey):messageNumber`. */
  readonly #skipped = new Map<string, SkippedKey>();
  /** Insertion order of {@link skipped} keys, so the oldest is the one evicted. */
  readonly #skippedOrder: string[] = [];
  readonly #associatedData: Uint8Array;

  private constructor(sharedSecret: Uint8Array, associatedData: Uint8Array) {
    this.#rootKey = sharedSecret;
    this.#associatedData = associatedData;
  }

  /**
   * Starts a session as the initiator, who already knows the peer's prekey.
   *
   * The initiator can send immediately: it performs the first DH step against the peer's signed
   * prekey, which is exactly the key the peer published for this purpose. `pair` defaults to a
   * fresh one and is a parameter only so a test vector can pin it, exactly as
   * {@link x3dh.initiate}'s ephemeral is.
   */
  static initiator(
    seed: SessionSeed,
    peerSignedPrekey: Uint8Array,
    pair: KeyPair = KeyPair.generate(),
  ): RatchetSession {
    const session = new RatchetSession(seed.exposeSharedSecret(), seed.associatedData.slice());
    const dh = pair.diffieHellman(peerSignedPrekey);
    const { first: rootKey, second: chain } = kdf.derivePair(
      dh,
      session.#rootKey,
      kdf.LABEL_RATCHET_ROOT,
      32,
      32,
    );
    dh.fill(0);
    session.#rootKey.fill(0);
    session.#rootKey = rootKey;
    session.#sendingChain = chain;
    session.#sendingPair = pair;
    session.#receivingKey = peerSignedPrekey.slice();
    return session;
  }

  /**
   * Starts a session as the responder, whose signed prekey pair is the first ratchet key.
   *
   * The responder cannot send until it has received, because until then it has no peer ratchet key
   * to step against. That is not a limitation in practice: the responder is by definition the side
   * that received the first message.
   */
  static responder(seed: SessionSeed, signedPrekeyPair: KeyPair): RatchetSession {
    const session = new RatchetSession(seed.exposeSharedSecret(), seed.associatedData.slice());
    session.#sendingPair = signedPrekeyPair;
    return session;
  }

  /** Number of messages sent in the current chain. */
  sentCount(): number {
    return this.#sentCount;
  }

  /** Number of messages received in the current chain. */
  receivedCount(): number {
    return this.#receivedCount;
  }

  /** How many skipped keys are currently retained. */
  skippedCount(): number {
    return this.#skipped.size;
  }

  /**
   * Encrypts `plaintext`, returning the header and the ciphertext.
   *
   * The ciphertext has no nonce prefix: the nonce is derived from the message key, which the
   * receiver reconstructs from the header.
   */
  encrypt(plaintext: Uint8Array): { header: RatchetHeader; ciphertext: Uint8Array } {
    const pair = this.#sendingPair;
    const chain = this.#sendingChain;
    if (pair === null || chain === null) {
      throw CryptoError.noSession();
    }

    const messageKey = advanceChain(chain);
    const header = new RatchetHeader(pair.public(), this.#previousSendingCount, this.#sentCount);
    this.#sentCount += 1;

    const aad = concat(this.#associatedData, header.toBytes());
    const sealed = aead.sealWithNonce(messageKey.key, messageKey.nonce, aad, plaintext);
    // `sealWithNonce` prefixes the nonce; the receiver derives it, so drop it.
    return { header, ciphertext: sealed.slice(NONCE_LEN) };
  }

  /**
   * Decrypts a message.
   *
   * Advances the ratchet only when decryption succeeds. A forged message that claimed a new ratchet
   * key would otherwise destroy the session's ability to decrypt genuine ones — a denial of service
   * from anyone who can inject a frame.
   */
  decrypt(header: RatchetHeader, ciphertext: Uint8Array): Uint8Array {
    const aad = concat(this.#associatedData, header.toBytes());

    // A late message whose key was already derived and set aside.
    const mapKey = skippedMapKey(header.ratchetKey, header.messageNumber);
    const skipped = this.#skipped.get(mapKey);
    if (skipped !== undefined) {
      this.#skipped.delete(mapKey);
      this.#forgetSkippedOrder(mapKey);
      const key = SymmetricKey.fromBytes(skipped.key);
      return aead.openWithNonce(key, skipped.nonce, aad, ciphertext);
    }

    const isNewChain =
      this.#receivingKey === null || !equalBytes(this.#receivingKey, header.ratchetKey);
    if (isNewChain) {
      return this.#stepReceivingChain(header, aad, ciphertext);
    }
    return this.#decryptInCurrentChain(header, aad, ciphertext);
  }

  /** Handles a message that belongs to the chain we are already tracking. */
  #decryptInCurrentChain(
    header: RatchetHeader,
    aad: Uint8Array,
    ciphertext: Uint8Array,
  ): Uint8Array {
    if (header.messageNumber < this.#receivedCount) {
      // Already consumed. The key was deleted on use, so this is either a replay or a duplicate
      // delivery; either way there is nothing to do.
      throw CryptoError.keyAlreadyUsed();
    }
    const gap = header.messageNumber - this.#receivedCount;
    if (gap > MAX_CHAIN_GAP) {
      throw CryptoError.chainGapTooLarge();
    }
    const chain = this.#receivingChain;
    if (chain === null) {
      throw CryptoError.noSession();
    }

    // Derive and stash the keys for anything skipped, then the key we want.
    const pending: Array<{ number: number; key: MessageKey }> = [];
    for (let offset = 0; offset < gap; offset += 1) {
      const key = advanceChain(chain);
      pending.push({ number: this.#receivedCount + offset, key });
    }
    const target = advanceChain(chain);
    const plaintext = aead.openWithNonce(target.key, target.nonce, aad, ciphertext);

    // Only now, once the message is proven genuine, mutate session state.
    for (const { number, key } of pending) {
      this.#stashSkipped(header.ratchetKey, number, key);
    }
    this.#receivedCount = header.messageNumber + 1;
    return plaintext;
  }

  /** Handles the first message of a new chain: turn the DH ratchet. */
  #stepReceivingChain(header: RatchetHeader, aad: Uint8Array, ciphertext: Uint8Array): Uint8Array {
    if (
      header.messageNumber > MAX_CHAIN_GAP ||
      header.previousChainLength > saturatingAdd(MAX_CHAIN_GAP, this.#receivedCount)
    ) {
      throw CryptoError.chainGapTooLarge();
    }
    const pair = this.#sendingPair;
    if (pair === null) {
      throw CryptoError.noSession();
    }

    // Finish the previous chain, so messages still in flight from it can be decrypted when they
    // arrive. This advances the *old* receiving chain in place; the new chain below is local until
    // it is proven, so a forged frame cannot corrupt what we already track.
    const leftovers: Array<{ ratchetKey: Uint8Array; number: number; key: MessageKey }> = [];
    if (this.#receivingChain !== null && this.#receivingKey !== null) {
      const chain = this.#receivingChain;
      const previousKey = this.#receivingKey;
      const remaining = saturatingSub(header.previousChainLength, this.#receivedCount);
      if (remaining > MAX_CHAIN_GAP) {
        throw CryptoError.chainGapTooLarge();
      }
      for (let offset = 0; offset < remaining; offset += 1) {
        leftovers.push({
          ratchetKey: previousKey,
          number: this.#receivedCount + offset,
          key: advanceChain(chain),
        });
      }
    }

    // Turn the DH ratchet: mix the peer's new key into the root key. `receivingChain` is a local
    // copy — nothing on `this` is touched until the target message opens.
    const dh = pair.diffieHellman(header.ratchetKey);
    const { first: rootAfterReceive, second: receivingChain } = kdf.derivePair(
      dh,
      this.#rootKey,
      kdf.LABEL_RATCHET_ROOT,
      32,
      32,
    );
    dh.fill(0);

    // Derive the keys this new chain skipped, then the one we want.
    const pending: Array<{ number: number; key: MessageKey }> = [];
    for (let number = 0; number < header.messageNumber; number += 1) {
      pending.push({ number, key: advanceChain(receivingChain) });
    }
    const target = advanceChain(receivingChain);
    const plaintext = aead.openWithNonce(target.key, target.nonce, aad, ciphertext);

    // Proven genuine: commit. Our own next chain steps too, with a fresh pair, which is what makes
    // the ratchet heal after a compromise.
    for (const { ratchetKey, number, key } of leftovers) {
      this.#stashSkipped(ratchetKey, number, key);
    }
    for (const { number, key } of pending) {
      this.#stashSkipped(header.ratchetKey, number, key);
    }
    this.#rootKey.fill(0);
    this.#rootKey = rootAfterReceive;
    this.#receivingChain = receivingChain;
    this.#receivingKey = header.ratchetKey.slice();
    this.#receivedCount = header.messageNumber + 1;
    this.#previousSendingCount = this.#sentCount;
    this.#sentCount = 0;
    // The sending chain is left unset: it is derived lazily on the next send, against a pair
    // generated then, so a session that only receives never generates keys it does not use.
    this.#sendingChain = null;
    return plaintext;
  }

  /**
   * Prepares the sending chain if a receive has invalidated it.
   *
   * Called before encrypting. Separated from {@link encrypt} so that the send path is not forced to
   * touch the random source on every message: the pair is only generated when the ratchet actually
   * needs to turn.
   */
  prepareSend(): void {
    if (this.#sendingChain !== null) {
      return;
    }
    const peerKey = this.#receivingKey;
    if (peerKey === null) {
      throw CryptoError.noSession();
    }
    const pair = KeyPair.generate();
    const dh = pair.diffieHellman(peerKey);
    const { first: rootKey, second: chain } = kdf.derivePair(
      dh,
      this.#rootKey,
      kdf.LABEL_RATCHET_ROOT,
      32,
      32,
    );
    dh.fill(0);
    this.#rootKey.fill(0);
    this.#rootKey = rootKey;
    this.#sendingChain = chain;
    this.#sendingPair = pair;
  }

  /** Encrypts, turning the DH ratchet first if the last operation was a receive. */
  encryptNext(plaintext: Uint8Array): { header: RatchetHeader; ciphertext: Uint8Array } {
    this.prepareSend();
    return this.encrypt(plaintext);
  }

  /** Stores a skipped key, evicting the oldest once the bound is reached. */
  #stashSkipped(ratchetKey: Uint8Array, number: number, key: MessageKey): void {
    while (this.#skipped.size >= MAX_SKIPPED_KEYS) {
      // Oldest first: a message that has been missing longest is the least likely to still arrive.
      const oldest = this.#skippedOrder[0];
      if (oldest === undefined) {
        break;
      }
      this.#skippedOrder.shift();
      this.#skipped.delete(oldest);
    }
    const mapKey = skippedMapKey(ratchetKey, number);
    const existed = this.#skipped.has(mapKey);
    this.#skipped.set(mapKey, { key: key.key.expose().slice(), nonce: key.nonce });
    if (!existed) {
      this.#skippedOrder.push(mapKey);
    }
  }

  /** Removes one key from the insertion-order list. */
  #forgetSkippedOrder(mapKey: string): void {
    const index = this.#skippedOrder.indexOf(mapKey);
    if (index !== -1) {
      this.#skippedOrder.splice(index, 1);
    }
  }

  /** `RatchetSession(sent: N, received: M, skipped: K)`. Never a key. */
  toString(): string {
    return `RatchetSession(sent: ${this.#sentCount}, received: ${this.#receivedCount}, skipped: ${this.#skipped.size})`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}

/**
 * Advances a chain key one step and returns the message key it yields.
 *
 * Two separate derivations from the same chain key: the next chain key, and the message key plus
 * nonce. The chain key is overwritten in place, so the previous value is gone — that is the
 * mechanism of forward secrecy, and it is why this takes the buffer and mutates it rather than
 * returning a new one. Callers rely on the mutation: the session holds the same buffer.
 */
function advanceChain(chain: Uint8Array): MessageKey {
  const { first: nextChain, second: material } = kdf.derivePair(
    chain,
    null,
    kdf.LABEL_RATCHET_CHAIN,
    32,
    56,
  );
  chain.set(nextChain);
  nextChain.fill(0);

  const key = material.slice(0, 32);
  const nonce = material.slice(32);
  material.fill(0);
  return { key: SymmetricKey.fromBytes(key), nonce };
}

/** The map key for a skipped message: the ratchet key in hex, then its number. */
function skippedMapKey(ratchetKey: Uint8Array, number: number): string {
  return `${bytesToHex(ratchetKey)}:${number}`;
}

/** `min(a + b, u32::MAX)`, matching Rust's `u32::saturating_add`. */
function saturatingAdd(a: number, b: number): number {
  return Math.min(a + b, U32_MAX);
}

/** `max(a - b, 0)`, matching Rust's `u32::saturating_sub`. */
function saturatingSub(a: number, b: number): number {
  return Math.max(a - b, 0);
}

/** Concatenates two byte strings into a fresh buffer. */
function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}
