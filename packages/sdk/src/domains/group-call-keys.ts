/**
 * The group-call frame-key domain: section 163's client-side triggers for a call's media key.
 *
 * The signaling domain ({@link GroupCallDomain}) moves the roster; this domain reacts to that
 * movement the way section 163 requires, because the *node* cannot — it never holds the keys:
 *
 *   1. **A participant joined or left** a call this device is seated in: the frame key rotates
 *      (the next epoch's key, sealed under the running one) and a `CALL_KEY_UPDATE` carries the
 *      sealed material to the roster. A leaver cannot decrypt what follows; a joiner cannot
 *      decrypt what came before.
 *   2. **This device joined a call already in progress**: it asks a seated participant for the
 *      running key — the request rides a `CALL_RENEGOTIATE` sealed under the pairwise session
 *      with the participant, and the answer (the running epoch and key, sealed under a wrapper
 *      key derived from that same session) rides a `CALL_SDP` back. Both halves live here: the
 *      joiner's ask and the seated participant's answer.
 *
 * # Why exactly one device rotates per membership change
 *
 * The frame key is *shared* — one key per epoch for the whole call — so if every seated client
 * minted a fresh key on the same membership change, two devices would produce two different keys
 * at the same epoch and {@link CallKeyState.adopt}'s never-regress refusal (the same rule the
 * message ratchets live by) would strand whichever half of the call heard the other key first.
 * The rotation therefore belongs to exactly one deterministic device: **the first seat in the
 * roster's join order**. Every seated client holds the same join-order projection (the snapshot's
 * verbatim order, announcements folded in arrival order), so every client computes the same
 * rotator without a vote. The rotating device advances its own state immediately and never
 * depends on hearing its own update back — the server does not relay a `CALL_KEY_UPDATE` to the
 * connection that sent it.
 *
 * A device that misses a rotation is stranded at its epoch until it re-joins the call; that is
 * the availability bound section 163 states for the sealed-update channel, not a secrecy break —
 * the stranded device cannot open what it cannot open.
 *
 * # The ask path, and why the request is a ratchet envelope
 *
 * The server relays a `CALL_RENEGOTIATE` to its target as a `CALL_SDP` and cannot tell a key
 * request from a codec renegotiation — that indistinguishability is the property that keeps the
 * request's *kind* off the wire's metadata. The request body is a full 1:1 Double Ratchet
 * envelope over a `call-key-ask` control event (not a bare AEAD blob under the join wrapper key):
 * a mid-call joiner may share no session with the participant it asks yet, and the ratchet
 * envelope carries the X3DH material that establishes one — after which both ends hold the same
 * session secret, which is the input the join wrapper key derives from. The control event inside
 * the envelope carries the joiner's account id as its `data`: the frame's own `fromDevice` names a
 * device, not an account, and the desktop holder's designated-rotator rule needs the account to
 * place the joiner in the roster's join order — an ask without it is silently ignored there. The
 * *answer* is the bare
 * sealed join distribution the crypto layer defines ({@link CallKeyState.sealedJoinDistribution}),
 * byte-compatible with every other client's, so a mixed-client call agrees on the reply even
 * where it disagrees on the ask.
 *
 * # Where the first key comes from
 *
 * {@link CallKeyState.fromSession} — the 1:1 derivation — cannot start a group call: the first
 * seat shares a pairwise session with nobody seated. The first seat mints
 * ({@link CallKeyState.create}); every participant after it receives the key through the ask path,
 * which is what makes a group call's key distributed rather than derived.
 */

import { idToBytes } from '@migo/wire';
import type { Id } from '@migo/wire';
import {
  OP,
  encodeCallKeyUpdate,
  decodeCallKeyUpdate,
  encodeCallRenegotiate,
  encodeCallSdp,
  decodeCallSdp,
  decodeAcknowledged,
} from '@migo/protocol';
import type { CallKeyUpdate, CallRenegotiate, CallSdp } from '@migo/protocol';

import { CallKeyState } from '../call-crypto.js';
import { ContentType, encodeContent, decodeContent } from '../content.js';
import { SdkError } from '../errors.js';
import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { SessionCrypto } from '../session-crypto.js';
import type { GroupCallDomain } from './group-calls.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * The control-event name a mid-call joiner's key request carries inside its ratchet envelope.
 *
 * A client-to-client constant, sealed before it leaves the device, so the server never sees it.
 */
export const CALL_KEY_ASK_EVENT = 'call-key-ask';

/** One seat, as this domain's join-order projection holds it. One seat per account is a server rule. */
interface Seat {
  userId: Id;
  deviceId: Id;
}

