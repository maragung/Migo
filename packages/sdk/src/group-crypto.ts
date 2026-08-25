/**
 * The end-to-end policy layer for broadcast conversations — which, on Migo, is every conversation.
 *
 * # Why sender-key, and why even for a direct chat
 *
 * The server fans one message out to a topic: a single sealed `envelope` reaches every device in the
 * conversation, including the sender's own other devices (`migo-messaging`'s `Fanout` excludes only
 * the sending device). One ciphertext therefore has to open on many devices at once, and a pairwise
 * Double Ratchet — one ciphertext per recipient device — cannot produce that. So content is sealed
 * once with a *sender key*: a symmetric chain the sender owns and distributes to each member device,
 * exactly the WhatsApp/Signal-group construction. This holds for a two-person "direct" chat too,
 * because that chat still has up to several devices per side and still fans out through one topic.
 *
 * The pairwise Double Ratchet ({@link file://./session-crypto.ts}) does not disappear — it becomes
 * the *distribution channel*. A sender hands each member device its {@link ContentType.ControlEvent}
 * sender-key distribution through that private per-device channel, and thereafter broadcasts content
 * under the sender key. This module owns the sender-key half; the messaging domain wires the two
 * together (seal content here, distribute the key via the 1:1 layer).
 *
 * # The envelope (section 11, group layout)
 *
 * A group message is the section 11 envelope with `scheme = SCHEME_SENDER_KEY` and the group's
 * `group_key_epoch`. There is no `ratchet_public_key` (the layout omits it "unless the scheme
 * requires it", and sender-key does not), and `previous_chain_length` is not meaningful for a
 * symmetric chain, so this scheme's concrete bytes — which section 11 leaves to the implementation —
 * are:
 *
 * ```text
 * u8      envelope_version       ENVELOPE_VERSION
 * u8      scheme                 SCHEME_SENDER_KEY
 * varint  sender_key_id          the chain id (which of the sender's chains this is)
 * varint  group_key_epoch        bumped whenever membership changes, so a removed member's key dies
 * varint  message_counter        index within the chain (the SenderKeyHeader's message number)
 * 64      signature              Ed25519 over header+ciphertext; proves which member wrote it
 * bytes   ciphertext             to the end; the trailing 16 bytes are the AEAD tag
 * ```
 *
 * The signature is what a symmetric chain cannot provide on its own: every member holds the chain
 * key, so without a per-message identity signature any member could forge another's message. The
 * conversation id is bound as the AEAD associated data (see {@link conversationContext}), so a
 * sealed message cannot be lifted into a different conversation.
 */

import type { Id } from '@migo/wire';
import {
  IdentityPublic,
  IDENTITY_PUBLIC_LEN,
  ReceiverKeyState,
  SenderKeyDistribution,
  SenderKeyHeader,
  SenderKeyState,
  SIGNATURE_LEN,
} from '@migo/crypto';
import type { IdentitySecret, SenderKeyMessage } from '@migo/crypto';

import { conversationContext } from './content.js';
import { EnvelopeReader, EnvelopeWriter } from './envelope-buffer.js';
import { SdkError } from './errors.js';
import { ENVELOPE_VERSION, SCHEME_SENDER_KEY } from './session-crypto.js';
import type { SealedEnvelope } from './session-crypto.js';

/** Bytes of a sender-key chain key, mirroring `CHAIN_KEY_LEN` in the crypto crate. */
const CHAIN_KEY_LEN = 32;

/**
 * The identity secret this layer signs with.
 *
 * Narrower than {@link LocalKeyStore} on purpose: the group layer only ever needs to sign its own
 * messages and build its own distributions, never to answer someone else's prekey. A `LocalKeyStore`
 * satisfies this structurally, so the client can pass the same object to both layers.
 */
export interface IdentityProvider {
  /** This device's long-term identity secret. */
  identity(): IdentitySecret;
}

/** One conversation's outbound sender key: the chain, its epoch, and who already has it. */
interface SendingEntry {
  state: SenderKeyState;
  /** The membership epoch this chain belongs to; travels in every message it seals. */
  epoch: number;
  /** Device ids that have received a distribution for the current chain, so resends are skipped. */
  distributed: Set<Id>;
}

/**
 * The per-conversation sender-key store.
 *
 * One instance per signed-in device. It holds one outbound chain per conversation (what this device
 * seals with) and one inbound {@link ReceiverKeyState} per remote sender device (what it opens their
 * messages with). Distribution — getting an outbound chain to the other devices, and accepting theirs
 * — is driven by the messaging domain through {@link distributionFor} and {@link acceptDistribution}.
 */
