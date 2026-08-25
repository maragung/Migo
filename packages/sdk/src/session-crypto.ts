/**
 * The end-to-end policy layer for 1:1 conversations.
 *
 * `@migo/crypto` owns the primitives — X3DH to agree a session secret, the Double Ratchet to turn
 * that one secret into a fresh key per message. This module owns the *policy* that a chat client
 * needs on top of them: one ratchet per remote device, when to run X3DH versus reuse a session, and
 * the exact bytes that go in the opaque `envelope` field of a `MessageSend` (section 11).
 *
 * # The cryptographic envelope (section 11)
 *
 * The server never reads this; it sees a byte string of some length and routes it. Both ends encode
 * it identically so a message sealed by a web client opens on an Android one. All fields are binary
 * with a fixed order and no field names — JSON is forbidden inside the envelope because it wastes a
 * few bytes on every message and leaks structure through length. The 1:1 layout is:
 *
 * ```text
 * u8      envelope_version           always ENVELOPE_VERSION for now
 * u8      scheme                     which of SCHEME_* below; decides the fields that follow
 * varint  sender_key_id              0 for 1:1 (the field exists for the group layout)
 * ── X3DH preamble, present only for SCHEME_DOUBLE_RATCHET_PREKEY ──
 * 64      initiator_identity         IdentityPublic.toBytes(); lets the responder run X3DH
 * 32      ephemeral_key              the initiator's X3DH ephemeral public key
 * varint  signed_prekey_id           which of the responder's signed prekeys was used
 * u8      has_one_time_prekey        1 if a one-time prekey was used, else 0
 * varint  one_time_prekey_id         present only when has_one_time_prekey is 1
 * ── Double Ratchet header + body ──
 * 32      ratchet_public_key         the sender's current ratchet public key
 * varint  message_counter            index within the sender's current chain
 * varint  previous_chain_length      messages sent in the sender's previous chain
 * bytes   ciphertext                 to the end; the trailing 16 bytes are the AEAD tag
 * ```
 *
 * Section 11 leaves the concrete `scheme` values to the implementation ("nilainya menyatakan Double
 * Ratchet 1-on-1 atau Sender Key untuk group"); {@link SCHEME_DOUBLE_RATCHET} and friends are that
 * assignment. The initial-message case is a distinct scheme rather than a flag because it changes
 * which fields are present, which is exactly the pattern section 11 uses for `ratchet_public_key`
 * ("hanya ada bila scheme memerlukan") and for the group layout's extra `group_key_epoch`.
 *
 * # What the tag authenticates
 *
 * The ratchet binds two things into every message's AEAD associated data: both device identities
 * (via the X3DH `associatedData`, `IK_initiator || IK_responder`) and the ratchet header. The
 * identity binding is the anti-UKS protection section 11 cares about most — the server cannot swap
 * the sender's identity or replay a session key into a conversation with a third party without the
 * tag failing. Section 11 additionally lists `conversation_id` and `message_id` among the metadata
 * it would bind; the ratchet in `@migo/crypto` (which mirrors `migo-crypto/src/ratchet.rs` step for
 * step) does not thread per-message associated data, and adding it on the TypeScript side alone
 * would desync from a Rust-side responder and make genuine messages undecryptable. Binding that
 * extra context is therefore a coordinated crypto-layer change, deferred rather than done half-way.
 *
 * # The inner plaintext is the caller's
 *
 * This module treats `plaintext` as opaque bytes. The section 11 inner layout — a `content_type`
 * byte, the MSE-encoded body, and optional fixed-bucket padding — is built one layer up by the
 * messaging domain, which is the layer that knows Text from MediaRef from VoiceNoteRef.
 */

import type { Id } from '@migo/wire';
import {
  initiate,
  respond,
  RatchetSession,
  RatchetHeader,
  IdentityPublic,
  IDENTITY_PUBLIC_LEN,
  PUBLIC_KEY_LEN,
} from '@migo/crypto';
import type { IdentitySecret, KeyPair, PrekeyBundle, InitialMessage } from '@migo/crypto';

