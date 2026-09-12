/**
 * The group-call domain: the signaling half of an SFU (server-forwarded) group call.
 *
 * Where a 1:1 call is two devices exchanging sealed media descriptions with each other, a group
 * call is a *roster*: every participant publishes one sealed offer to the server, the server stores
 * and re-serves those blobs without ever opening them (the same mail-slot promise the rest of the
 * protocol makes — the server routes bytes, it does not read them), and each client dials the
 * others directly. What lives here is the membership signaling around that: joining a call, leaving
 * it, and the three events a roster UI renders — the snapshot a joiner builds a screen from, and
 * the join/leave announcements everyone else keeps their list true with.
 *
 * # The two topics, one opcode
 *
 * Every group-call event arrives as `CALL_SFU_EVENT` (a `CallStateEvent` payload, the same struct
 * the 1:1 domain's `CALL_STATE_EVENT` uses), but on two different topics with different shapes:
 *
 *   - The joiner's **own user topic** receives the roster snapshot — the full participant list —
 *     published by the server as part of answering the join. It is the one frame a joining screen
 *     builds the call from, and it is published rather than replied because the join's reply slot
 *     is already spent on the TURN relay list.
 *   - The **conversation's topic** receives the announcements: one `Connected` frame when a
 *     participant joins (carrying their sealed offer — the E2E media-description hand-off) and one
 *     `Ended` frame when a participant leaves. A seat replacement — the same account joining from a
 *     new device — is a departure followed by an arrival, and the roster hears both facts, in that
 *     order.
 *
 * The domain classifies each frame by shape (the snapshot is the one carrying a participant list)
 * and fans it out to three typed listeners, so a roster UI never has to re-derive which kind of
 * frame it just got.
 *
 * # What this domain does not do
 *
 * Like the 1:1 domain, it holds no call state — which calls this account is seated in is the
 * application's projection, fed by these events. It never opens or seals anything: the sealed
 * offers pass through in both directions verbatim, and the end-to-end media encryption is the
 * caller's crypto, never the server's and never this domain's. One seat per account is a server
 * rule (a second join from the same account replaces the seat); the client observes it as the
 * departure/arrival pair above, it does not enforce it.
 */

import type { Id } from '@migo/wire';
import { NIL_ID } from '@migo/wire';
import {
  OP,
  encodeCallInvite,
  encodeCallEnd,
  decodeCallTurnResponse,
  decodeCallStateEvent,
  decodeAcknowledged,
} from '@migo/protocol';
import type { CallSfuParticipant, CallStateEvent, TurnServer } from '@migo/protocol';

import { newId } from '../ids.js';
import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';
import { CallEndReason, CallMediaKind, CallState } from './calls.js';

export type { CallSfuParticipant } from '@migo/protocol';

/**
 * The wire's `CallStateEvent.state` is a bare number, and the group-call shapes ride two of its
 * values: a roster snapshot and a join announcement are `Connected`, a departure is `Ended`. The
 * SDK's {@link CallState} enum mirrors those numbers; reading through `number` is what keeps the
 * classification a number-to-number comparison rather than an enum compared against a field a
 * future schema version could widen.
 */
const STATE_CONNECTED: number = CallState.Connected;
const STATE_ENDED: number = CallState.Ended;

/**
 * The roster snapshot a joiner's own user topic receives: everything a call screen builds its
 * participant list from, in one frame.
 *
 * `userId`/`deviceId` name the joiner the snapshot was published to — this account, on this
 * device — so a UI that wants to mark "you" in the roster can find its own line without asking
 * the server who it is.
 */
export interface GroupCallRoster {
  callId: Id;
  /** The conversation whose members may join this call. */
  conversationId: Id;
  /** The joiner this snapshot was published to. */
  userId: Id;
  deviceId: Id;
  /** The call's size, as the server counted it when publishing. */
  participantCount: number;
  /** The full roster, in join order. */
  participants: CallSfuParticipant[];
}

/**
 * A participant joined, as the conversation's topic announces it.
 *
 * `sealedOffer` is the joiner's media description, sealed for the call's members — the blob a
 * participant's WebRTC stack dials the joiner with. Absent only in a future shape this version
 * does not send.
 */
export interface GroupCallJoinedEvent {
  callId: Id;
  conversationId: Id;
  userId: Id;
  deviceId: Id;
  /** The call's size after the join. */
  participantCount: number;
  sealedOffer?: Uint8Array;
}