export class GroupCrypto {
  readonly #keys: IdentityProvider;
  readonly #sending = new Map<Id, SendingEntry>();
  readonly #receiving = new Map<string, ReceiverKeyState>();

  constructor(keys: IdentityProvider) {
    this.#keys = keys;
  }

  /** Whether this device already has an outbound chain for a conversation. */
  hasSenderKey(conversationId: Id): boolean {
    return this.#sending.has(conversationId);
  }

  /** The current membership epoch for a conversation's outbound chain, or 0 if there is none. */
  currentEpoch(conversationId: Id): number {
    return this.#sending.get(conversationId)?.epoch ?? 0;
  }

  /**
   * Seals a plaintext once for broadcast to the whole conversation.
   *
   * Establishes an outbound chain on first use. When the chain reaches its rotation bound the caller
   * should have rotated already ({@link needsRotation}); sealing past the bound is refused by the
   * crypto layer rather than silently continuing.
   */
  sealContent(conversationId: Id, plaintext: Uint8Array): SealedEnvelope {
    const entry = this.#ensureSending(conversationId);
    const context = conversationContext(conversationId);
    const message = entry.state.encrypt(this.#keys.identity(), context, plaintext);
    const envelope = encodeSenderKeyEnvelope(entry.epoch, message);
    return { scheme: SCHEME_SENDER_KEY, senderKeyId: message.header.chainId, envelope };
  }

  /**
   * The serialized sender-key distribution for a conversation's current chain.
   *
   * This is the payload the messaging domain seals into a {@link ContentType.ControlEvent} and sends
   * to one member device through the pairwise channel. It carries the chain key *as of now*, so a
   * member who receives it cannot read messages sealed before they were given the key.
   */
  distributionFor(conversationId: Id): Uint8Array {
    const entry = this.#ensureSending(conversationId);
    return serializeDistribution(entry.state.distribution(this.#keys.identity()));
  }

  /** Whether a member device still needs the current chain's distribution. */
  needsDistribution(conversationId: Id, deviceId: Id): boolean {
    const entry = this.#sending.get(conversationId);
    return entry === undefined || !entry.distributed.has(deviceId);
  }

  /** Records that a member device has received the current chain's distribution. */
  markDistributed(conversationId: Id, deviceId: Id): void {
    this.#ensureSending(conversationId).distributed.add(deviceId);
  }

  /** Whether the outbound chain has reached its rotation bound and must be rotated before more sends. */
  needsRotation(conversationId: Id): boolean {
    return this.#sending.get(conversationId)?.state.needsRotation() ?? false;
  }

  /**
   * Starts a fresh outbound chain and bumps the epoch.
   *
   * Called when membership changes (someone left, so the old chain must die) or when the current
   * chain hits its message bound. Every member must be re-sent the new distribution, so the
   * `distributed` set is cleared.
   */
  rotate(conversationId: Id): void {
    const previous = this.#sending.get(conversationId);
    const epoch = (previous?.epoch ?? 0) + 1;
    this.#sending.set(conversationId, {
      state: SenderKeyState.create(randomChainId()),
      epoch,
      distributed: new Set(),
    });
  }

  /**
   * Accepts a sender-key distribution from a remote device, so its future messages can be opened.
   *
   * A later distribution for the same sender replaces the earlier one: that is how a rotation is
   * adopted — the sender rotates, re-distributes, and this overwrites the stale receiver state.
   */
  acceptDistribution(conversationId: Id, senderDeviceId: Id, distributionBytes: Uint8Array): void {
    const distribution = parseDistribution(distributionBytes);
    this.#receiving.set(
      receiverKey(conversationId, senderDeviceId),
      ReceiverKeyState.accept(distribution),
    );
  }

  /** Whether a distribution has been accepted for a remote sender device. */
  hasReceiver(conversationId: Id, senderDeviceId: Id): boolean {
    return this.#receiving.has(receiverKey(conversationId, senderDeviceId));
  }

  /**
   * Opens a broadcast envelope from a remote device.
   *
   * Throws {@link SdkError} if no distribution has been accepted for the sender yet — the messaging
   * domain reacts by holding the message until the sender's distribution arrives. If the message
   * names a chain the receiver has not been told about (a rotation), the crypto layer throws and the
   * same "await the distribution" path applies.
   */
  open(conversationId: Id, senderDeviceId: Id, envelope: Uint8Array): Uint8Array {
    const parsed = decodeSenderKeyEnvelope(envelope);
    const receiver = this.#receiving.get(receiverKey(conversationId, senderDeviceId));
    if (receiver === undefined) {
      throw new SdkError(`group-crypto: no sender key for ${conversationId}|${senderDeviceId}`);
    }
    return receiver.decrypt(conversationContext(conversationId), parsed.message);
  }

