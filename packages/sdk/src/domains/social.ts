/**
 * The social domain: friendships, blocks, and finding people.
 *
 * # The graph is server-owned, the client only mirrors it
 *
 * A friendship is a server-side edge between two accounts, and every mutation here — request,
 * respond, block — is a *request* that the server apply a rule it owns: who may message whom, whether
 * the recipient's privacy settings admit the request at all, whether either side is on the other's
 * block list. The client never derives relationship state locally, because a client-side guess would
 * drift from the server's the moment either party acted from another device. Instead the graph is
 * re-read ({@link listRelationships}) whenever {@link onFriendEvent} reports that something changed.
 *
 * # Discovery is prefix search, not a directory
 *
 * {@link search} matches a username prefix and {@link suggestions} returns accounts the graph thinks
 * are relevant (mutual friends). Both answer with {@link SuggestedUser} — public profile fields only.
 * Nothing here can enumerate accounts wholesale: a short or empty query is rejected or clamped
 * server-side, which is the enumeration defence a public directory would otherwise need.
 *
 * # Blocks are one-sided and silent
 *
 * {@link blockUser} sets a Block edge. Blocking is not a friendship teardown dialogue: the wire
 * carries no notification to the blocked party, and the block list is only ever visible to the
 * blocker through their own relationship list.
 *
 * # Mutes are quieter still
 *
 * {@link muteUser} sets a Mute edge — a personal, unnotified choice like a block, but lighter: it
 * neither tears down a friendship nor bars a message, it only marks an account the caller would
 * rather not hear from so the UI can hide that account's room chatter. The muted set is read back
 * as a filtered view of the relationship list ({@link mutedAccounts}), the same server-owned graph
 * a block lives in.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  RelationshipKind,
  encodeFriendTarget,
  encodeFriendRespond,
  encodeRelationshipListReq,
  decodeRelationshipList,
  encodeSuggestReq,
  encodeSearchReq,
  encodeMuteSet,
  decodeSearchResponse,
  decodeFriendEvent,
  decodeAcknowledged,
} from '@migo/protocol';
import type {
  FriendEvent,
  FriendRespond,
  FriendTarget,
  MuteSet,
  RelationshipEntry,
  RelationshipListReq,
  SearchReq,
  SearchResponse,
  SuggestReq,
  SuggestedUser,
} from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * The relationship page {@link listRelationships} asks for when the caller does not bound it.
 *
 * The wire takes a required limit, and the server treats it as a ceiling on the combined listing so a
 * large graph cannot flood a frame. Two hundred covers a personal social graph with room to spare;
 * a caller with a genuinely larger one pages explicitly.
 */
const DEFAULT_RELATIONSHIP_LIMIT = 200;

/**
 * Manage friendships and blocks, discover people, and observe friendship changes.
 *
 * One instance per client. {@link start} begins delivering {@link FriendEvent}s (which arrive on the
 * caller's own user topic, already subscribed by the client); the request methods work without it.
 */
