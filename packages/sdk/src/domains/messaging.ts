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
 *
 * # Why a gap heals itself
 *
 * A conversation's sequence numbers are gapless by construction (§152), so a message that lands
 * above `watermark + 1` means pages this device never received — not reordering *within* the stream
 * but a hole in it. The watermark holds (§158: the reconnect comparison must name what is truly
 * held) and the domain asks the {@link GapFiller} — the client, wired around the sync domain — to
 * fetch exactly that hole and replay it through {@link ingest}. The ask is guarded so a persistent
 * hole cannot become a loop: one fill per conversation at a time, and a fill that could not move
 * the watermark is not re-asked on every later event. What the fill replays is deduplicated against
 * what the live stream already delivered above the hole, so a message that arrived live while its
 * page was being fetched surfaces once, not twice.
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
  encodeMessageEdit,
  encodeReactionSet,
  decodeAcknowledged,
  encodeGroupKeyDistribution,
  decodeGroupKeyDistribution,
} from '@migo/protocol';
import type {
  GroupKeyDistribution,
  MessageAccepted,
  MessageEvent,
  MessageReceipt,
  MessageSend,
  MessageDelete,
  MessageEdit,
  ReactionSet,
  ReceiptKind,
} from '@migo/protocol';

import { ContentType, encodeContent, decodeContent } from '../content.js';
import type { ContentEncodeOptions, ControlEventContent, MessageContent } from '../content.js';
import { newId } from '../ids.js';
import { SdkError } from '../errors.js';
import type { GroupCrypto } from '../group-crypto.js';
import type { SessionCrypto, SealedEnvelope } from '../session-crypto.js';
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

/**
 * How many above-the-watermark deliveries we remember per conversation before dropping the oldest.
 *
 * The guard exists only for the window a gap fill is in flight, so the bound is generous rather
 * than exact: beyond it, an ancient above-gap delivery may surface twice (the same trade the
 * pending buffer makes) rather than the set growing without limit under a hole the server never
 * fills.
 */
const MAX_AHEAD_REMEMBERED = 4096;

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

/**
 * How the messaging layer asks for the pages a detected gap is missing.
 *
 * The domain owns the watermark accounting but cannot fetch history — sync is another domain's
 * slice — so the composition root (the client) supplies this seam, exactly the way it supplies
 * {@link DeviceDirectory}. The contract is deliberately narrow: the filler pages the range in and
 * replays it through {@link MessagingDomain.ingest}; whether the watermark then moved is the
 * domain's own accounting, and a fill that could not move it is ended by resolving, never by
 * throwing twice.
 */
export interface GapFiller {
  /**
   * Fetches and replays the events missing above the conversation's watermark, up to `toSeq`.
   *
   * `toSeq` is the highest sequence the domain has seen for the conversation — the top of the
   * hole — so the filler asks for exactly the gap (§158's `to_seq` ranges a SYNC into one hole)
   * and never a tail of whatever has arrived since. Resolves when it has paged as far as it
   * will; a rejection is reported once to the event-error sink and the domain will not ask
   * again until the watermark moves.
   */
  fillGap(conversationId: Id, toSeq: number): Promise<void>;
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
  /**
   * The client-chosen message id, minted by the caller and reused on every retry of the same
   * send. The server's send idempotency is keyed on this id, so a retry after a lost reply —
   * where the request reached the server but the acknowledgement did not — is answered
   * `duplicate` and produces no second row. Omitted, a fresh id is minted per call, which is
   * correct for every path that does not retry: a re-typed message is a new message.
   */
  messageId?: Id;
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
  readonly #gapFiller: GapFiller | undefined;
  readonly #deviceId: Id | undefined;

  readonly #messageListeners = new Set<Listener<IncomingMessage>>();
  readonly #deletionListeners = new Set<Listener<MessageDeletion>>();
  readonly #receiptListeners = new Set<Listener<MessageReceipt>>();
  /**
   * Notified whenever an inbound key exchange established or advanced pairwise or sender-key
   * state — the moment this device's key store may have mutated (a responder handshake consumes
   * one of our one-time prekeys), which is what a caller that persists the store needs to hear.
   */
  readonly #keyExchangeListeners = new Set<Listener<void>>();

  /** Messages we could not open yet, keyed by `${conversationId}|${senderDevice}`. */
  readonly #pending = new Map<string, MessageEvent[]>();