/**
 * A participant left, as the conversation's topic announces it.
 *
 * The wire has no "left" state, so a departure rides as `Ended` with a `ByCaller` reason — the
 * participant withdrew themselves. `participantCount` of zero means the last seat has left and the
 * call is retired: there is nothing to dial back into, and a UI should clear the call.
 */
export interface GroupCallLeftEvent {
  callId: Id;
  conversationId: Id;
  userId: Id;
  deviceId: Id;
  /** The call's size after the departure; zero means the call itself is gone. */
  participantCount: number;
}

/** What {@link GroupCallDomain.join} resolves with: the call's id and the relays to dial through. */
export interface GroupCallJoinResult {
  /** The id the server dedupes the join on — the one to leave with, and the one the events carry. */
  callId: Id;
  /** Short-lived TURN relay credentials, as a 1:1 `CALL_TURN_FETCH` would return. */
  servers: TurnServer[];
}

/**
 * Signal an SFU group call: join, leave, and observe the roster.
 *
 * One instance per client, constructed with this device's id and started alongside the other
 * domains. The join is idempotent by its `callId` — a retried join re-seats the same call rather
 * than creating a second one — and leaving reuses the 1:1 `CALL_END` frame: the server routes a
 * `CALL_END` to the group service when the id names a group call, so there is no separate leave
 * opcode and this domain stamps the same `ByCaller` reason the departure announcement carries.
 */
export class GroupCallDomain {
  readonly #rpc: Rpc;
  readonly #deviceId: Id;
  readonly #onEventError: EventErrorHandler | undefined;

  readonly #rosterListeners: ListenerSet<GroupCallRoster>;
  readonly #joinedListeners: ListenerSet<GroupCallJoinedEvent>;
  readonly #leftListeners: ListenerSet<GroupCallLeftEvent>;

  #unsubscribes: Array<() => void> = [];

  constructor(rpc: Rpc, deviceId: Id, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#deviceId = deviceId;
    this.#onEventError = onEventError;
    this.#rosterListeners = new ListenerSet(OP.CALL_SFU_EVENT, onEventError);
    this.#joinedListeners = new ListenerSet(OP.CALL_SFU_EVENT, onEventError);
    this.#leftListeners = new ListenerSet(OP.CALL_SFU_EVENT, onEventError);
  }

  /** Begins delivering group-call events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribes.length > 0) {
      return;
    }
    this.#unsubscribes.push(
      // One opcode carries all three shapes (see the file header); the classification happens
      // here, once, so each listener set hands its handlers exactly one typed fact.
      this.#rpc.on(OP.CALL_SFU_EVENT, decodeCallStateEvent, (event) => {
        if (event.participants !== undefined) {
          this.#deliverRoster(event);
        } else if (event.state === STATE_CONNECTED) {
          this.#deliverJoined(event);
        } else if (event.state === STATE_ENDED) {
          this.#deliverLeft(event);
        }
        // Any other shape is a future server's; this version has nothing honest to hand a
        // handler for it, and dropping it quietly keeps the roster true to what it can render.
      }),
    );
  }

  /** Stops delivering group-call events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    for (const unsubscribe of this.#unsubscribes) {
      unsubscribe();
    }
    this.#unsubscribes = [];
  }

  /**
   * Registers a handler for roster snapshots. Returns its unsubscribe.
   *
   * The server publishes one to this account's own topic for every accepted join — including a
   * re-join after a seat replacement — so this fires once per {@link join}, never for other
   * participants' movement.
   */
  onRoster(handler: Listener<GroupCallRoster>): () => void {
    return this.#rosterListeners.add(handler);
  }

  /**
   * Registers a handler for join announcements. Returns its unsubscribe.
   *
   * Every seated participant hears these on the conversation's topic, except the joiner's own
   * connection (the snapshot above is that join's event). A seat replacement arrives as a
   * departure ({@link onParticipantLeft}) followed by one of these, naming the same account on
   * the new device.
   */
  onParticipantJoined(handler: Listener<GroupCallJoinedEvent>): () => void {
    return this.#joinedListeners.add(handler);
  }

  /**
   * Registers a handler for departure announcements. Returns its unsubscribe.
   *
   * `participantCount` of zero is the retirement: the last seat has left and the call no longer
   * exists server-side. A leave sent from *this* connection is answered by the reply, not an
   * announcement — but this account's other devices hear it, which is the point.
   */
  onParticipantLeft(handler: Listener<GroupCallLeftEvent>): () => void {
    return this.#leftListeners.add(handler);
  }