export class SocialDomain {
  readonly #rpc: Rpc;
  readonly #friendListeners: ListenerSet<FriendEvent>;
  #unsubscribe: (() => void) | null = null;

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#friendListeners = new ListenerSet(OP.FRIEND_EVENT, onEventError);
  }

  /** Begins delivering friend events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribe !== null) {
      return;
    }
    this.#unsubscribe = this.#rpc.on(OP.FRIEND_EVENT, decodeFriendEvent, (event) =>
      this.#friendListeners.deliver(event),
    );
  }

  /** Stops delivering friend events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }

  /**
   * Registers a handler for friendship changes. Returns an unsubscribe function.
   *
   * An event names the other account and a `state` string (`"request"`, `"accepted"`); it is a hint
   * that the graph moved, not a source of truth — re-read {@link listRelationships} to draw the
   * right buttons, since the event carries no direction (incoming vs outgoing) and no removal state.
   */
  onFriendEvent(handler: Listener<FriendEvent>): () => void {
    return this.#friendListeners.add(handler);
  }

  /**
   * Sends a friend request.
   *
   * Resolves when the server has accepted the request *for delivery* — the recipient still has to
   * answer it, and a request the recipient's privacy settings forbid is rejected here with an error.
   * A repeated request while one is pending is idempotent server-side.
   */
  async friendRequest(userId: Id): Promise<void> {
    const request: FriendTarget = { userId };
    await this.#rpc.call(OP.FRIEND_REQUEST, encodeFriendTarget, decodeAcknowledged, request);
  }

  /**
   * Answers a pending friend request.
   *
   * `accept: false` declines (or withdraws an already-declined request); the edge simply disappears
   * from the graph either way. Only the *recipient* of a request may respond to it.
   */
  async friendRespond(userId: Id, accept: boolean): Promise<void> {
    const request: FriendRespond = { userId, accept };
    await this.#rpc.call(OP.FRIEND_RESPOND, encodeFriendRespond, decodeAcknowledged, request);
  }

  /**
   * Blocks an account.
   *
   * One-sided and unnotified: the blocked account is not told, and the block shows only in the
   * blocker's own {@link listRelationships}. Blocking also tears down any friendship between the two
   * server-side, so a caller that holds the relationship list should refresh it after this resolves.
   */
  async blockUser(userId: Id): Promise<void> {
    const request: FriendTarget = { userId };
    await this.#rpc.call(OP.BLOCK_SET, encodeFriendTarget, decodeAcknowledged, request);
  }

  /**
   * Mutes or unmutes an account for the caller.
   *
   * A personal, one-sided choice: `on: true` sets a Mute edge, `on: false` clears it, and neither
   * touches friendship or delivery — a muted account's messages still arrive, the client simply
   * hides that account's room chatter for the muter. Like a block it is unnotified and visible only
   * to the caller. A caller that holds the muted set should refresh it after this resolves.
   */
  async muteUser(userId: Id, on: boolean): Promise<void> {
    const request: MuteSet = { userId, on };
    await this.#rpc.call(OP.MUTE_SET, encodeMuteSet, decodeAcknowledged, request);
  }

  /**
   * Reads the accounts the caller has muted, as a plain list of ids.
   *
   * A thin projection over {@link listAllRelationships}: it reads the whole graph and keeps only the
   * Mute edges, since the wire carries mutes mixed in with friends, blocks, and the rest rather than
   * as a list of their own. The result is the set a client loads once at session start and consults
   * to hide muted voices.
   */
  async mutedAccounts(): Promise<Id[]> {
    const KIND_MUTE: number = RelationshipKind.Mute;
    const entries = await this.listAllRelationships();
    return entries.filter((entry) => entry.kind === KIND_MUTE).map((entry) => entry.userId);
  }

  /**
   * Reads the caller's relationship graph: friends, pending requests in both directions, follows,
   * and blocks, each as a {@link RelationshipEntry} whose `kind` is a {@link RelationshipKind} value.
   *
   * The list is the caller's own view (the block list of another account is never served), so a
   * caller re-reads it rather than maintaining a local mirror. `limit` bounds the combined result;
   * the default covers a personal graph.
   */
  async listRelationships(
    limit: number = DEFAULT_RELATIONSHIP_LIMIT,
  ): Promise<RelationshipEntry[]> {
    const request: RelationshipListReq = { limit };
    const response = await this.#rpc.call(
      OP.RELATIONSHIP_LIST,
      encodeRelationshipListReq,
      decodeRelationshipList,
      request,
    );
    return response.entries;
  }

  /**
   * Reads the caller's whole relationship graph in one unfiltered list: friends, pending
   * requests in both directions, follows and followers, blocks, and favourites.
   *
   * The wire is the same {@link RELATIONSHIP_LIST} call {@link listRelationships} makes, but the
   * client bounds nothing: `limit` rides as zero, which the server reads as "apply your own page",
   * so every kind the graph holds comes back mixed together and the *caller* filters by `kind`.
   * This is the form for a caller that wants the blocks and favourites alongside the friends
   * without naming a page size, where {@link listRelationships} is the form for a bounded read.
   */
  async listAllRelationships(): Promise<RelationshipEntry[]> {
    const request: RelationshipListReq = { limit: 0 };
    const response = await this.#rpc.call(
      OP.RELATIONSHIP_LIST,
      encodeRelationshipListReq,
      decodeRelationshipList,
      request,
    );
    return response.entries;
  }

  /**
   * Friend suggestions: accounts the graph considers relevant, strongest first.
   *
   * Each result carries a mutual-friend count, which is the only signal the server is willing to
   * expose about *why* an account was suggested. `limit` is clamped server-side; omit it for the
   * server's default page.
   */
  async suggestions(limit?: number): Promise<SuggestedUser[]> {
    const request: SuggestReq = {};
    if (limit !== undefined) {
      request.limit = limit;
    }
    const response: SearchResponse = await this.#rpc.call(
      OP.SUGGESTIONS,
      encodeSuggestReq,
      decodeSearchResponse,
      request,
    );
    return response.results;
  }

  /**
   * Searches public profiles by username prefix.
   *
   * The match is a prefix match on the username (display names are not searched), and the query is
   * required: there is deliberately no "list everyone" form, since that would be a directory dump
   * rather than a lookup. Results carry only public profile fields.
   */
  async search(query: string, limit?: number): Promise<SuggestedUser[]> {
    const request: SearchReq = { query };
    if (limit !== undefined) {
      request.limit = limit;
    }
    const response = await this.#rpc.call(
      OP.SEARCH,
      encodeSearchReq,
      decodeSearchResponse,
      request,
    );
    return response.results;
  }
}
