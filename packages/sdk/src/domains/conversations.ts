/**
 * The conversations domain: list the conversations this account is in, create new ones, and run the
 * group lifecycle around them.
 *
 * Direct chats are the thin case: {@link create} is *idempotent* on its member set, so creating "the
 * conversation with Alice" twice returns the same conversation rather than a second one — the server
 * derives a deterministic id from the sorted member ids, which is what lets a client call {@link
 * create} freely whenever it needs the conversation to exist before sending.
 *
 * # The group lifecycle
 *
 * A group has two founders — the creator and the first person they named — and the founders are the
 * group's memory of who built it. {@link invite} is every member's right; {@link mute}, {@link kick},
 * and {@link rename} are the founders'; {@link voteKick} is the members' own recourse, carrying at
 * half the roster rounded up. Leaving ({@link leave}) is nobody's to gate: when the last founder
 * walks out, the longest-standing member inherits the role, so a group never reaches a state where
 * nobody can rename it or answer a report.
 *
 * Three live streams follow from {@link start}: {@link onMember} for joins, departures, and removals
 * — rotate sender keys on every one of these, exactly as for a room — {@link onVote} for a running
 * kick tally, and {@link onState} for coalesced metadata deltas such as a rename. All three arrive
 * only on a conversation the client has subscribed to through {@link
 * MigoClient.watchConversation}.
 *
 * The summaries this returns carry an {@link EncryptionMode} the UI may display, but that field is a
 * claim about transport, not a substitute for the end-to-end guarantee: content is sealed by the
 * crypto layers regardless of what mode a summary advertises.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  ConversationKind,
  encodeConversationListRequest,
  decodeConversationListResponse,
  encodeConversationCreateRequest,
  decodeConversationSummary,
  encodeConversationInviteRequest,
  encodeConversationLeaveRequest,
  encodeConversationRosterRequest,
  decodeConversationRosterResponse,
  encodeConversationMuteRequest,
  encodeConversationKickRequest,
  encodeConversationVoteKickRequest,
  decodeConversationVoteKickResponse,
  encodeConversationUpdateRequest,
  decodeConversationMemberEvent,
  decodeConversationVoteEvent,
  decodeConversationStateEvent,
  decodeAcknowledged,
} from '@migo/protocol';
import type {
  ConversationListRequest,
  ConversationListResponse,
  ConversationCreateRequest,
  ConversationSummary,
  ConversationInviteRequest,
  ConversationLeaveRequest,
  ConversationRosterRequest,
  ConversationRosterResponse,
  ConversationRosterEntry,
  ConversationMuteRequest,
  ConversationKickRequest,
  ConversationVoteKickRequest,
  ConversationVoteKickResponse,
  ConversationUpdateRequest,
  ConversationMemberEvent,
  ConversationVoteEvent,
  ConversationStateEvent,
  Acknowledged,
} from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/** Optional creation parameters beyond the member set. */
export interface CreateConversationOptions {
  /** A title, meaningful for group conversations; ignored by the server for direct ones. */
  title?: string;
}

/**
 * List, create, and run the lifecycle of conversations.
 *
 * One instance per client. {@link start} begins delivering the three group event streams; the
 * request/response methods work independently of it.
 */