  /**
   * The highest contiguous sequence number held per conversation: section 158's local `last_seq`.
   *
   * Every message routes through the same path — live delivery and sync replay both — so this map
   * advances exactly when the client becomes entitled to claim a prefix: a sequence one past the
   * watermark extends it, anything at or below is a redelivery, and anything further ahead is a gap
   * that holds the watermark where it stands until the missing pages arrive. The first message a
   * conversation ever delivers becomes the floor (history below it may be gone — the server's
   * `Truncated` — or simply unfetched, which is the caller's floor to choose, not this map's).
   */
  readonly #watermarks = new Map<Id, number>();

  /**
   * The highest sequence number ever seen per conversation, gap or no gap.
   *
   * The watermark names the top of what is *held*; this names the top of what has *arrived*. The
   * difference is the hole a gap fill must target: a fill asked for less would leave the tail of
   * the hole unfetched, and one asked for more would tail live traffic instead of filling.
   */
  readonly #highestSeen = new Map<Id, number>();

  /** Conversations with a gap fill in flight, so one hole spawns one fill, not one per event. */
  readonly #filling = new Set<Id>();

  /**
   * The watermark a fill that could not close the gap stopped at, per conversation.
   *
   * While the current watermark equals this value the server has already been asked and has
   * already answered without moving it, so a later above-gap event re-asks nothing — the hot loop
   * a persistent hole must never become. The stall lifts the moment the watermark moves (live
   * delivery filled part of the hole, or a reconnect's resync ran), and a fill that made progress
   * never records one at all.
   */
  readonly #stalledAt = new Map<Id, number>();

  /**
   * Sequence numbers already dispatched above the watermark, so the page that later fills the gap
   * beneath them does not deliver them twice.
   *
   * A live event that lands above the watermark is delivered at once (the hole below is no reason
   * to hold it) and its seq is remembered here; the fill's page then reaches the same seq and must
   * not repeat it. Entries at or below the watermark can never be fetched again and are pruned as
   * the watermark passes them.
   */
  readonly #ahead = new Map<Id, number[]>();

  #unsubscribes: Array<() => void> = [];