import { EnvelopeReader, EnvelopeWriter } from './envelope-buffer.js';
import { SdkError } from './errors.js';

/** The only envelope version this build writes, and the only one it reads. */
export const ENVELOPE_VERSION = 1;

/** An established 1:1 Double Ratchet message — no X3DH preamble. */
export const SCHEME_DOUBLE_RATCHET = 1;
/** A 1:1 first message: the same ratchet body preceded by the X3DH material the peer needs. */
export const SCHEME_DOUBLE_RATCHET_PREKEY = 2;
/** A group (sender-key) message. Handled by the group layer, not this one — rejected here. */
export const SCHEME_SENDER_KEY = 3;

/**
 * The device-local private key material this layer needs to answer a first message.
 *
 * All three come from the keys domain, which holds what {@link KEY_PUBLISH} published the public
 * halves of. Private keys never leave the device, so this interface is the only way the crypto
 * policy reaches them, and a caller can back it with whatever secure storage its platform offers.
 */
export interface LocalKeyStore {
  /** This device's long-term identity secret. */
  identity(): IdentitySecret;
  /**
   * The key pair for one of our published signed prekeys, or `null` if that id is unknown.
   *
   * A first message names which signed prekey the initiator used; if we have rotated it away and
   * no longer hold the pair, the session cannot be answered.
   */
  signedPrekeyPair(signedPrekeyId: number): KeyPair | null;
  /**
   * The key pair for one of our published one-time prekeys *without* consuming it, or `null` if we
   * do not hold that id.
   *
   * Consumption is deliberately split from lookup ({@link consumeOneTimePrekey}) because every
   * content message on Migo is broadcast to the whole conversation — a first message the initiator
   * pairwise-sealed for one device still reaches every other device, which then tries and fails to
   * open it. If merely *attempting* a responder handshake consumed the prekey, those foreign
   * broadcasts would drain the pool and, worse, could exhaust it before the message that really is
   * for us arrives. So the 1:1 layer peeks here, attempts the decrypt, and consumes only on success.
   */
  oneTimePrekeyPair(keyId: number): KeyPair | null;
  /**
   * Permanently consumes a one-time prekey, called once a first message has actually opened.
   *
   * After this, {@link oneTimePrekeyPair} for the same id returns `null`, so a replayed first
   * message can never derive a second responder session from the same prekey — the forward-secrecy
   * property a one-time prekey exists to provide.
   */
  consumeOneTimePrekey(keyId: number): void;
}

/**
 * A source of peer prekey bundles, backed by a {@link KEY_BUNDLE_FETCH} round trip.
 *
 * Fetching is asynchronous and consumes one of the peer's one-time prekeys server-side, so it
 * happens exactly once per device — when this layer first needs to become the initiator.
 */
export interface PeerBundleSource {
  /** Fetches the bundle for one device of one user. Rejects if the server has none to serve. */
  fetchBundle(userId: Id, deviceId: Id): Promise<PrekeyBundle>;
}

/** A sealed message, ready to place in a `MessageSend`. */
export interface SealedEnvelope {
  /** The `scheme` byte the envelope carries, for the caller's diagnostics. */
  scheme: number;
  /**
   * The authoritative `sender_key_id`, which for 1:1 is always 0.
   *
   * The server accepts `sender_key_id` on `MessageSend` but never echoes it on `MessageEvent`,
   * because the binding copy lives inside the envelope (section, messaging deviations). A caller
   * may set the plaintext field from this on send; a receiver reads it from the opened envelope.
   */
  senderKeyId: number;
  /** The opaque envelope bytes. */
  envelope: Uint8Array;
}

