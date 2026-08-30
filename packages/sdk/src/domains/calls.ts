/**
 * The calls domain: the signaling half of a 1:1 voice or video call.
 *
 * A call is two planes with nothing in common but the `callId`. The media plane is WebRTC: SDP and
 * ICE candidates exchanged *through* this domain as opaque sealed bytes, then media flowing directly
 * between the two devices, encrypted end to end, never touching a Migo server. The signaling plane
 * is what lives here: invite, answer, decline, cancel, end, and the relaying of those sealed SDP and
 * ICE blobs — the server routes them by device id but cannot read them, because an SDP body carries
 * DTLS fingerprints and ICE candidates carry the two parties' network addresses, and a signaling
 * server that could read them would learn exactly what the end-to-end promise exists to protect.
 *
 * # Why the caller learns the callee's device from the answer, not the invite
 *
 * `CALL_INVITE` names the callee *account*, and the server fans the invite out to that account's
 * devices; which device answers is not knowable in advance. So a caller cannot address `CALL_SDP`
 * or `CALL_ICE` until an answer comes back — the answer's relay ({@link onSdp}) carries the
 * answering device in `fromDevice`, and only then does the caller have a target for its own
 * candidates. A callee has no such wait: the invite event already names `callerDevice`.
 *
 * # What this domain does not do
 *
 * It holds no call state. Which calls are ringing, connected, or ended is a *product* projection the
 * application keeps ({@link ActiveCall} is its shape); the server pushes the authoritative
 * transitions through {@link onCallState}. It also never opens or seals anything: the bytes handed
 * to {@link invite}, {@link answer}, {@link sendSdp}, and {@link sendIce} are already sealed by the
 * caller, and the bytes handed back through {@link onIncomingCall} and the relay listeners are
 * passed through verbatim. The end-to-end media encryption is a separate, future layer; until it
 * lands the application seals with placeholder material, and this domain cannot tell the
 * difference — by design, so swapping the real crypto in touches nothing here.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  encodeCallInvite,
  decodeCallInviteResult,
  encodeCallAnswer,
  encodeCallDecline,
  encodeCallCancel,
  encodeCallEnd,
  encodeCallSdp,
  encodeCallIce,
  encodeCallStats,
  encodeCallTurnFetch,
  decodeCallTurnResponse,
  decodeCallInviteEvent,
  decodeCallStateEvent,
  decodeCallSdp,
  decodeCallIce,
  decodeAcknowledged,
} from '@migo/protocol';
import type {
  CallInviteResult,
  CallInviteEvent,
  CallStateEvent,
  CallSdp,
  CallIce,
  CallStats,
  TurnServer,
} from '@migo/protocol';

import { newId } from '../ids.js';
import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * The life of a call as the server reports it. These are the wire's five states; the product
 * surface (section 180) adds a sixth, *degraded*, which is a connected call whose quality has
 * dropped — a client-side judgement from live media statistics, not a signaling fact, so it is not
 * on the wire and not in this enum.
 */
export enum CallState {
  /** The invite is out and the callee's client is (or will be) ringing. */
  Ringing = 0,
  /** Answered; SDP and ICE are being exchanged but media is not flowing yet. */
  Connecting = 1,
  /** Media is flowing. */
  Connected = 2,
  /** The transport dropped and the reconnect window (ICE restart, renegotiation) is running. */
  Reconnecting = 3,
  /** Terminal, always with a reason ({@link CallEndReason}). */
  Ended = 4,
}

/** What a call carries: audio only, or audio plus video. */
export enum CallMediaKind {
  Audio = 0,
  Video = 1,
}

/** Why a call ended. The first three are human decisions; the last two are system failures. */
export enum CallEndReason {
  /** The caller hung up. */
  ByCaller = 0,
  /** The callee hung up. */
  ByCallee = 1,
  /** The callee declined the invite. */
  Declined = 2,
  /** Nobody answered before the invite expired. */
  NoAnswer = 3,
  /** The call never connected (media failure, unreachable peer). */
  Failed = 4,
  /** The network gave out and the reconnect window closed. */
  Network = 5,
}