  constructor(
    rpc: Rpc,
    sessionCrypto: SessionCrypto,
    groupCrypto: GroupCrypto,
    directory: DeviceDirectory,
    onEventError?: EventErrorHandler,
    gapFiller?: GapFiller,
    deviceId?: Id,
  ) {
    this.#rpc = rpc;
    this.#sessionCrypto = sessionCrypto;
    this.#groupCrypto = groupCrypto;
    this.#directory = directory;
    this.#onEventError = onEventError;
    this.#gapFiller = gapFiller;
    this.#deviceId = deviceId;
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
      // Section 163's redistribution channel: a membership change makes each member device send
      // its fresh sender-key chain to every member device, one GROUP_KEY_DISTRIBUTE per device
      // because each copy is sealed under its own pairwise session. The frame names this device
      // in `toDevice`, but a copy sealed for this *account's* other device lands here too (the
      // relay rides user topics, which are per account) — the pairwise open below refuses it as
      // the same fan-out noise the MESSAGE_SEND path tolerates.
      this.#rpc.on(OP.GROUP_KEY_DISTRIBUTE, decodeGroupKeyDistribution, (event) =>
        this.#onGroupKeyDistribute(event),
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
   * Registers a handler for inbound key exchanges: a sender-key distribution accepted over the
   * pairwise channel, whether it rode a `MESSAGE_SEND` or a section 163 `GROUP_KEY_DISTRIBUTE`.
   *
   * The notification carries no payload on purpose — what the caller needs to know is *that* key
   * material moved, because a responder handshake consumes one of this device's one-time prekeys
   * and mutates the key store a caller persists. It fires whenever a distribution opened through
   * the session layer, even one the group layer then refused as stale: the open itself advanced
   * the ratchet (and may have spent a prekey), so the store moved either way. Fan-out noise — a
   * copy sealed for another device — never fires it.
   */
  onKeyExchange(handler: () => void): () => void {
    this.#keyExchangeListeners.add(handler);
    return () => this.#keyExchangeListeners.delete(handler);
  }

  /**
   * The highest sequence number this client holds contiguously for a conversation.
   *
   * Section 158's reconnect order asks the client to compare the server's `last_seq` against its
   * own and sync *only* the gap; this is the local half of that comparison. `undefined` means
   * nothing has been ingested for the conversation yet, so the caller picks its own floor (a
   * fresh thread replays from the beginning; one whose history is gone replays from whatever the
   * server can still serve). A reconnect where this survives says exactly where to resume: one
   * past this number. A live event that lands above this number plus one is a hole, and the
   * domain now schedules its own fill for it (through the {@link GapFiller} the client supplies),
   * so a mid-session gap heals without waiting for a reconnect to notice it.
   */
  watermark(conversationId: Id): number | undefined {
    return this.#watermarks.get(conversationId);
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
      messageId: options.messageId ?? newId(),
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
   * Edits a message in place: replaces its sealed envelope while keeping the message's seq.
   *
   * The caller seals the replacement exactly as it sealed the original — the same sender key, the
   * same padding policy — and the server stores the new envelope under the existing id, so
   * receivers see the message again with an `editedAt` stamp rather than as a new message. The
   * bytes pass through verbatim; sealing is the caller's crypto, never re-done here.
   */
  async editMessage(conversationId: Id, messageId: Id, envelope: Uint8Array): Promise<void> {
    const request: MessageEdit = { messageId, conversationId, envelope };
    await this.#rpc.call(OP.MESSAGE_EDIT, encodeMessageEdit, decodeAcknowledged, request);
  }

  /**
   * Sends a reaction to a message.
   *
   * The `envelope` is the sealed reaction content — the server learns only that *some* reaction
   * was set on `targetMessageId`, never which emoji — and setting a different reaction or the
   * same one again is a server-side replace. A reaction reaches the conversation's other
   * participants as a {@link protocol.ReactionEvent}, coalesced per target message.
   */
  async sendReaction(targetMessageId: Id, conversationId: Id, envelope: Uint8Array): Promise<void> {
    const request: ReactionSet = { targetMessageId, conversationId, envelope };
    await this.#rpc.call(OP.REACTION_SET, encodeReactionSet, decodeAcknowledged, request);
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
   * Rotates onto a membership change and redistributes the fresh chain: section 163's client-side
   * obligation when a group's membership moves.
   *
   * The wire names the generation the change produced (`groupKeyEpoch` on the member event), so
   * every member device rotates onto the *same* epoch without coordinating — and each device owns
   * its own chain, which is why every member redistributing independently cannot collide the way
   * a shared call key would. With `epoch` the rotation lands on exactly that generation (never
   * below what the chain already holds, so a stale event is a no-op); without it the chain simply
   * bumps. Then one `GROUP_KEY_DISTRIBUTE` leaves per member device that still lacks the chain —
   * after a genuine rotation that is everyone; after a no-op it is whoever joined late — each
   * sealed under the pairwise session with that device, exactly as the first-send path seals
   * distributions, so the receiving side accepts it with the same logic. One device's failure
   * (a member already removed, a fetch refused) must not strand the rest, so each send is caught
   * and reported rather than thrown.
   */
  async redistributeSenderKey(conversationId: Id, epoch?: number): Promise<void> {
    if (this.#deviceId === undefined) {
      // The frame must honestly name the distributing device; the receiver's ratchet is keyed by
      // it, so a nil stamp would make every distribution we send unopenable.
      throw new SdkError('messaging: redistributeSenderKey requires the device id');
    }
    if (epoch === undefined) {
      this.#groupCrypto.rotate(conversationId);
    } else {
      this.#groupCrypto.rotateTo(conversationId, epoch);
    }
    await this.#distributeChain(
      conversationId,
      OP.GROUP_KEY_DISTRIBUTE,
      (sealed, device) => {
        // Section 163's redistribution frame: addressed to the one device the copy is sealed
        // for, because the receiving side keys its ratchet lookup by our device id.
        const request: GroupKeyDistribution = {
          conversationId,
          fromDevice: this.#deviceId as Id,
          toAccount: device.userId,
          toDevice: device.deviceId,
          sealedDistribution: sealed.envelope,
        };
        return this.#rpc.call(
          OP.GROUP_KEY_DISTRIBUTE,
          encodeGroupKeyDistribution,
          decodeAcknowledged,
          request,
        );
      },
      true,
    );
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
    await this.#distributeChain(conversationId, OP.MESSAGE_SEND, (sealed, _device) => {
      const send: MessageSend = {
        messageId: newId(),
        conversationId,
        kind: MessageKind.KeyExchange,
        envelope: sealed.envelope,
        senderKeyId: sealed.senderKeyId,
      };
      return this.#rpc.call(OP.MESSAGE_SEND, encodeMessageSend, decodeMessageAccepted, send);
    });
  }

  /**
   * The distribution loop both channels share: seal the current chain for one device at a time,
   * hand the sealed envelope to the channel's own frame builder, and mark. `tolerate` is the
   * difference between the two callers — a send whose key exchange fails must fail (the content
   * would be undecryptable for that device), while a redistribution triggered by a member event
   * must reach every device it can and not strand the rest behind one refusal (the member was
   * removed mid-flight, the bundle fetch failed).
   */
  async #distributeChain(
    conversationId: Id,
    opcode: number,
    sendOne: (sealed: SealedEnvelope, device: DeviceAddress) => Promise<unknown>,
    tolerate = false,
  ): Promise<void> {
    const devices = await this.#directory.recipientDevices(conversationId);
    for (const device of devices) {
      if (!this.#groupCrypto.needsDistribution(conversationId, device.deviceId)) {
        continue;
      }
      try {
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
        await sendOne(sealed, device);
      } catch (cause) {
        if (!tolerate) {
          throw cause;
        }
        // One device's refusal is reported, not thrown: the remaining member devices still need
        // the fresh chain, and a member removed between the roster read and this send is the
        // server's PERMISSION_DENIED working as designed.
        this.#onEventError?.(opcode, cause);
        continue;
      }
      this.#groupCrypto.markDistributed(conversationId, device.deviceId);
    }
  }

  /** Routes one inbound message event by kind. */
  #onMessageEvent(event: MessageEvent): void {
    this.#trackWatermark(event);
    if (this.#alreadyDispatched(event)) {
      // The gap fill's page caught up with a delivery the live stream had already made above the
      // hole; the accounting above already counted it, so the dispatch below must not repeat it.
      return;
    }
    this.#rememberDispatch(event);
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
   * Advances the conversation's contiguous watermark, or notes a gap by standing still.
   *
   * Tombstones count: a deletion occupies a sequence number like any message, so the prefix this
   * map describes is of *events*, not of content. Called before dispatch so both listeners and
   * the crypto layers below see the same accounting a later `watermark` read reports. A sequence
   * above `held + 1` is a hole in a space §152 says is gapless, so it also schedules the fill that
   * asks the {@link GapFiller} for the missing pages.
   */
  #trackWatermark(event: MessageEvent): void {
    const held = this.#watermarks.get(event.conversationId);
    if (held === undefined || event.seq === held + 1) {
      // The floor, or the next brick on top of it.
      this.#watermarks.set(event.conversationId, event.seq);
    }
    const high = this.#highestSeen.get(event.conversationId);
    if (high === undefined || event.seq > high) {
      this.#highestSeen.set(event.conversationId, event.seq);
    }
    if (held !== undefined && event.seq > held + 1) {
      // Above held + 1: a gap. The watermark waits for the missing pages, and something goes to
      // fetch them — a resync from what is truly held is exactly what section 158 asks for.
      this.#scheduleGapFill(event.conversationId);
    }
    // At or below: a redelivery the caller's own dedup handles. Below the floor: the caller's
    // floor to choose, not ours to fill.
  }

  /**
   * Asks the gap filler for the pages a detected hole is missing, once per hole.
   *
   * The ask is guarded twice so a hole cannot become a loop. While a fill is in flight the guard
   * is the {@link #filling} set — every later above-gap event for that conversation is already
   * the running fill's business. After a fill that could not move the watermark the guard is
   * {@link #stalledAt}: the server has answered, and re-asking on every later event would be
   * exactly the hot loop a persistent hole must never become. A fill that made progress records
   * no stall, so a fill cut short by its page budget continues on the next above-gap event; and
   * when events arrived *above* the target while a fill ran, the continuation is scheduled as
   * the fill ends, without waiting for another event to notice the new hole.
   */
  #scheduleGapFill(conversationId: Id): void {
    const filler = this.#gapFiller;
    if (filler === undefined || this.#filling.has(conversationId)) {
      return;
    }
    const haveSeq = this.#watermarks.get(conversationId);
    const toSeq = this.#highestSeen.get(conversationId);
    if (haveSeq === undefined || toSeq === undefined || toSeq <= haveSeq) {
      return;
    }
    if (this.#stalledAt.get(conversationId) === haveSeq) {
      return;
    }
    this.#filling.add(conversationId);
    void filler
      .fillGap(conversationId, toSeq)
      .catch((cause: unknown) => {
        // The fill is background repair; its failure is surfaced, not thrown into the live path.
        this.#onEventError?.(OP.SYNC, cause);
      })
      .finally(() => {
        this.#filling.delete(conversationId);
        const after = this.#watermarks.get(conversationId) ?? haveSeq;
        if (after > haveSeq) {
          this.#stalledAt.delete(conversationId);
        } else {
          this.#stalledAt.set(conversationId, after);
        }
        const ceiling = this.#highestSeen.get(conversationId) ?? toSeq;
        if (ceiling > toSeq && after < ceiling) {
          // Events arrived above the target while the fill ran; their hole is a new ask. The
          // stall recorded above (when the fill made no progress) still applies, so this cannot
          // chain on its own — only genuinely new arrivals reopen the question.
          this.#scheduleGapFill(conversationId);
        }
      });
  }

  /**
   * Whether this event was already dispatched as an above-the-watermark live delivery.
   *
   * Consuming the memory: the seq is forgotten here, because the dispatch it guarded against has
   * either happened (this page copy is that dispatch's duplicate) or the page it would have ridden
   * never came and the live copy stands alone.
   */
  #alreadyDispatched(event: MessageEvent): boolean {
    const ahead = this.#ahead.get(event.conversationId);
    if (ahead === undefined) {
      return false;
    }
    const index = ahead.indexOf(event.seq);
    if (index !== -1) {
      ahead.splice(index, 1);
      return true;
    }
    // Entries at or below the watermark can never be fetched again; prune them so the list holds
    // only the live window above the hole.
    const watermark = this.#watermarks.get(event.conversationId);
    if (watermark !== undefined) {
      const live = ahead.filter((seq) => seq > watermark);
      if (live.length !== ahead.length) {
        if (live.length === 0) {
          this.#ahead.delete(event.conversationId);
        } else {
          this.#ahead.set(event.conversationId, live);
        }
      }
    }
    return false;
  }

  /**
   * Remembers an above-the-watermark delivery, so the gap fill's page cannot repeat it.
   *
   * Only a seq above the current watermark can be re-fetched later — everything at or below it is
   * behind the cursor every fetch starts from — so those are the only deliveries worth guarding.
   */
  #rememberDispatch(event: MessageEvent): void {
    const watermark = this.#watermarks.get(event.conversationId);
    if (watermark === undefined || event.seq <= watermark) {
      return;
    }
    let ahead = this.#ahead.get(event.conversationId);
    if (ahead === undefined) {
      ahead = [];
      this.#ahead.set(event.conversationId, ahead);
    }
    if (!ahead.includes(event.seq)) {
      ahead.push(event.seq);
      if (ahead.length > MAX_AHEAD_REMEMBERED) {
        ahead.shift();
      }
    }
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
    this.#acceptKeyDistribution(
      event.conversationId,
      event.senderId,
      event.senderDevice,
      event.envelope,
    );
  }

  /**
   * Handles a section 163 `GROUP_KEY_DISTRIBUTE`: the redistribution a member device sends when
   * membership changes. The accept logic is the KeyExchange path's own — the sealed body is the
   * same control event over the same pairwise channel — so both channels stay interchangeable
   * for a receiver, whichever one a sender chose.
   */
  #onGroupKeyDistribute(event: GroupKeyDistribution): void {
    // `toAccount` rides the wire frame; the 1:1 layer takes the sender's identity from the
    // envelope's own X3DH material, so the slot is unused here and passes through as the honest
    // value the frame carries.
    this.#acceptKeyDistribution(
      event.conversationId,
      event.toAccount,
      event.fromDevice,
      event.sealedDistribution,
    );
  }

  /**
   * Opens, adopts, and drains one sealed sender-key distribution, whichever channel it rode.
   *
   * A copy sealed for another device throws at the open and is dropped as expected fan-out noise;
   * a body that is not a sender-key distribution is dropped the same way (the pairwise channel
   * carries other control events). Only a genuine accept fires the key-exchange notification,
   * because only a genuine accept is evidence the key store may have moved.
   */
  #acceptKeyDistribution(
    conversationId: Id,
    senderUserId: Id,
    senderDeviceId: Id,
    envelope: Uint8Array,
  ): void {
    let plaintext: Uint8Array;
    try {
      plaintext = this.#sessionCrypto.open(conversationId, senderUserId, senderDeviceId, envelope);
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
    this.#groupCrypto.acceptDistribution(conversationId, senderDeviceId, content.data);
    this.#drainPending(conversationId, senderDeviceId);
    for (const listener of this.#keyExchangeListeners) {
      try {
        listener();
      } catch (cause) {
        this.#onEventError?.(OP.GROUP_KEY_DISTRIBUTE, cause);
      }
    }
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
