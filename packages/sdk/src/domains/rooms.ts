/**
 * The rooms domain: discover public rooms, join and leave them, and observe their membership and state.
 *
 * A room is a large, discoverable conversation. Joining one returns an ordinary {@link
 * ConversationSummary}-style handle — a `conversationId`, an {@link EncryptionMode}, and the current
 * `lastSeq` — so once joined a room is read and written through the same messaging domain as any other
 * conversation. What is distinct is the lifecycle around it: rooms are browsed and searched ({@link
 * list}), entered and left ({@link join} / {@link leave}), and they emit live streams a direct
 * chat does not.
 *
 * # The three event streams
 *
 * {@link onMember} carries per-member joins and leaves; {@link onState} carries coalesced counters and
 * metadata — online count, member count, topic, slow-mode interval — as *deltas*, so a handler applies
 * each field it receives onto the room state it already holds rather than treating an event as a full
 * snapshot. {@link onVote} carries the running tally of a kick vote, coalesced per room, so every member
 * watches the same count climb toward the threshold. All three are broadcast to every device in the
 * room, so a client sees membership churn, counter movement, and vote progress without polling. A
 * joined room's encryption is still end-to-end where the summary says so; discoverability changes who
 * may enter, not whether content is sealed.
 *
 * # Moderation
 *
 * Two paths remove trouble. {@link voteKick} is the members' own recourse: any member calls for a kick,
 * and when half the room has cast the same call the target is removed — the reply and the {@link onVote}
 * stream both report the tally. {@link sanction} is the staff path: a member with rank enough (a
 * moderator, or a global admin) mutes, kicks, or bans a lower-ranked member outright, no vote needed.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  encodeRoomJoinRequest,
  decodeRoomJoinResponse,
  encodeRoomLeaveRequest,
  encodeRoomListRequest,
  decodeRoomListResponse,
  encodeRoomCreate,
  encodeRosterReq,
  decodeRosterResponse,
  decodeRoomMemberEvent,
  decodeRoomStateEvent,
  decodeRoomVoteEvent,
  encodeRoomVoteKick,
  decodeRoomVoteKickResponse,
  encodeRoomSanction,
  decodeAcknowledged,
} from '@migo/protocol';
import type {
  RoomCreate,
  RoomJoinRequest,
  RoomJoinResponse,
  RoomLeaveRequest,
  RoomListRequest,
  RoomListResponse,
  RoomMemberEvent,
  RoomStateEvent,
  RoomVoteEvent,
  RoomVoteKick,
  RoomVoteKickResponse,
  RoomSanction,
  SanctionAction,
  RosterEntry,
  RosterReq,
  RosterResponse,
  Acknowledged,
  RoomKind,
} from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/** Optional filters for a room search. Any combination narrows the listing. */
export interface RoomListFilter {
  /** A free-text query matched against room name, topic, and description. */
  query?: string;
  /** Restrict to a category. */
  category?: string;
  /** Restrict to a language code. */
  language?: string;
  /** Restrict to a country code. */
  country?: string;
  /** The {@link RoomListResponse.nextCursor} from a previous page. */
  cursor?: string;
}

/**
 * Browse, join, leave, and observe rooms.
 *
 * One instance per client. {@link start} begins delivering the two event streams; {@link list},
 * {@link join}, and {@link leave} work independently of it.
 */