/** Why a callee declined. `Busy` answers faster than a ring that can never be picked up. */
export enum CallDeclineReason {
  Busy = 0,
  Declined = 1,
}

/**
 * The application-side projection of one call: everything a call UI needs to render, in one shape.
 *
 * The domain never builds this — it is the caller's own bookkeeping, fed by the domain's replies
 * and events. `startedAt` is the moment media first connected (the duration timer's zero), set by
 * the client when it sees `Connected`, not by the server.
 */
export interface ActiveCall {
  callId: Id;
  conversationId: Id;
  /** The account that placed the call. */
  callerId: Id;
  /** The account the call was placed to. */
  calleeId: Id;
  mediaKind: CallMediaKind;
  state: CallState;
  /** Present once {@link CallState.Ended}; the reason the call ended. */
  endReason?: CallEndReason;
  /**
   * The wire's `CallInviteResult.status` when the invite never rang (declined, expired, or the
   * callee blocked the caller), kept as the raw number so the ended screen can state the
   * distinction the wire drew — the reason enum has no Blocked member, and a blocked refusal is
   * a different fact on the caller's screen than a declined one. Absent for every call that
   * rang, and never sent back to the server: `endReason` is the wire-facing half of the fact.
   */
  inviteStatus?: number;
  /** Whether this device placed the call — decides whose "Decline"/"Cancel" button shows. */
  isCaller: boolean;
  /** When media first connected, in epoch milliseconds; absent until then. */
  startedAt?: number;
}

/**
 * Signal a 1:1 call: invite, answer, and the relays that carry sealed SDP and ICE between devices.
 *
 * One instance per client. Constructed with this device's id — the caller device stamp on an
 * invite and the `fromDevice` on every relay must be the id the server authenticated this session
 * under, which only the composition root knows. {@link start} begins delivering the four inbound
 * streams; register handlers before calling it so the first invite cannot race a late subscriber.
 */
export class CallsDomain {
  readonly #rpc: Rpc;
  readonly #deviceId: Id;

  readonly #inviteListeners: ListenerSet<CallInviteEvent>;
  readonly #stateListeners: ListenerSet<CallStateEvent>;
  readonly #sdpListeners: ListenerSet<CallSdp>;
  readonly #iceListeners: ListenerSet<CallIce>;

  #unsubscribes: Array<() => void> = [];

  constructor(rpc: Rpc, deviceId: Id, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#deviceId = deviceId;
    this.#inviteListeners = new ListenerSet(OP.CALL_INVITE_EVENT, onEventError);
    this.#stateListeners = new ListenerSet(OP.CALL_STATE_EVENT, onEventError);
    this.#sdpListeners = new ListenerSet(OP.CALL_SDP, onEventError);
    this.#iceListeners = new ListenerSet(OP.CALL_ICE, onEventError);
  }

  /** Begins delivering call events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribes.length > 0) {
      return;
    }
    this.#unsubscribes.push(
      this.#rpc.on(OP.CALL_INVITE_EVENT, decodeCallInviteEvent, (event) =>
        this.#inviteListeners.deliver(event),
      ),
      this.#rpc.on(OP.CALL_STATE_EVENT, decodeCallStateEvent, (event) =>
        this.#stateListeners.deliver(event),
      ),
      // A relay is addressed to one device; one sealed for another device (the server fans an
      // invite out to every device of the callee account, and answers may come from any of them)
      // is not ours to open. Delivering only relays addressed to this device keeps a handler from
      // ever seeing a blob it has no session for.
      this.#rpc.on(OP.CALL_SDP, decodeCallSdp, (event) => {
        if (event.toDevice === this.#deviceId) {
          this.#sdpListeners.deliver(event);
        }
      }),
      this.#rpc.on(OP.CALL_ICE, decodeCallIce, (event) => {
        if (event.toDevice === this.#deviceId) {
          this.#iceListeners.deliver(event);
        }
      }),
    );
  }

  /** Stops delivering call events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    for (const unsubscribe of this.#unsubscribes) {
      unsubscribe();
    }
    this.#unsubscribes = [];
  }

  /** Registers a handler for inbound invites (another account is calling us). Returns its unsubscribe. */
  onIncomingCall(handler: Listener<CallInviteEvent>): () => void {
    return this.#inviteListeners.add(handler);
  }