  /**
   * Forgets sender-key state for a conversation.
   *
   * With `deviceId`, drops only that remote sender's inbound state. Without it, drops this device's
   * outbound chain and every inbound receiver for the conversation — used when leaving a conversation
   * or when a member's identity key change invalidates the trust the chain was built on.
   */
  forget(conversationId: Id, deviceId?: Id): void {
    if (deviceId !== undefined) {
      this.#receiving.delete(receiverKey(conversationId, deviceId));
      return;
    }
    this.#sending.delete(conversationId);
    const prefix = `${conversationId}|`;
    for (const key of this.#receiving.keys()) {
      if (key.startsWith(prefix)) {
        this.#receiving.delete(key);
      }
    }
  }

  /** Returns the outbound chain for a conversation, creating or rotating it as needed. */
  #ensureSending(conversationId: Id): SendingEntry {
    const existing = this.#sending.get(conversationId);
    if (existing === undefined) {
      const created: SendingEntry = {
        state: SenderKeyState.create(randomChainId()),
        epoch: 1,
        distributed: new Set(),
      };
      this.#sending.set(conversationId, created);
      return created;
    }
    if (existing.state.needsRotation()) {
      this.rotate(conversationId);
      // rotate() always sets an entry for this conversation, so the get() cannot be undefined.
      return this.#sending.get(conversationId) as SendingEntry;
    }
    return existing;
  }
}

/** The map key for an inbound receiver: conversation, then sender device. */
function receiverKey(conversationId: Id, senderDeviceId: Id): string {
  return `${conversationId}|${senderDeviceId}`;
}

/** A fresh random 32-bit chain id from the platform CSPRNG. */
function randomChainId(): number {
  const webcrypto = (globalThis as { crypto?: Crypto }).crypto;
  if (webcrypto?.getRandomValues === undefined) {
    throw new TypeError('no Web Crypto available to mint a chain id');
  }
  const out = new Uint32Array(1);
  webcrypto.getRandomValues(out);
  return out[0] as number;
}

/** Serialises a distribution: chain id, message number, the 32-byte chain key, the 64-byte identity. */
function serializeDistribution(distribution: SenderKeyDistribution): Uint8Array {
  const writer = new EnvelopeWriter();
  writer.varint(distribution.chainId);
  writer.varint(distribution.messageNumber);
  writer.bytes(distribution.exposeChainKey());
  writer.bytes(distribution.identity.toBytes());
  return writer.finish();
}

/** Parses a distribution written by {@link serializeDistribution}. */
function parseDistribution(bytes: Uint8Array): SenderKeyDistribution {
  const reader = new EnvelopeReader(bytes);
  const chainId = reader.varint();
  const messageNumber = reader.varint();
  const chainKey = reader.take(CHAIN_KEY_LEN);
  const identity = IdentityPublic.parse(reader.take(IDENTITY_PUBLIC_LEN));
  return new SenderKeyDistribution(chainId, messageNumber, chainKey, identity);
}

/** Assembles the section 11 group envelope from a sealed sender-key message. */
function encodeSenderKeyEnvelope(epoch: number, message: SenderKeyMessage): Uint8Array {
  const writer = new EnvelopeWriter();
  writer.u8(ENVELOPE_VERSION);
  writer.u8(SCHEME_SENDER_KEY);
  writer.varint(message.header.chainId);
  writer.varint(epoch);
  writer.varint(message.header.messageNumber);
  writer.bytes(message.signature);
  writer.bytes(message.ciphertext);
  return writer.finish();
}

/** The parsed shape of a group envelope. */
interface ParsedSenderKeyEnvelope {
  epoch: number;
  message: SenderKeyMessage;
}

/** Parses a section 11 group envelope, rejecting a version or scheme this build does not understand. */
function decodeSenderKeyEnvelope(bytes: Uint8Array): ParsedSenderKeyEnvelope {
  const reader = new EnvelopeReader(bytes);
  const version = reader.u8();
  if (version !== ENVELOPE_VERSION) {
    throw new SdkError(`group-crypto: unsupported envelope version ${version}`);
  }
  const scheme = reader.u8();
  if (scheme !== SCHEME_SENDER_KEY) {
    throw new SdkError(`group-crypto: not a sender-key envelope (scheme ${scheme})`);
  }
  const chainId = reader.varint();
  const epoch = reader.varint();
  const messageNumber = reader.varint();
  const signature = reader.take(SIGNATURE_LEN);
  const ciphertext = reader.rest();
  const header = new SenderKeyHeader(chainId, messageNumber);
  return { epoch, message: { header, ciphertext, signature } };
}
