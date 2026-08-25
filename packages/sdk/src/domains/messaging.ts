/**
 * The messaging domain: send and receive end-to-end encrypted messages in a conversation.
 *
 * This is where the two crypto layers are wired to the wire protocol. Sending a message is two
 * steps — make sure every device that will receive the broadcast holds our current sender key, then
 * seal the content once and send it — and this domain owns the orchestration of both.
 *
 * # Why a message send is two protocol messages (sometimes more)
 *
 * The server fans a `MessageSend` out to every device in the conversation except the sending one
 * ({@link file://../../../server/crates/migo-messaging/src/fanout.rs}), and it does so with a single
 * shared `envelope` — it cannot seal a different ciphertext per recipient. So content is sealed once
 * under a *sender key* ({@link GroupCrypto}) that every recipient device must already hold. Handing a
 * device that key is a separate, pairwise-encrypted message: a {@link MessageKind.KeyExchange} whose
 * body is the sender-key distribution, sealed for that one device through the Double Ratchet
 * ({@link SessionCrypto}). The first message to a fresh conversation therefore expands to one
 * KeyExchange per recipient device plus the content itself; steady-state sends are just the content,
 * because {@link GroupCrypto.needsDistribution} reports everyone already has the key.
 *
 * # Why receiving tolerates disorder
 *
 * Those KeyExchange messages are broadcast like everything else, so a device receives distributions
 * sealed for *other* devices too; each fails to open and is dropped as expected fan-out noise. And a
 * content message can arrive before the distribution that unlocks it — reordered by the network, or
 * sent by someone whose sender key we have not yet been handed. Such a message is buffered per sender
 * device and retried the moment that sender's distribution lands, so ordinary reordering never
 * surfaces as a decryption failure. The buffer is bounded, so a genuinely undecryptable message
 * cannot grow it without limit.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  MessageKind,
  encodeMessageSend,
  decodeMessageAccepted,
  decodeMessageEvent,
  encodeMessageDelete,
  encodeMessageReceipt,
  decodeMessageReceipt,
} from '@migo/protocol';
import type {
  MessageAccepted,
  MessageEvent,
  MessageReceipt,
  MessageSend,
  MessageDelete,
  ReceiptKind,
} from '@migo/protocol';

import { ContentType, encodeContent, decodeContent } from '../content.js';
import type { ContentEncodeOptions, ControlEventContent, MessageContent } from '../content.js';
import { newId } from '../ids.js';
import type { GroupCrypto } from '../group-crypto.js';
import type { SessionCrypto } from '../session-crypto.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * The control-event name that carries a sender-key distribution.
 *
 * It rides in a {@link ContentType.ControlEvent} sealed through the 1:1 channel, so the string is a
 * client-to-client constant — the server never sees it — and only this exact event is treated as key
 * material on receipt.
 */
const SENDER_KEY_EVENT = 'sender-key';

/** How many undecryptable messages we hold per sender device before dropping the oldest. */
const MAX_PENDING_PER_SENDER = 64;

/** One device to seal a pairwise distribution for: which user owns it, and which device it is. */
export interface DeviceAddress {
  userId: Id;
  deviceId: Id;
}

/**
 * How the messaging domain learns which devices to distribute a sender key to.
 *
 * The client backs this — it is the layer that knows the conversation's membership and this device's
 * own identity. The returned set is every device that will receive a broadcast to the conversation
 * *except our own sending device*, which mirrors the server's fan-out exactly: our other devices are
 * included (multi-device sync), the sending device is not (it already has what it sent).
 */
export interface DeviceDirectory {
  /** The devices a sender key must reach for this conversation, excluding our own sending device. */
  recipientDevices(conversationId: Id): Promise<DeviceAddress[]>;
}

/** A decrypted inbound message handed to the application. */
export interface IncomingMessage {
  messageId: Id;
  conversationId: Id;
  seq: number;
  senderId: Id;
  senderDevice: Id;
  content: MessageContent;
  createdAt: number;
  replyTo?: Id;
  editedAt?: number;
}

/** Notification that a message was deleted (a tombstone the server broadcast). */
export interface MessageDeletion {
  messageId: Id;
  conversationId: Id;
  seq: number;
  senderId: Id;
  senderDevice: Id;
  createdAt: number;
}

/** Extra send parameters layered on top of the content-padding options. */
export interface SendOptions extends ContentEncodeOptions {
  /** The message this one replies to, surfaced by the server as a threading hint. */
  replyTo?: Id;
  /** A disappearing-message lifetime in milliseconds, after which the server expires the message. */
  expiresInMs?: number;
}