  /**
   * Registers a handler for a call's state transitions. Returns its unsubscribe.
   *
   * The server is the authority on Ringing/Connecting/Connected/Ended; a client's own transport
   * observations refine *between* these events (its reconnect window, its degraded judgement) but
   * must not contradict an `Ended`, which is terminal.
   */
  onCallState(handler: Listener<CallStateEvent>): () => void {
    return this.#stateListeners.add(handler);
  }

  /**
   * Registers a handler for SDP relays addressed to this device. Returns its unsubscribe.
   *
   * For a caller this is how the answer arrives; for a callee, a renegotiated offer (a future
   * flow). The blob is sealed for this device — the handler receives it verbatim and opening it is
   * the application's crypto, never the server's.
   */
  onSdp(handler: Listener<CallSdp>): () => void {
    return this.#sdpListeners.add(handler);
  }

  /**
   * Registers a handler for batched ICE candidate relays addressed to this device. Returns its
   * unsubscribe.
   *
   * One relay carries a whole batch, not one candidate — a session's gathering can produce tens of
   * candidates, and one frame each is exactly the signaling storm the batch shape exists to avoid.
   */
  onIce(handler: Listener<CallIce>): () => void {
    return this.#iceListeners.add(handler);
  }

  /**
   * Places a call: sends the sealed offer and resolves with the server's verdict.
   *
   * The `callId` is minted here — client-minted ids are the protocol's idempotency key, so a
   * retried invite re-rings the same call rather than placing a second one — and is echoed in the
   * {@link CallInviteResult}; track the call under that id. A caller that needs the id *before*
   * the invite lands (to fetch TURN relays for the peer connection that will produce the offer —
   * `CALL_TURN_FETCH` must be addressed before the call exists server-side, and its handler
   * charges nothing and reads no call state, so the id of the call about to be placed is the
   * honest key) passes its own minted `callId`; the reply echoes whichever id was sent. The
   * result's `status` says whether the callee is being rung (`0`), or why not (declined,
   * expired, blocked); `expiresAt` is the moment an unanswered invite ends itself with
   * {@link CallEndReason.NoAnswer}.
   */
  async invite(
    conversationId: Id,
    calleeId: Id,
    mediaKind: CallMediaKind,
    sealedOffer: Uint8Array,
    callId?: Id,
  ): Promise<CallInviteResult> {
    const request = {
      callId: callId ?? newId(),
      conversationId,
      calleeId,
      mediaKind,
      callerDevice: this.#deviceId,
      // No codec or feature bits are negotiated in this version; the field rides as zero so a
      // future negotiation has its slot without a wire change.
      capabilities: 0n,
      sealedOffer,
    };
    return this.#rpc.call(OP.CALL_INVITE, encodeCallInvite, decodeCallInviteResult, request);
  }