export class RoomsDomain {
  readonly #rpc: Rpc;
  readonly #memberListeners: ListenerSet<RoomMemberEvent>;
  readonly #stateListeners: ListenerSet<RoomStateEvent>;
  readonly #voteListeners: ListenerSet<RoomVoteEvent>;
  #unsubscribes: Array<() => void> = [];

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#memberListeners = new ListenerSet(OP.ROOM_MEMBER_EVENT, onEventError);
    this.#stateListeners = new ListenerSet(OP.ROOM_STATE_EVENT, onEventError);
    this.#voteListeners = new ListenerSet(OP.ROOM_VOTE_EVENT, onEventError);
  }

  /** Begins delivering room member, state, and vote events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribes.length > 0) {
      return;
    }
    this.#unsubscribes.push(
      this.#rpc.on(OP.ROOM_MEMBER_EVENT, decodeRoomMemberEvent, (event) =>
        this.#memberListeners.deliver(event),
      ),
      this.#rpc.on(OP.ROOM_STATE_EVENT, decodeRoomStateEvent, (event) =>
        this.#stateListeners.deliver(event),
      ),
      this.#rpc.on(OP.ROOM_VOTE_EVENT, decodeRoomVoteEvent, (event) =>
        this.#voteListeners.deliver(event),
      ),
    );
  }

  /** Stops delivering room events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    for (const unsubscribe of this.#unsubscribes) {
      unsubscribe();
    }
    this.#unsubscribes = [];
  }

  /** Registers a handler for per-member join and leave events. Returns an unsubscribe function. */
  onMember(handler: Listener<RoomMemberEvent>): () => void {
    return this.#memberListeners.add(handler);
  }

  /**
   * Registers a handler for coalesced room-state deltas. Returns an unsubscribe function.
   *
   * Each event carries only the fields that changed; apply them onto held state rather than replacing
   * it.
   */
  onState(handler: Listener<RoomStateEvent>): () => void {
    return this.#stateListeners.add(handler);
  }

  /**
   * Registers a handler for kick-vote tallies. Returns an unsubscribe function.
   *
   * Each event names the target and the count so far against the threshold; `closed` marks the vote's
   * end (it passed, expired, or the target left). The stream is coalesced per room, so a handler holds
   * one tally per target and replaces it as counts arrive rather than accumulating.
   */
  onVote(handler: Listener<RoomVoteEvent>): () => void {
    return this.#voteListeners.add(handler);
  }

  /**
   * Searches discoverable rooms.
   *
   * `limit` bounds one page; pass {@link RoomListResponse.nextCursor} as {@link RoomListFilter.cursor}
   * to page. A response with no `nextCursor` is the last page.
   */
  async list(limit: number, filter: RoomListFilter = {}): Promise<RoomListResponse> {
    const request: RoomListRequest = { limit };
    if (filter.query !== undefined) {
      request.query = filter.query;
    }
    if (filter.category !== undefined) {
      request.category = filter.category;
    }
    if (filter.language !== undefined) {
      request.language = filter.language;
    }
    if (filter.country !== undefined) {
      request.country = filter.country;
    }
    if (filter.cursor !== undefined) {
      request.cursor = filter.cursor;
    }
    return this.#rpc.call(OP.ROOM_LIST, encodeRoomListRequest, decodeRoomListResponse, request);
  }

  /**
   * Creates a room and enters it, resolving with the same join handle {@link join} returns.
   *
   * The caller becomes the room's Owner ({@link RoomRole.Owner}): the one role that can appoint
   * managers and the one a room cannot lose. `slug` is the room's permanent address and must be
   * unique server-side; `kind` picks the governance line — {@link RoomKind.Public} for a
   * community room, {@link RoomKind.Managed} for one under server moderation. The reply is a
   * join response because creation is entry: the creator is the first member.
   */
  async create(
    slug: string,
    name: string,
    kind: RoomKind,
    topic?: string,
  ): Promise<RoomJoinResponse> {
    const request: RoomCreate = { slug, name, kind };
    if (topic !== undefined) {
      request.topic = topic;
    }
    return this.#rpc.call(OP.ROOM_CREATE, encodeRoomCreate, decodeRoomJoinResponse, request);
  }

  /**
   * Joins a room, optionally with an invite code for a non-public one.
   *
   * Resolves with the room summary plus the conversation handle to read and write it through the
   * messaging domain: its `conversationId`, its `encryption` mode, and the current `lastSeq` to start
   * syncing from.
   */
  async join(roomId: Id, inviteCode?: string): Promise<RoomJoinResponse> {
    const request: RoomJoinRequest = { roomId };
    if (inviteCode !== undefined) {
      request.inviteCode = inviteCode;
    }
    return this.#rpc.call(OP.ROOM_JOIN, encodeRoomJoinRequest, decodeRoomJoinResponse, request);
  }

  /**
   * Leaves a room.
   *
   * After leaving, forget the room's crypto state through the messaging domain — a room rotates its
   * sender key on membership change so departed members cannot read new messages, and the local
   * receiver state for it is no longer useful.
   */
  async leave(roomId: Id): Promise<Acknowledged> {
    const request: RoomLeaveRequest = { roomId };
    return this.#rpc.call(OP.ROOM_LEAVE, encodeRoomLeaveRequest, decodeAcknowledged, request);
  }

  /**
   * Casts a vote to kick a member, resolving with the tally after this voice landed.
   *
   * The vote is the members' own recourse, needing no rank: the first call opens the vote, each
   * further member's call adds to it, and when {@link RoomVoteKickResponse.votes} reaches `needed`
   * — half the room, rounded up — the kick lands and `open` turns false. A caller's repeated vote is
   * idempotent. The same tally is broadcast to the room on the {@link onVote} stream, so a client
   * that voted and one that only watched converge on the same count.
   */
  async voteKick(roomId: Id, targetId: Id): Promise<RoomVoteKickResponse> {
    const request: RoomVoteKick = { roomId, targetId };
    return this.#rpc.call(
      OP.ROOM_VOTE_KICK,
      encodeRoomVoteKick,
      decodeRoomVoteKickResponse,
      request,
    );
  }

  /**
   * Applies a moderation action to a member: mute, unmute, kick, ban, or unban.
   *
   * This is the staff path, not the vote: the server admits it only from a caller who outranks the
   * target (a room moderator or above) or a global admin, and rejects it otherwise. `action` is a
   * {@link SanctionAction}; a room mute silences the target for the room (distinct from a caller's
   * personal mute), a kick removes with the door open, a ban bars re-entry, and their undos reverse
   * each. `reason` is an optional note the server may record. The kept-in-sync membership then
   * arrives on the ordinary {@link onMember} stream as a {@link MemberChange}.
   */
  async sanction(args: {
    roomId: Id;
    targetId: Id;
    action: SanctionAction;
    reason?: string;
  }): Promise<Acknowledged> {
    const request: RoomSanction = {
      roomId: args.roomId,
      targetId: args.targetId,
      action: args.action,
    };
    if (args.reason !== undefined) {
      request.reason = args.reason;
    }
    return this.#rpc.call(OP.ROOM_SANCTION, encodeRoomSanction, decodeAcknowledged, request);
  }

  /**
   * Reads a room's roster, highest role first.
   *
   * Each {@link RosterEntry} carries the account, its {@link RoomRole} as a number, and when it
   * joined. `limit` bounds one page; `after` is the cursor — the last `accountId` of the previous
   * page — so a caller pages until a short page arrives. Unlike the live {@link onMember} stream,
   * this is a snapshot for rendering a member list, not a mirror to maintain.
   */
  async getRoster(roomId: Id, limit?: number, after?: Id): Promise<RosterEntry[]> {
    const request: RosterReq = { roomId };
    if (limit !== undefined) {
      request.limit = limit;
    }
    if (after !== undefined) {
      request.after = after;
    }
    const response: RosterResponse = await this.#rpc.call(
      OP.ROOM_ROSTER,
      encodeRosterReq,
      decodeRosterResponse,
      request,
    );
    return response.members;
  }
}