/** A handler that can be unsubscribed by calling the returned function. */
type Listener<T> = (value: T) => void;

/**
 * Send and receive encrypted messages for the signed-in device.
 *
 * One instance per client. It does not subscribe until {@link start} is called, so the client can
 * register application handlers first and not miss the first delivered event.
 */
export class MessagingDomain {
  readonly #rpc: Rpc;
  readonly #sessionCrypto: SessionCrypto;
  readonly #groupCrypto: GroupCrypto;
  readonly #directory: DeviceDirectory;
  readonly #onEventError: EventErrorHandler | undefined;

  readonly #messageListeners = new Set<Listener<IncomingMessage>>();
  readonly #deletionListeners = new Set<Listener<MessageDeletion>>();
  readonly #receiptListeners = new Set<Listener<MessageReceipt>>();

  /** Messages we could not open yet, keyed by `${conversationId}|${senderDevice}`. */
  readonly #pending = new Map<string, MessageEvent[]>();

  #unsubscribes: Array<() => void> = [];

  constructor(
    rpc: Rpc,
    sessionCrypto: SessionCrypto,
    groupCrypto: GroupCrypto,
    directory: DeviceDirectory,
    onEventError?: EventErrorHandler,
  ) {
    this.#rpc = rpc;
    this.#sessionCrypto = sessionCrypto;
    this.#groupCrypto = groupCrypto;
    this.#directory = directory;
    this.#onEventError = onEventError;
  }