  /**
   * Answers a ringing call with the sealed SDP answer.
   *
   * This tells the *server* the call is answered (the caller's client learns of it through the
   * state stream); the answer itself reaches the caller's WebRTC stack through a `CALL_SDP` relay,
   * which the application sends separately via {@link sendSdp} — this method does not send it,
   * because the relay needs the caller's device id, which lives in the invite event the
   * application holds, not in this domain.
   */
  async answer(callId: Id, sealedAnswer: Uint8Array): Promise<void> {
    const request = { callId, calleeDevice: this.#deviceId, sealedAnswer };
    await this.#rpc.call(OP.CALL_ANSWER, encodeCallAnswer, decodeAcknowledged, request);
  }

  /**
   * Declines a ringing call.
   *
   * `Busy` tells the caller's client to stop ringing immediately without implying a human refusal;
   * the default, `Declined`, is the human's "no".
   */
  async decline(callId: Id, reason: CallDeclineReason = CallDeclineReason.Declined): Promise<void> {
    const request = { callId, reason };
    await this.#rpc.call(OP.CALL_DECLINE, encodeCallDecline, decodeAcknowledged, request);
  }

  /**
   * Cancels a call we placed while it is still ringing.
   *
   * Cancel is the caller's pre-answer exit; after media connects the same intent is
   * {@link end} with {@link CallEndReason.ByCaller}.
   */
  async cancel(callId: Id): Promise<void> {
    await this.#rpc.call(OP.CALL_CANCEL, encodeCallCancel, decodeAcknowledged, { callId });
  }

  /**
   * Ends an established call, always with a reason.
   *
   * The reason is what the other side's ended screen states, and the server records it as call
   * history metadata. Hang up as {@link CallEndReason.ByCaller} or {@link CallEndReason.ByCallee};
   * the system reasons are for failures this side detected.
   */
  async end(callId: Id, reason: CallEndReason): Promise<void> {
    const request = { callId, reason };
    await this.#rpc.call(OP.CALL_END, encodeCallEnd, decodeAcknowledged, request);
  }

  /**
   * Relays a sealed SDP offer or answer to one device of the peer.
   *
   * `toDevice` is the peer device the blob is sealed for — for a caller, the answering device from
   * the relayed answer's `fromDevice`; for a callee, the invite event's `callerDevice`. This
   * domain stamps `fromDevice` with this session's device id.
   */
  async sendSdp(callId: Id, toDevice: Id, sealedSdp: Uint8Array): Promise<void> {
    const request = { callId, fromDevice: this.#deviceId, toDevice, sealedSdp };
    await this.#rpc.call(OP.CALL_SDP, encodeCallSdp, decodeAcknowledged, request);
  }

  /**
   * Relays a batch of sealed ICE candidates to one device of the peer.
   *
   * Batch before calling this — one frame per candidate is the anti-pattern the wire comment calls
   * out — and hold a short linger so a trickling batch leaves as few frames as it can.
   */
  async sendIce(callId: Id, toDevice: Id, sealedCandidates: Uint8Array): Promise<void> {
    const request = { callId, fromDevice: this.#deviceId, toDevice, sealedCandidates };
    await this.#rpc.call(OP.CALL_ICE, encodeCallIce, decodeAcknowledged, request);
  }

  /**
   * Fetches short-lived TURN relay credentials for a call.
   *
   * For when direct P2P fails (symmetric NAT, corporate firewall, UDP blocked): credentials are
   * minted per call and never embedded in the client. v1 clients call P2P-only and do not fetch;
   * the method is here so the fallback path needs no SDK change.
   */
  async getTurnServers(callId: Id): Promise<TurnServer[]> {
    const response = await this.#rpc.call(
      OP.CALL_TURN_FETCH,
      encodeCallTurnFetch,
      decodeCallTurnResponse,
      { callId },
    );
    return response.servers;
  }

  /**
   * Reports aggregate call-quality numbers for a call.
   *
   * `CALL_STATS` is Droppable — a lost report costs nothing — and carries only aggregate numbers:
   * setup time, round-trip time, loss, jitter, whether TURN was used. Never any call content. The
   * fields are optional; send what this call measured and leave the rest unset.
   */
  async reportStats(callId: Id, stats: Partial<Omit<CallStats, 'callId'>>): Promise<void> {
    const request: CallStats = { callId, ...stats };
    await this.#rpc.call(OP.CALL_STATS, encodeCallStats, decodeAcknowledged, request);
  }
}