export class ConversationsDomain {
  readonly #rpc: Rpc;
  readonly #memberListeners: ListenerSet<ConversationMemberEvent>;
  readonly #voteListeners: ListenerSet<ConversationVoteEvent>;
  readonly #stateListeners: ListenerSet<ConversationStateEvent>;
  #unsubscribes: Array<() => void> = [];

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#memberListeners = new ListenerSet(OP.CONVERSATION_MEMBER_EVENT, onEventError);
    this.#voteListeners = new ListenerSet(OP.CONVERSATION_VOTE_EVENT, onEventError);
    this.#stateListeners = new ListenerSet(OP.CONVERSATION_STATE_EVENT, onEventError);
  }

  /** Begins delivering group member, vote, and state events. Idempotent. */
  start(): void {
    if (this.#unsubscribes.length > 0) {
      return;
    }
    this.#unsubscribes.push(
      this.#rpc.on(OP.CONVERSATION_MEMBER_EVENT, decodeConversationMemberEvent, (event) =>
        this.#memberListeners.deliver(event),
      ),
      this.#rpc.on(OP.CONVERSATION_VOTE_EVENT, decodeConversationVoteEvent, (event) =>
        this.#voteListeners.deliver(event),
      ),
      this.#rpc.on(OP.CONVERSATION_STATE_EVENT, decodeConversationStateEvent, (event) =>
        this.#stateListeners.deliver(event),
      ),
    );
  }

  /** Stops delivering conversation events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    for (const unsubscribe of this.#unsubscribes) {
      unsubscribe();
    }
    this.#unsubscribes = [];
  }

  /**
   * Registers a handler for group membership movement: joins, departures, and removals. Returns an
   * unsubscribe function.
   *
   * Membership churn is a crypto event before it is a UI one — rotate the conversation's sender key
   * on every one of these, exactly as for a room, so a removed member cannot read what is sent next.
   */
  onMember(handler: Listener<ConversationMemberEvent>): () => void {
    return this.#memberListeners.add(handler);
  }

  /**
   * Registers a handler for group kick-vote tallies. Returns an unsubscribe function.
   *
   * Each event names the target and the count so far against the threshold; `closed` marks the vote's
   * end (it passed, expired, or the target walked out). The stream is coalesced per conversation, so a
   * handler holds one tally per target and replaces it as counts arrive rather than accumulating.
   */
  onVote(handler: Listener<ConversationVoteEvent>): () => void {
    return this.#voteListeners.add(handler);
  }

  /**
   * Registers a handler for coalesced group metadata deltas. Returns an unsubscribe function.
   *
   * Each event carries only the fields that changed — today, a `title` when a founder renames the
   * group. Apply them onto held state rather than replacing it.
   */
  onState(handler: Listener<ConversationStateEvent>): () => void {
    return this.#stateListeners.add(handler);
  }

  /**
   * Lists the account's conversations, newest activity first.
   *
   * Pass the {@link ConversationListResponse.nextCursor} from a previous page as `cursor` to fetch the
   * next; a response with no `nextCursor` is the last page. `limit` bounds one page.
   */
  async list(limit: number, cursor?: string): Promise<ConversationListResponse> {
    const request: ConversationListRequest = { limit };
    if (cursor !== undefined) {
      request.cursor = cursor;
    }
    return this.#rpc.call(
      OP.CONVERSATION_LIST,
      encodeConversationListRequest,
      decodeConversationListResponse,
      request,
    );
  }

  /**
   * Creates a conversation, or returns the existing one for a direct chat.
   *
   * `members` is the *other* participants; the server adds the caller. For a {@link
   * ConversationKind.Direct} chat this is idempotent — the same two members always resolve to the same
   * conversation — so callers may use it as "ensure the conversation exists" before a first send.
   */
  async create(
    kind: ConversationKind,
    members: Id[],
    options: CreateConversationOptions = {},
  ): Promise<ConversationSummary> {
    const request: ConversationCreateRequest = { kind, members };
    if (options.title !== undefined) {
      request.title = options.title;
    }
    return this.#rpc.call(
      OP.CONVERSATION_CREATE,
      encodeConversationCreateRequest,
      decodeConversationSummary,
      request,
    );
  }

  /**
   * Adds members to a group, resolving with the group's summary as it now stands.
   *
   * Any current member may invite; the new seats arrive as plain members. `members` holds account ids
   * — to invite by username, resolve the name to an id first (an account search), then pass the id
   * here. Already-seated members and the caller are quietly skipped, and each person who actually
   * landed is announced to the group on the {@link onMember} stream, so the roster stays true without
   * a refetch. Not the group's founders, so the group always has at least one.
   */
  async invite(conversationId: Id, members: Id[]): Promise<ConversationSummary> {
    const request: ConversationInviteRequest = { conversationId, members };
    return this.#rpc.call(
      OP.CONVERSATION_INVITE,
      encodeConversationInviteRequest,
      decodeConversationSummary,
      request,
    );
  }

  /**
   * Leaves a group. Nobody's permission is asked — leaving is a right, not a request.
   *
   * After leaving, forget the conversation's crypto state through the messaging domain, exactly as for
   * a room: the group rotates its sender key on the departure, and the local receiver state is no
   * longer useful. When the last founder leaves, the longest-standing member silently inherits the
   * role, so the group never ends up with nobody able to rename it or answer a report.
   */
  async leave(conversationId: Id): Promise<Acknowledged> {
    const request: ConversationLeaveRequest = { conversationId };
    return this.#rpc.call(
      OP.CONVERSATION_LEAVE,
      encodeConversationLeaveRequest,
      decodeAcknowledged,
      request,
    );
  }

  /**
   * Reads a group's roster: active members first by join time, then the departed.
   *
   * Each {@link ConversationRosterEntry} carries the account, its {@link ConversationRole}, when it
   * joined, and any group mute still running against them. Departed members carry a `leftAt`, which is
   * how a UI renders "was here" without pretending history is deletable. This is a snapshot for
   * rendering, not a mirror to maintain — live movement arrives on {@link onMember}.
   */
  async getRoster(conversationId: Id): Promise<ConversationRosterEntry[]> {
    const request: ConversationRosterRequest = { conversationId };
    const response: ConversationRosterResponse = await this.#rpc.call(
      OP.CONVERSATION_ROSTER,
      encodeConversationRosterRequest,
      decodeConversationRosterResponse,
      request,
    );
    return response.entries;
  }

  /**
   * Mutes or unmutes one member of a group — a founder's action, not a vote.
   *
   * While the mute runs, the target cannot send to the group; they keep every other right, including
   * the vote. Omit `until` to lift a mute early; pass a future epoch-milliseconds timestamp to set
   * one. Founders are beyond each other's reach, and neither the caller nor a founder may be the
   * target. There is no event for this — the roster is the record — so a client that needs to show
   * the change refetches the roster.
   */
  async mute(conversationId: Id, targetId: Id, until?: number): Promise<Acknowledged> {
    const request: ConversationMuteRequest = { conversationId, targetId };
    if (until !== undefined) {
      request.until = until;
    }
    return this.#rpc.call(
      OP.CONVERSATION_MUTE,
      encodeConversationMuteRequest,
      decodeAcknowledged,
      request,
    );
  }

  /**
   * Removes a member outright, no vote — a founder's action.
   *
   * The other founder is beyond this reach: a group built by two cannot be halved by one of them. The
   * removal is announced to the group on the {@link onMember} stream, and the group rotates its sender
   * key, so rotate local crypto state exactly as for any membership change.
   */
  async kick(conversationId: Id, targetId: Id): Promise<Acknowledged> {
    const request: ConversationKickRequest = { conversationId, targetId };
    return this.#rpc.call(
      OP.CONVERSATION_KICK,
      encodeConversationKickRequest,
      decodeAcknowledged,
      request,
    );
  }

  /**
   * Casts a voice to kick a member by vote, resolving with the tally after this voice landed.
   *
   * The vote is the members' own recourse, needing no rank: the first call opens the vote, each
   * further member's call adds to it, and when {@link ConversationVoteKickResponse.votes} reaches
   * `needed` — half the group, rounded up — the kick lands and `open` turns false. A caller's
   * repeated voice is idempotent, a muted member still votes, and founders are immune (a group's
   * founders are answerable to nobody but each other). One vote may run per group at a time, and a
   * vote nobody finishes expires after a minute. The same tally is broadcast on the {@link onVote}
   * stream, so a member who voted and one who only watched converge on the same count.
   */
  async voteKick(conversationId: Id, targetId: Id): Promise<ConversationVoteKickResponse> {
    const request: ConversationVoteKickRequest = { conversationId, targetId };
    return this.#rpc.call(
      OP.CONVERSATION_VOTE_KICK,
      encodeConversationVoteKickRequest,
      decodeConversationVoteKickResponse,
      request,
    );
  }

  /**
   * Renames a group, resolving with the summary carrying the new title.
   *
   * A founder's action. The new title travels to every member on the {@link onState} stream as a
   * coalesced delta, so a client applies it onto the summary it already holds. Direct conversations
   * carry no title to change.
   */
  async rename(conversationId: Id, title: string): Promise<ConversationSummary> {
    const request: ConversationUpdateRequest = { conversationId, title };
    return this.#rpc.call(
      OP.CONVERSATION_UPDATE,
      encodeConversationUpdateRequest,
      decodeConversationSummary,
      request,
    );
  }
}