/** One tracked call: the seat projection, the frame key, and the ask/answer bookkeeping. */
interface TrackedCall {
  conversationId: Id;
  /** The roster in join order, exactly as the snapshot gave it and the announcements folded it. */
  seats: Seat[];
  /** The call's frame key, from the first seat's mint or a joiner's install. Null until either. */
  state: CallKeyState | null;
  /**
   * Joiner bookkeeping: the device this device asked for the running key. Null while seated
   * without asking (the first seat, or an ask still in flight).
   */
  distributor: Id | null;
  /**
   * Distributor bookkeeping: joiner devices this device has already answered. A duplicate ask is
   * re-answered with the running key but never rotates twice, and a join announcement for a
   * device already answered does not rotate either — the ask was the first news of that join, and
   * the rotation it triggered covered it.
   */
  answered: Set<Id>;
}

/**
 * The frame keys of the group calls this device is seated in.
 *
 * One instance per client, constructed alongside the signaling domain it listens to and started
 * with the others. It owns no UI state and touches no media: the frames it seals and opens are
 * the media plane's ({@link sealFrame} / {@link openFrame}), the key-state changes that tell the
 * media plane when sealing is possible and when a rotation has invalidated a negotiation in
 * flight are its {@link onKeyChanged}, and everything else it does is the key distribution
 * section 163 makes the client's own obligation.
 */
export class GroupCallKeysDomain {
  readonly #rpc: Rpc;
  readonly #groupCalls: GroupCallDomain;
  readonly #sessionCrypto: SessionCrypto;
  readonly #accountId: Id;
  readonly #deviceId: Id;
  readonly #onEventError: EventErrorHandler | undefined;

  readonly #calls = new Map<Id, TrackedCall>();
  readonly #keyListeners: ListenerSet<Id>;
  #unsubscribes: Array<() => void> = [];

  constructor(
    rpc: Rpc,
    groupCalls: GroupCallDomain,
    sessionCrypto: SessionCrypto,
    accountId: Id,
    deviceId: Id,
    onEventError?: EventErrorHandler,
  ) {
    this.#rpc = rpc;
    this.#groupCalls = groupCalls;
    this.#sessionCrypto = sessionCrypto;
    this.#accountId = accountId;
    this.#deviceId = deviceId;
    this.#onEventError = onEventError;
    // The opcode here only labels a failing handler in the error sink; the events this set carries
    // are this domain's own key-state changes, not wire frames.
    this.#keyListeners = new ListenerSet<Id>(OP.CALL_KEY_UPDATE, onEventError);
  }