/** One remote device's ratchet, plus the initiator state that governs the scheme we send. */
interface SessionEntry {
  session: RatchetSession;
  /**
   * The X3DH material to keep prepending, or `null` once the peer has replied.
   *
   * Set when we initiate. Until we successfully open a message from the peer we cannot know they
   * received our first message, so every message we send re-carries this material — the standard
   * "keep sending prekey messages until acknowledged" rule. Cleared on the first successful open.
   */
  pendingInit: InitialMessage | null;
}

/**
 * The per-device Double Ratchet store for 1:1 conversations.
 *
 * One instance per signed-in device, shared across every conversation. It keys sessions by
 * `(conversationId, remoteDeviceId)`: a direct conversation has one peer but that peer may have
 * several devices, and each device is a separate ratchet.
 */
export class SessionCrypto {
  readonly #keys: LocalKeyStore;
  readonly #bundles: PeerBundleSource;
  readonly #sessions = new Map<string, SessionEntry>();

  constructor(keys: LocalKeyStore, bundles: PeerBundleSource) {
    this.#keys = keys;
    this.#bundles = bundles;
  }

  /** Whether a session already exists for a conversation's remote device. */
  hasSession(conversationId: Id, deviceId: Id): boolean {
    return this.#sessions.has(sessionKey(conversationId, deviceId));
  }