  /**
   * Joins (or re-joins) a group call, publishing this device's sealed offer.
   *
   * The `callId` is minted here unless passed — client-minted ids are the protocol's idempotency
   * key, so a retried join re-seats the same call — and the reply carries the TURN relays the
   * media plane should dial through, exactly as a 1:1 fetch would. The roster itself does not
   * come back on the reply: the server *publishes* the full participant list to this account's
   * own topic as the {@link onRoster} snapshot, so register that handler before joining or the
   * snapshot can race a late subscriber.
   *
   * The join frame is the 1:1 invite's shape read with group semantics: `calleeId` is the nil id
   * (a group call has no single callee) and `capabilities` rides as zero, the same as a 1:1
   * invite. `callerDevice` stamps this session's device even though the server takes the joining
   * device from the connection — an honest frame beats a slot the server fills itself.
   */
  async join(
    conversationId: Id,
    mediaKind: CallMediaKind,
    sealedOffer: Uint8Array,
    callId?: Id,
  ): Promise<GroupCallJoinResult> {
    const id = callId ?? newId();
    // The wire struct is the 1:1 `CallInvite`; the group reader ignores `calleeId` and
    // `callerDevice`, but they are required slots, so they carry their honest values.
    const request = {
      callId: id,
      conversationId,
      calleeId: NIL_ID,
      mediaKind,
      callerDevice: this.#deviceId,
      capabilities: 0n,
      sealedOffer,
    };
    const response = await this.#rpc.call(
      OP.CALL_SFU_JOIN,
      encodeCallInvite,
      decodeCallTurnResponse,
      request,
    );
    return { callId: id, servers: response.servers };
  }

  /**
   * Leaves a group call.
   *
   * This is the 1:1 `end` frame — the server routes a `CALL_END` to the group service when the id
   * names a group call — stamped with the same `ByCaller` reason the departure announcement
   * carries, because that is the fact: the participant withdrew themselves. When the last seat
   * leaves, the server retires the call; there is nothing to re-join under that id.
   */
  async leave(callId: Id): Promise<void> {
    const request = { callId, reason: CallEndReason.ByCaller };
    await this.#rpc.call(OP.CALL_END, encodeCallEnd, decodeAcknowledged, request);
  }

  /**
   * Hands a decoded snapshot to the roster listeners, after checking the fields a roster cannot
   * render without. The server always sets them; a frame missing one is malformed in the same way
   * a frame that fails to decode is, so it goes to the error sink rather than a handler.
   */
  #deliverRoster(event: CallStateEvent): void {
    if (
      event.conversationId === undefined ||
      event.userId === undefined ||
      event.deviceId === undefined ||
      event.participantCount === undefined
    ) {
      this.#onEventError?.(OP.CALL_SFU_EVENT, new Error('roster snapshot missing required fields'));
      return;
    }
    this.#rosterListeners.deliver({
      callId: event.callId,
      conversationId: event.conversationId,
      userId: event.userId,
      deviceId: event.deviceId,
      participantCount: event.participantCount,
      participants: event.participants ?? [],
    });
  }

  /** Hands a decoded join announcement to the joined listeners, with the same malformed guard. */
  #deliverJoined(event: CallStateEvent): void {
    if (
      event.conversationId === undefined ||
      event.userId === undefined ||
      event.deviceId === undefined ||
      event.participantCount === undefined
    ) {
      this.#onEventError?.(
        OP.CALL_SFU_EVENT,
        new Error('join announcement missing required fields'),
      );
      return;
    }
    const joined: GroupCallJoinedEvent = {
      callId: event.callId,
      conversationId: event.conversationId,
      userId: event.userId,
      deviceId: event.deviceId,
      participantCount: event.participantCount,
    };
    if (event.sealedOffer !== undefined) {
      joined.sealedOffer = event.sealedOffer;
    }
    this.#joinedListeners.deliver(joined);
  }

  /** Hands a decoded departure announcement to the left listeners, with the same malformed guard. */
  #deliverLeft(event: CallStateEvent): void {
    if (
      event.conversationId === undefined ||
      event.userId === undefined ||
      event.deviceId === undefined ||
      event.participantCount === undefined
    ) {
      this.#onEventError?.(
        OP.CALL_SFU_EVENT,
        new Error('departure announcement missing required fields'),
      );
      return;
    }
    this.#leftListeners.deliver({
      callId: event.callId,
      conversationId: event.conversationId,
      userId: event.userId,
      deviceId: event.deviceId,
      participantCount: event.participantCount,
    });
  }
}