  /** Begins tracking the seated calls' keys. Idempotent. */
  start(): void {
    if (this.#unsubscribes.length > 0) {
      return;
    }
    this.#unsubscribes.push(
      this.#groupCalls.onRoster((roster) => this.#onRoster(roster)),
      this.#groupCalls.onParticipantJoined((event) => this.#onJoined(event)),
      this.#groupCalls.onParticipantLeft((event) => this.#onLeft(event)),
      this.#rpc.on(OP.CALL_KEY_UPDATE, decodeCallKeyUpdate, (event) => this.#onKeyUpdate(event)),
      this.#rpc.on(OP.CALL_SDP, decodeCallSdp, (event) => this.#onSdp(event)),
    );
  }

  /** Stops tracking. Held keys are dropped with the subscriptions. */
  stop(): void {
    for (const unsubscribe of this.#unsubscribes) {
      unsubscribe();
    }
    this.#unsubscribes = [];
    this.#calls.clear();
  }

  /** Announces a call's key-state change to the registered listeners. */
  #announceKey(callId: Id): void {
    this.#keyListeners.deliver(callId);
  }

  // --- the media plane's surface ---

  /**
   * Registers a handler for this device's frame-key state changing. Returns its unsubscribe.
   *
   * Fires when a call's key first becomes held — the first seat's mint, or a joiner installing a
   * distributor's answer — and on every epoch advance after that, whether the advance was this
   * device's own rotation or an update it adopted. The media plane is the customer: it cannot
   * seal or open a call's signaling until a key is held, and a rotation invalidates the sealing
   * of anything still mid-negotiation — the epoch is the AEAD's associated data, so an offer
   * sealed under the old epoch no longer opens once the roster moved. This listener is how it
   * learns both moments without polling.
   */
  onKeyChanged(handler: Listener<Id>): () => void {
    return this.#keyListeners.add(handler);
  }

  /** Whether this device holds a frame key for the call. */
  hasKey(callId: Id): boolean {
    return this.#calls.get(callId)?.state !== undefined && this.#calls.get(callId)?.state !== null;
  }

  /** The epoch of the call's held frame key, or `null` when none is held. */
  keyEpoch(callId: Id): number | null {
    return this.#calls.get(callId)?.state?.epoch() ?? null;
  }

  /** Seals one media frame under the call's current key. Throws when no key is held. */
  sealFrame(callId: Id, frame: Uint8Array): Uint8Array {
    return this.#requireKey(callId).sealFrame(frame);
  }

  /** Opens one media frame under the call's current key. Throws when no key is held. */
  openFrame(callId: Id, sealed: Uint8Array): Uint8Array {
    return this.#requireKey(callId).openFrame(sealed);
  }

  /** Drops the tracked state for a call, for a caller that leaves by its own orchestration. */
  forget(callId: Id): void {
    this.#calls.delete(callId);
  }

  // --- trigger 3, joiner half: the roster snapshot decides ask versus mint ---

  /**
   * A roster snapshot is this account's own join made visible, so this is the one moment the
   * frame-key state for a call begins. A snapshot naming this account on *another* device is that
   * device's join — its keys are its own — and is ignored.
   *
   * Alone in the call, this device is the first seat and mints the key. With anyone seated, this
   * is a mid-call join: the running key already exists and this device cannot know it, so it asks
   * the first seated participant that is not this account — the same device every joiner picks,
   * because the snapshot's join order is the same fact every client holds. (A joiner's fresh seat
   * is appended at the end of the join order, so the first seat of a call in progress is always
   * another account's; the account filter is the guard that keeps the choice honest even in the
   * seat-replacement edge, where the seat being replaced is this account's own.)
   */
  #onRoster(roster: {
    callId: Id;
    conversationId: Id;
    userId: Id;
    deviceId: Id;
    participants: ReadonlyArray<{ userId: Id; deviceId: Id }>;
  }): void {
    if (roster.userId !== this.#accountId || roster.deviceId !== this.#deviceId) {
      return;
    }
    const seats: Seat[] = roster.participants.map((participant) => ({
      userId: participant.userId,
      deviceId: participant.deviceId,
    }));
    const entry: TrackedCall = {
      conversationId: roster.conversationId,
      seats,
      state: null,
      distributor: null,
      answered: new Set(),
    };
    this.#calls.set(roster.callId, entry);

    const distributor = seats.find((seat) => seat.userId !== this.#accountId);
    if (distributor === undefined) {
      // The first seat: no pairwise session exists to derive from (there is nobody seated to
      // share one with), so the key is minted and every later participant receives it by the ask
      // path below. See the class doc for why the 1:1 derivation cannot serve a group's first key.
      entry.state = CallKeyState.create(roster.callId);
      this.#announceKey(roster.callId);
      return;
    }
    entry.distributor = distributor.deviceId;
    this.#sendAsk(roster.callId, entry, distributor);
  }

  /**
   * Sends the joiner's sealed key request to the participant the snapshot's order chose.
   *
   * The request carries the joiner's account id as the control event's `data` — the one fact the
   * frame's own `fromDevice` cannot say (an id names a device, not an account), and the one the
   * desktop holder's designated-rotator rule needs to place the joiner in the roster's join order.
   * Desktop's holder requires the field (`let bytes = data?`), so an ask without it is silently
   * ignored there — byte-identical shapes keep a mixed-client call answering.
   */
  #sendAsk(callId: Id, entry: TrackedCall, distributor: Seat): void {
    void this.#sessionCrypto
      .seal(
        entry.conversationId,
        this.#deviceId,
        distributor.userId,
        distributor.deviceId,
        encodeContent({
          type: ContentType.ControlEvent,
          event: CALL_KEY_ASK_EVENT,
          data: idToBytes(this.#accountId),
        }),
      )
      .then((sealed) =>
        // The ask rides the renegotiation frame the call's relay already owns: from the server's
        // side a key request is indistinguishable from a codec renegotiation, and a separate
        // opcode would name its kind in the metadata.
        this.#rpc.call(OP.CALL_RENEGOTIATE, encodeCallRenegotiate, decodeAcknowledged, {
          callId,
          fromDevice: this.#deviceId,
          toDevice: distributor.deviceId,
          sealedSdp: sealed.envelope,
        } satisfies CallRenegotiate),
      )
      .catch((cause: unknown) => {
        this.#onEventError?.(OP.CALL_RENEGOTIATE, cause);
      });
  }

  // --- trigger 2: membership movement in a call this device is seated in ---

  /**
   * A participant joined. The seat list folds the arrival (an account already seated is replaced,
   * not doubled — one seat per account), and the rotator — the first seat in the post-fold join
   * order — rotates and announces. A join whose ask this device already answered carries no new
   * fact: the rotation that ask triggered already covered it.
   */
  #onJoined(event: { callId: Id; userId: Id; deviceId: Id }): void {
    const entry = this.#calls.get(event.callId);
    if (entry === undefined) {
      return;
    }
    const alreadyAnswered = entry.answered.has(event.deviceId);
    entry.seats = [
      ...entry.seats.filter((seat) => seat.userId !== event.userId),
      { userId: event.userId, deviceId: event.deviceId },
    ];
    if (event.userId === this.#accountId || alreadyAnswered) {
      // Our own account's movement is the seat-replacement pair the departure below finishes, and
      // an answered ask already rotated for this joiner; neither mints a second key.
      return;
    }
    this.#rotateIfRotator(event.callId, entry);
  }

  /**
   * A participant left. The seat folds away; the retirement (the last seat gone) drops the call's
   * state with it. A departure naming this account *on this device* is the seat being withdrawn
   * from under the session (replaced from this account's other device) — this device is no longer
   * in the call and holds no further duty in it.
   */
  #onLeft(event: { callId: Id; userId: Id; deviceId: Id; participantCount: number }): void {
    const entry = this.#calls.get(event.callId);
    if (entry === undefined) {
      return;
    }
    if (event.userId === this.#accountId && event.deviceId === this.#deviceId) {
      this.#calls.delete(event.callId);
      return;
    }
    entry.seats = entry.seats.filter((seat) => seat.userId !== event.userId);
    if (event.participantCount === 0) {
      this.#calls.delete(event.callId);
      return;
    }
    this.#rotateIfRotator(event.callId, entry);
  }

  /**
   * The single rotator's duty: mint the next epoch, sealed under the running key, and hand the
   * sealed material to the whole roster. Every other seated client computes the same rotator from
   * the same join order and waits — see the class doc for why two minting devices at one epoch
   * cannot converge.
   */
  #rotateIfRotator(callId: Id, entry: TrackedCall): void {
    if (!this.#namesRotatorSeat(entry) || entry.state === null) {
      return;
    }
    const sealed = entry.state.rotate();
    const epoch = entry.state.epoch();
    this.#announceKey(callId);
    void this.#rpc
      .call(OP.CALL_KEY_UPDATE, encodeCallKeyUpdate, decodeAcknowledged, {
        callId,
        epoch,
        sealedKeyMaterial: sealed,
      } satisfies CallKeyUpdate)
      .catch((cause: unknown) => {
        this.#onEventError?.(OP.CALL_KEY_UPDATE, cause);
      });
  }

  /** Whether the first seat in the join order is this device's own. */
  #namesRotatorSeat(entry: TrackedCall): boolean {
    const first = entry.seats[0];
    return (
      first !== undefined && first.userId === this.#accountId && first.deviceId === this.#deviceId
    );
  }

  // --- trigger 2, receiving half: adopting the announced rotation ---

  /**
   * A rotation announcement from the rotator. A replay (an epoch at or below the held one) is
   * dropped — the refusal is the mechanism — and a material this device cannot open means it
   * missed an earlier update; both leave the held key working, and the second is surfaced to the
   * event-error sink because a stranded seat is a fact a caller may want to act on (a re-join
   * re-seats and re-asks).
   */
  #onKeyUpdate(event: CallKeyUpdate): void {
    const entry = this.#calls.get(event.callId);
    if (entry === undefined || entry.state === null) {
      return;
    }
    if (event.epoch <= entry.state.epoch()) {
      return;
    }
    try {
      entry.state.adopt(event.epoch, event.sealedKeyMaterial);
      this.#announceKey(event.callId);
    } catch (cause) {
      this.#onEventError?.(OP.CALL_KEY_UPDATE, cause);
    }
  }

  // --- trigger 3, both halves, on the one opcode the relay owns ---

  /**
   * One `CALL_SDP` addressed to this device, carrying either a joiner's ask (the relay projects a
   * `CALL_RENEGOTIATE` to its target as `CALL_SDP`) or another participant's answer to this
   * device's own ask. Ordinary call signaling rides the same opcode; anything that does not open
   * as this domain's own frames is left for it, silently.
   */
  #onSdp(event: CallSdp): void {
    if (event.toDevice !== this.#deviceId) {
      return;
    }
    const entry = this.#calls.get(event.callId);
    if (entry === undefined) {
      return;
    }
    this.#maybeAnswerAsk(event, entry);
    this.#maybeInstallAnswer(event, entry);
  }

  /**
   * The seated half: an ask from a joiner, if that is what this frame is.
   *
   * The ask is a 1:1 ratchet envelope over the pairwise session this device shares with the
   * joiner — the envelope that *establishes* that session when none exists, which is why the ask
   * is not sealed under the join wrapper key itself. Opening it commits the session, so the
   * session secret the wrapper key derives from is held on both ends by the time the answer is
   * sealed.
   *
   * The answer carries the *running* key. When the ask is the first news of the join (the join
   * announcement has not landed yet), the rotator rotates first, so the joiner's first key is one
   * that did not exist while they were outside the call — section 163's "the distributor is
   * expected to rotate on the join". A duplicate ask from the same device is re-answered with the
   * running key but never rotates twice.
   */
  #maybeAnswerAsk(event: CallSdp, entry: TrackedCall): void {
    if (entry.state === null) {
      // Seated without a key yet (this device's own ask is still in flight): nothing to hand out.
      return;
    }
    const asker = entry.seats.find((seat) => seat.deviceId === event.fromDevice);
    let isAsk = false;
    try {
      const plaintext = this.#sessionCrypto.open(
        entry.conversationId,
        asker?.userId ?? event.fromDevice,
        event.fromDevice,
        event.sealedSdp,
      );
      const content = decodeContent(plaintext);
      isAsk = content.type === ContentType.ControlEvent && content.event === CALL_KEY_ASK_EVENT;
    } catch {
      // Not an ask for this device — an SDP relay the signaling domain owns, or fan-out noise.
      return;
    }
    if (!isAsk) {
      return;
    }

    if (!entry.answered.has(event.fromDevice)) {
      entry.answered.add(event.fromDevice);
      if (asker === undefined && this.#namesRotatorSeat(entry)) {
        // The asker is not yet in this device's seat projection, so the ask beat the announcement:
        // this is the join's first news, and the rotation the join owes happens now. Only the
        // rotator mints (the class doc's single-rotator rule); a non-rotator's answer simply
        // carries the running key, and the rotator's own announcement-driven rotation covers it.
        this.#rotateIfRotator(event.callId, entry);
      }
    }

    const secret = this.#sessionCrypto.sessionSecret(entry.conversationId, event.fromDevice);
    if (secret === null) {
      // The open above committed a session, so this is unreachable in practice; kept honest
      // rather than asserting, because the cost of a dropped answer is one re-ask.
      return;
    }
    const sealedJoin = entry.state.sealedJoinDistribution(secret);
    void this.#rpc
      .call(OP.CALL_SDP, encodeCallSdp, decodeAcknowledged, {
        callId: event.callId,
        fromDevice: this.#deviceId,
        toDevice: event.fromDevice,
        sealedSdp: sealedJoin,
      } satisfies CallSdp)
      .catch((cause: unknown) => {
        this.#onEventError?.(OP.CALL_SDP, cause);
      });
  }

  /**
   * The joiner half: the running key, sealed for this device, if that is what this frame is.
   *
   * The answer opens under the join wrapper key derived from the session this device shares with
   * the participant it asked — the same session the ask established — and the first distribution
   * is the baseline (there is no earlier epoch to compare against). A later answer that names a
   * higher epoch re-baselines the same way, so a re-ask after the distributor rotated still
   * converges; one that does not advance changes nothing.
   */
  #maybeInstallAnswer(event: CallSdp, entry: TrackedCall): void {
    if (entry.distributor !== event.fromDevice) {
      return;
    }
    const secret = this.#sessionCrypto.sessionSecret(entry.conversationId, event.fromDevice);
    if (secret === null) {
      return;
    }
    let received: CallKeyState;
    try {
      received = CallKeyState.fromJoinDistribution(secret, event.callId, event.sealedSdp);
    } catch {
      // Not a key answer — an SDP relay for this call, sealed the signaling way. Ours to ignore.
      return;
    }
    if (entry.state !== null && received.epoch() <= entry.state.epoch()) {
      return;
    }
    entry.state = received;
    this.#announceKey(event.callId);
  }

  /** The held key for a call, or a thrown error naming the call when none is held. */
  #requireKey(callId: Id): CallKeyState {
    const state = this.#calls.get(callId)?.state;
    if (state === undefined || state === null) {
      throw new SdkError(`group-call-keys: no frame key held for call ${callId}`);
    }
    return state;
  }
}