  /**
   * Seals `plaintext` for one remote device, establishing a session first if there is none.
   *
   * The first message to a device fetches that device's prekey bundle, runs X3DH as the initiator,
   * and emits a {@link SCHEME_DOUBLE_RATCHET_PREKEY} envelope carrying the material the peer needs
   * to answer. Messages after that stay in prekey scheme until the peer replies, then switch to the
   * plain {@link SCHEME_DOUBLE_RATCHET} form.
   */
  async seal(
    conversationId: Id,
    peerUserId: Id,
    peerDeviceId: Id,
    plaintext: Uint8Array,
  ): Promise<SealedEnvelope> {
    const key = sessionKey(conversationId, peerDeviceId);
    let entry = this.#sessions.get(key);

    if (entry === undefined) {
      // Become the initiator: verify and consume a prekey bundle, then seed a ratchet from it.
      const bundle = await this.#bundles.fetchBundle(peerUserId, peerDeviceId);
      const initiation = initiate(this.#keys.identity(), bundle);
      const session = RatchetSession.initiator(initiation.seed, bundle.signedPrekey.publicKey);
      entry = { session, pendingInit: initiation.message };
      this.#sessions.set(key, entry);
    }

    const { header, ciphertext } = entry.session.encryptNext(plaintext);

    if (entry.pendingInit !== null) {
      const envelope = encodeEnvelope(
        SCHEME_DOUBLE_RATCHET_PREKEY,
        0,
        header,
        ciphertext,
        entry.pendingInit,
      );
      return { scheme: SCHEME_DOUBLE_RATCHET_PREKEY, senderKeyId: 0, envelope };
    }
    const envelope = encodeEnvelope(SCHEME_DOUBLE_RATCHET, 0, header, ciphertext);
    return { scheme: SCHEME_DOUBLE_RATCHET, senderKeyId: 0, envelope };
  }

  /**
   * Opens an envelope from one remote device, establishing a responder session if it is a first
   * message and none exists yet.
   *
   * A {@link SCHEME_DOUBLE_RATCHET_PREKEY} envelope with no existing session runs X3DH as the
   * responder. A prekey envelope for a device we already have a session with is a resend the
   * initiator made before hearing back; it decrypts in the session we already hold. A plain
   * {@link SCHEME_DOUBLE_RATCHET} envelope requires an existing session.
   *
   * # Commit only on success
   *
   * Establishing a responder session mutates two things that must not be spent on a message that is
   * not ours: the session slot for this sender device, and a one-time prekey. Because content is
   * broadcast to the whole conversation, a first message pairwise-sealed for another device arrives
   * here too, decodes as a well-formed prekey envelope, and would — if we committed eagerly — plant
   * a bogus session in this slot (so the real distribution could never open) and burn a prekey. So
   * the responder session is derived locally and the decrypt is attempted *before* anything is
   * stored: a foreign broadcast fails the AEAD tag, throws, and leaves the store untouched, exactly
   * as the ratchet's own anti-DoS rule leaves an established session untouched on a bad message.
   */
  open(conversationId: Id, senderUserId: Id, senderDeviceId: Id, envelope: Uint8Array): Uint8Array {
    void senderUserId; // Identity comes from the envelope's X3DH material, not the wire frame.
    const parsed = decodeEnvelope(envelope);
    const key = sessionKey(conversationId, senderDeviceId);
    const existing = this.#sessions.get(key);

    if (parsed.scheme === SCHEME_SENDER_KEY) {
      // Group messages are sealed with a sender-key ratchet handled by the group layer.
      throw new SdkError('session-crypto: sender-key envelope reached the 1:1 layer');
    }
    if (parsed.scheme !== SCHEME_DOUBLE_RATCHET && parsed.scheme !== SCHEME_DOUBLE_RATCHET_PREKEY) {
      throw new SdkError(`session-crypto: unknown envelope scheme ${parsed.scheme}`);
    }

    if (existing !== undefined) {
      // An established session: the ratchet guarantees it is not mutated if the decrypt fails, so a
      // resent prekey preamble or a foreign broadcast that lands on this slot cannot corrupt it.
      const plaintext = existing.session.decrypt(parsed.header, parsed.ciphertext);
      // We have now heard from the peer, so they hold a working session; stop re-sending X3DH.
      existing.pendingInit = null;
      return plaintext;
    }

    if (parsed.init === null) {
      // A ratchet message with no session and no X3DH material: the first message was lost, and
      // this one cannot bootstrap the session on its own. The peer must send a fresh prekey.
      throw new SdkError('session-crypto: no session for a non-prekey envelope');
    }

    // Derive a responder session but do not commit it: resolve the named prekeys, run X3DH, and
    // attempt the decrypt. Only a decrypt that passes the AEAD tag proves this message was sealed
    // for us rather than broadcast for another device.
    const derived = this.#deriveResponder(parsed.init);
    const plaintext = derived.session.decrypt(parsed.header, parsed.ciphertext);

    // Success: this message was ours. Now — and only now — consume the prekey and keep the session.
    if (derived.oneTimePrekeyId !== null) {
      this.#keys.consumeOneTimePrekey(derived.oneTimePrekeyId);
    }
    this.#sessions.set(key, { session: derived.session, pendingInit: null });
    return plaintext;
  }

  /**
   * Forgets sessions, so the next message re-runs X3DH.
   *
   * With `deviceId`, forgets that one device's session in the conversation. Without it, forgets
   * every device's session in the conversation. Use it when a peer's identity key changes, which
   * section 155 requires be surfaced as a visible warning rather than accepted silently — the old
   * session is against the old identity and must not be reused.
   */
  forget(conversationId: Id, deviceId?: Id): void {
    if (deviceId !== undefined) {
      this.#sessions.delete(sessionKey(conversationId, deviceId));
      return;
    }
    const prefix = `${conversationId}|`;
    for (const key of this.#sessions.keys()) {
      if (key.startsWith(prefix)) {
        this.#sessions.delete(key);
      }
    }
  }

  /**
   * Derives a responder session for a first message *without committing* — the caller attempts the
   * decrypt and, only if it succeeds, consumes the returned one-time prekey id and keeps the session.
   *
   * Returns the derived session and the one-time prekey id that must be consumed on success, or
   * `null` when the first message used no one-time prekey. Throws before touching any state when a
   * named prekey is unknown, which is the cheap rejection path for a broadcast meant for a device
   * whose prekey ids we do not share.
   */
  #deriveResponder(init: InitialMessage): {
    session: RatchetSession;
    oneTimePrekeyId: number | null;
  } {
    const signedPrekeyPair = this.#keys.signedPrekeyPair(init.signedPrekeyId);
    if (signedPrekeyPair === null) {
      throw new SdkError(`session-crypto: no signed prekey for id ${init.signedPrekeyId}`);
    }

    let oneTimePrekeyPair: KeyPair | null = null;
    if (init.oneTimePrekeyId !== null) {
      oneTimePrekeyPair = this.#keys.oneTimePrekeyPair(init.oneTimePrekeyId);
      if (oneTimePrekeyPair === null) {
        throw new SdkError(`session-crypto: no one-time prekey for id ${init.oneTimePrekeyId}`);
      }
    }

    const seed = respond(this.#keys.identity(), signedPrekeyPair, oneTimePrekeyPair, init);
    const session = RatchetSession.responder(seed, signedPrekeyPair);
    return { session, oneTimePrekeyId: oneTimePrekeyPair !== null ? init.oneTimePrekeyId : null };
  }
}

/** The map key for a session: conversation, then device. */
function sessionKey(conversationId: Id, deviceId: Id): string {
  return `${conversationId}|${deviceId}`;
}

/** Assembles the section 11 envelope from a ratchet output and, for a first message, X3DH material. */
function encodeEnvelope(
  scheme: number,
  senderKeyId: number,
  header: RatchetHeader,
  ciphertext: Uint8Array,
  init?: InitialMessage,
): Uint8Array {
  const writer = new EnvelopeWriter();
  writer.u8(ENVELOPE_VERSION);
  writer.u8(scheme);
  writer.varint(senderKeyId);

  if (scheme === SCHEME_DOUBLE_RATCHET_PREKEY) {
    if (init === undefined) {
      throw new SdkError('session-crypto: prekey scheme requires X3DH material');
    }
    writer.bytes(init.identity.toBytes());
    writer.bytes(init.ephemeralKey);
    writer.varint(init.signedPrekeyId);
    if (init.oneTimePrekeyId !== null) {
      writer.u8(1);
      writer.varint(init.oneTimePrekeyId);
    } else {
      writer.u8(0);
    }
  }

  writer.bytes(header.ratchetKey);
  writer.varint(header.messageNumber);
  writer.varint(header.previousChainLength);
  writer.bytes(ciphertext);
  return writer.finish();
}

/** The parsed shape of an envelope: its scheme, any X3DH material, and the ratchet message. */
interface ParsedEnvelope {
  scheme: number;
  senderKeyId: number;
  init: InitialMessage | null;
  header: RatchetHeader;
  ciphertext: Uint8Array;
}

/** Parses a section 11 envelope, rejecting a version or shape this build does not understand. */
function decodeEnvelope(bytes: Uint8Array): ParsedEnvelope {
  const reader = new EnvelopeReader(bytes);
  const version = reader.u8();
  if (version !== ENVELOPE_VERSION) {
    throw new SdkError(`session-crypto: unsupported envelope version ${version}`);
  }
  const scheme = reader.u8();
  const senderKeyId = reader.varint();

  let init: InitialMessage | null = null;
  if (scheme === SCHEME_DOUBLE_RATCHET_PREKEY) {
    const identity = IdentityPublic.parse(reader.take(IDENTITY_PUBLIC_LEN));
    const ephemeralKey = reader.take(PUBLIC_KEY_LEN);
    const signedPrekeyId = reader.varint();
    const hasOneTimePrekey = reader.u8();
    const oneTimePrekeyId = hasOneTimePrekey === 1 ? reader.varint() : null;
    init = { identity, ephemeralKey, signedPrekeyId, oneTimePrekeyId };
  }

  const ratchetKey = reader.take(PUBLIC_KEY_LEN);
  const messageNumber = reader.varint();
  const previousChainLength = reader.varint();
  const ciphertext = reader.rest();
  const header = new RatchetHeader(ratchetKey, previousChainLength, messageNumber);

  return { scheme, senderKeyId, init, header, ciphertext };
}
