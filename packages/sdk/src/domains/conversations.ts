/**
 * The conversations domain: list the conversations this account is in, and create new ones.
 *
 * This is a thin request/response domain — no crypto and no events. The interesting property lives in
 * {@link create}: a direct (two-person) conversation is *idempotent* on its member set, so creating
 * "the conversation with Alice" twice returns the same conversation rather than a second one. The
 * server derives a deterministic id from the sorted member ids for direct chats, which is what lets a
 * client call {@link create} freely whenever it needs the conversation to exist before sending.
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
} from '@migo/protocol';
import type {
  ConversationListRequest,
  ConversationListResponse,
  ConversationCreateRequest,
  ConversationSummary,
} from '@migo/protocol';

import type { Rpc } from './rpc.js';

/** Optional creation parameters beyond the member set. */
export interface CreateConversationOptions {
  /** A title, meaningful for group conversations; ignored by the server for direct ones. */
  title?: string;
}

/**
 * List and create conversations.
 *
 * One instance per client. Stateless beyond its {@link Rpc}: every call is a fresh round trip, so a
 * caller that wants a cached view keeps it themselves.
 */
export class ConversationsDomain {
  readonly #rpc: Rpc;

  constructor(rpc: Rpc) {
    this.#rpc = rpc;
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
}
