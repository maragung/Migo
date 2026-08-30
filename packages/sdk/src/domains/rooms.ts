/**
 * The rooms domain: discover public rooms, join and leave them, and observe their membership and state.
 *
 * A room is a large, discoverable conversation. Joining one returns an ordinary {@link
 * ConversationSummary}-style handle — a `conversationId`, an {@link EncryptionMode}, and the current
 * `lastSeq` — so once joined a room is read and written through the same messaging domain as any other
 * conversation. What is distinct is the lifecycle around it: rooms are browsed and searched ({@link
 * list}), entered and left ({@link join} / {@link leave}), and they emit two live streams a direct
 * chat does not.
 *
 * # The two event streams
 *
 * {@link onMember} carries per-member joins and leaves; {@link onState} carries coalesced counters and
 * metadata — online count, member count, topic, slow-mode interval — as *deltas*, so a handler applies
 * each field it receives onto the room state it already holds rather than treating an event as a full
 * snapshot. Both are broadcast to every device in the room, so a client sees membership churn and
 * counter movement without polling. A joined room's encryption is still end-to-end where the summary
 * says so; discoverability changes who may enter, not whether content is sealed.
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
  #unsubscribes: Array<() => void> = [];

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#memberListeners = new ListenerSet(OP.ROOM_MEMBER_EVENT, onEventError);
    this.#stateListeners = new ListenerSet(OP.ROOM_STATE_EVENT, onEventError);
  }

  /** Begins delivering room member and state events to registered handlers. Idempotent. */
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