  /** Begins delivering inbound messages and receipts. Idempotent. */
  start(): void {
    if (this.#unsubscribes.length > 0) {
      return;
    }
    this.#unsubscribes.push(
      this.#rpc.on(OP.MESSAGE_EVENT, decodeMessageEvent, (event) => this.#onMessageEvent(event)),
      this.#rpc.on(OP.MESSAGE_RECEIPT, decodeMessageReceipt, (receipt) =>
        this.#deliver(this.#receiptListeners, receipt),
      ),
    );
  }

  /** Stops delivering events. The crypto state and pending buffers are kept. */
  stop(): void {
    for (const unsubscribe of this.#unsubscribes) {
      unsubscribe();
    }
    this.#unsubscribes = [];
  }

  /** Registers a handler for decrypted inbound messages. Returns an unsubscribe function. */
  onMessage(handler: Listener<IncomingMessage>): () => void {
    this.#messageListeners.add(handler);
    return () => this.#messageListeners.delete(handler);
  }

  /** Registers a handler for message deletions. Returns an unsubscribe function. */
  onDeletion(handler: Listener<MessageDeletion>): () => void {
    this.#deletionListeners.add(handler);
    return () => this.#deletionListeners.delete(handler);
  }

  /** Registers a handler for delivery and read receipts. Returns an unsubscribe function. */
  onReceipt(handler: Listener<MessageReceipt>): () => void {
    this.#receiptListeners.add(handler);
    return () => this.#receiptListeners.delete(handler);
  }

  /**
   * Feeds a message event through the same decryption and routing path as a live delivery.
   *
   * The sync domain replays fetched history through here, so catching up on missed messages applies
   * exactly the live rules: a historical {@link MessageKind.KeyExchange} rebuilds the sender's session,
   * a content message opens under the sender key or is buffered until its distribution replays, and a
   * tombstone surfaces as a deletion. Replaying a page in the order the server returned it preserves
   * the "distribution before content" ordering the buffering relies on. Idempotent decryption is the
   * caller's concern: a message already seen by a live event and then re-seen from sync will attempt to
   * decrypt twice, which the ratchet's replay protection rejects, so the caller de-duplicates by seq.
   */
  ingest(event: MessageEvent): void {
    this.#onMessageEvent(event);
  }

  /**
   * Sends a message to a conversation.
   *
   * Distributes the current sender key to any recipient device that lacks it, then seals the content
   * once and sends it. Resolves with the server's acknowledgement, which carries the assigned
   * sequence number and whether the message was a duplicate.
   */
  async send(
    conversationId: Id,
    content: MessageContent,
    options: SendOptions = {},
  ): Promise<MessageAccepted> {
    await this.#distribute(conversationId);

    const plaintext = encodeContent(content, options);
    const sealed = this.#groupCrypto.sealContent(conversationId, plaintext);

    const send: MessageSend = {
      messageId: newId(),
      conversationId,
      kind: kindForContent(content.type),
      envelope: sealed.envelope,
      senderKeyId: sealed.senderKeyId,
    };
    if (options.replyTo !== undefined) {
      send.replyTo = options.replyTo;
    }
    if (options.expiresInMs !== undefined) {
      send.expiresInMs = options.expiresInMs;
    }
    return this.#rpc.call(OP.MESSAGE_SEND, encodeMessageSend, decodeMessageAccepted, send);
  }

  /**
   * Deletes a message, for ourselves or for everyone.
   *
   * A delete-for-everyone reaches every other device as a tombstone {@link MessageEvent}; a
   * delete-for-me is not broadcast. Resolves with the server's acknowledgement.
   */
  async deleteMessage(
    conversationId: Id,
    messageId: Id,
    forEveryone: boolean,
  ): Promise<MessageAccepted> {
    const request: MessageDelete = { messageId, conversationId, forEveryone };
    return this.#rpc.call(OP.MESSAGE_DELETE, encodeMessageDelete, decodeMessageAccepted, request);
  }

  /**
   * Sends a delivery or read receipt up to a sequence number.
   *
   * Receipts are fire-and-forget: the protocol defines no reply, and a lost receipt is corrected by
   * the next one, which carries a watermark rather than a single-message acknowledgement.
   */
  async sendReceipt(conversationId: Id, kind: ReceiptKind, seq: number): Promise<void> {
    const receipt: MessageReceipt = { conversationId, kind, seq };
    await this.#rpc.notify(OP.MESSAGE_RECEIPT, encodeMessageReceipt, receipt);
  }

  /**
   * Rotates the sender key for a conversation, so a departed member's key can no longer read new
   * messages. The next {@link send} re-distributes the fresh key to every remaining device.
   */
  rotateSenderKey(conversationId: Id): void {
    this.#groupCrypto.rotate(conversationId);
  }

  /**
   * Forgets crypto state for a conversation, or for one device within it.
   *
   * Use it when leaving a conversation, or when a peer's identity key changes and the sessions built
   * on the old identity must not be reused (section 155). With `deviceId`, only that device's inbound
   * state is dropped; without it, both our outbound sender key and every inbound session are dropped.
   */
  forget(conversationId: Id, deviceId?: Id): void {
    this.#sessionCrypto.forget(conversationId, deviceId);
    this.#groupCrypto.forget(conversationId, deviceId);
  }

  /** Sends the current sender key to every recipient device that does not already hold it. */
  async #distribute(conversationId: Id): Promise<void> {
    const devices = await this.#directory.recipientDevices(conversationId);
    for (const device of devices) {
      if (!this.#groupCrypto.needsDistribution(conversationId, device.deviceId)) {
        continue;
      }
      const control: ControlEventContent = {
        type: ContentType.ControlEvent,
        event: SENDER_KEY_EVENT,
        data: this.#groupCrypto.distributionFor(conversationId),
      };
      const sealed = await this.#sessionCrypto.seal(
        conversationId,
        device.userId,
        device.deviceId,
        encodeContent(control),
      );
      const send: MessageSend = {
        messageId: newId(),
        conversationId,
        kind: MessageKind.KeyExchange,
        envelope: sealed.envelope,
        senderKeyId: sealed.senderKeyId,
      };
      await this.#rpc.call(OP.MESSAGE_SEND, encodeMessageSend, decodeMessageAccepted, send);
      this.#groupCrypto.markDistributed(conversationId, device.deviceId);
    }
  }

  /** Routes one inbound message event by kind. */
  #onMessageEvent(event: MessageEvent): void {
    if (event.deleted === true) {
      this.#deliver(this.#deletionListeners, {
        messageId: event.messageId,
        conversationId: event.conversationId,
        seq: event.seq,
        senderId: event.senderId,
        senderDevice: event.senderDevice,
        createdAt: event.createdAt,
      });
      return;
    }
    if (event.kind === MessageKind.KeyExchange) {
      this.#onKeyExchange(event);
      return;
    }
    this.#onContent(event);
  }

  /**
   * Handles a KeyExchange message: our sender-key distribution, or fan-out noise sealed for another
   * device.
   *
   * A distribution sealed for a different device reaches us too and cannot open — that throws and is
   * dropped silently, because it is expected, not an error. A distribution that does open is adopted
   * and the sender's pending messages are drained.
   */
  #onKeyExchange(event: MessageEvent): void {
    let plaintext: Uint8Array;
    try {
      plaintext = this.#sessionCrypto.open(
        event.conversationId,
        event.senderId,
        event.senderDevice,
        event.envelope,
      );
    } catch {
      // Broadcast to us but pairwise-sealed for another device; expected, not surfaced.
      return;
    }

    let content: MessageContent;
    try {
      content = decodeContent(plaintext);
    } catch (cause) {
      this.#onEventError?.(OP.MESSAGE_EVENT, cause);
      return;
    }

    if (
      content.type !== ContentType.ControlEvent ||
      content.event !== SENDER_KEY_EVENT ||
      content.data === undefined
    ) {
      // A control event over the 1:1 channel that is not a sender-key distribution; nothing to do.
      return;
    }
    this.#groupCrypto.acceptDistribution(event.conversationId, event.senderDevice, content.data);
    this.#drainPending(event.conversationId, event.senderDevice);
  }

  /** Handles a content message: open it under the sender key, or buffer it until the key arrives. */
  #onContent(event: MessageEvent): void {
    if (!this.#groupCrypto.hasReceiver(event.conversationId, event.senderDevice)) {
      // We have not been handed this sender's key yet; hold the message for when we are.
      this.#buffer(event);
      return;
    }
    let plaintext: Uint8Array;
    try {
      plaintext = this.#groupCrypto.open(event.conversationId, event.senderDevice, event.envelope);
    } catch {
      // We hold a key but this message did not open under it — most likely a rotation we have not
      // caught up to. Buffer it; a newer distribution will drain it, and the bound caps a bad one.
      this.#buffer(event);
      return;
    }
    this.#emitContent(event, plaintext);
  }

  /** Decodes an opened plaintext and delivers it, or reports a malformed body. */
  #emitContent(event: MessageEvent, plaintext: Uint8Array): void {
    let content: MessageContent;
    try {
      content = decodeContent(plaintext);
    } catch (cause) {
      this.#onEventError?.(OP.MESSAGE_EVENT, cause);
      return;
    }
    const message: IncomingMessage = {
      messageId: event.messageId,
      conversationId: event.conversationId,
      seq: event.seq,
      senderId: event.senderId,
      senderDevice: event.senderDevice,
      content,
      createdAt: event.createdAt,
    };
    if (event.replyTo !== undefined) {
      message.replyTo = event.replyTo;
    }
    if (event.editedAt !== undefined) {
      message.editedAt = event.editedAt;
    }
    this.#deliver(this.#messageListeners, message);
  }

  /** Holds an undecryptable message, dropping the oldest once the per-sender bound is reached. */
  #buffer(event: MessageEvent): void {
    const key = pendingKey(event.conversationId, event.senderDevice);
    let list = this.#pending.get(key);
    if (list === undefined) {
      list = [];
      this.#pending.set(key, list);
    }
    list.push(event);
    if (list.length > MAX_PENDING_PER_SENDER) {
      list.shift();
    }
  }

  /** Retries every buffered message for a sender now that its key may have arrived. */
  #drainPending(conversationId: Id, senderDevice: Id): void {
    const key = pendingKey(conversationId, senderDevice);
    const list = this.#pending.get(key);
    if (list === undefined) {
      return;
    }
    const stillPending: MessageEvent[] = [];
    for (const event of list) {
      try {
        const plaintext = this.#groupCrypto.open(conversationId, senderDevice, event.envelope);
        this.#emitContent(event, plaintext);
      } catch {
        // Still not openable — a later distribution may yet unlock it; keep holding it.
        stillPending.push(event);
      }
    }
    if (stillPending.length > 0) {
      this.#pending.set(key, stillPending);
    } else {
      this.#pending.delete(key);
    }
  }

  /** Delivers a value to every listener, isolating a throw from one handler from the others. */
  #deliver<T>(listeners: Set<Listener<T>>, value: T): void {
    for (const listener of listeners) {
      try {
        listener(value);
      } catch (cause) {
        this.#onEventError?.(OP.MESSAGE_EVENT, cause);
      }
    }
  }
}

/** The buffer key for a sender's undecryptable messages. */
function pendingKey(conversationId: Id, senderDevice: Id): string {
  return `${conversationId}|${senderDevice}`;
}

/**
 * The cleartext {@link MessageKind} for a content type.
 *
 * The server routes and counts by this coarse kind, which travels in the clear on `MessageSend`; the
 * exact struct is the {@link ContentType} byte sealed inside the ciphertext. A reaction rides as a
 * text-kind message (it is user-authored conversation content), while a control event is System
 * (machinery, not a message a user wrote).
 */
function kindForContent(type: ContentType): MessageKind {
  switch (type) {
    case ContentType.Text:
      return MessageKind.Text;
    case ContentType.MediaRef:
      return MessageKind.Media;
    case ContentType.VoiceNoteRef:
      return MessageKind.Voice;
    case ContentType.Reaction:
      return MessageKind.Text;
    case ContentType.ControlEvent:
      return MessageKind.System;
    default: {
      const unreachable: never = type;
      return unreachable;
    }
  }
}
