/**
 * Export-all: replaying every conversation's history into logs.
 *
 * A conversation's decrypted transcript exists in React state only while its window is open, so
 * "export every chat" cannot read a store — it must re-fetch each conversation's history and
 * replay it through the same decrypt-and-deliver path the thread uses (`sync.fetch` backwards
 * pages, each event through `MessagingDomain.ingest`). A temporary `onMessage` listener collects
 * what the replay delivers; it is registered before the first fetch and removed after the last,
 * so it sees exactly this conversation's replay and nothing else.
 *
 * # The honest bounds
 *
 *   * Pages are bounded ({@link EXPORT_PAGE} × {@link MAX_EXPORT_PAGES} per conversation): an
 *     export is a foreground action with a progress line, not a background sync, and a
 *     conversation longer than the budget exports its newest messages with the cut stated in the
 *     UI rather than hanging the export on an unbounded walk.
 *   * A message whose key distribution has not replayed yet is buffered by the messaging layer
 *     and may surface after this conversation's pass; it is simply absent from that
 *     conversation's log. The same is true of the thread itself, which also renders only what
 *     has opened.
 *
 * The seam is a narrow interface ({@link ChatLogClient}), so tests can drive the replay with a
 * double instead of a live socket, the same shape of honesty the profile cache's
 * `ProfileClient` carries.
 */

import type {
  ConversationSummary,
  Id,
  IncomingMessage,
  MessageEvent,
  UserProfile,
} from '@migo/sdk';

import { buildConversationLog } from '../chat-logs.js';
import type { ConversationLog } from '../chat-logs.js';

/** How many events one backwards page asks the server for. */
const EXPORT_PAGE = 200;
/** How many pages one conversation's export walks at most. */
const MAX_EXPORT_PAGES = 10;

/** The slice of the client the replay needs: history fetch, decrypt delivery, and profile names. */
export interface ChatLogClient {
  readonly sync: {
    fetch(
      conversationId: Id,
      haveSeq: number,
      limit: number,
      options: { backwards?: boolean },
    ): Promise<{ fromSeq: number; toSeq: number; more: boolean; messages: MessageEvent[] }>;
  };
  readonly messaging: {
    onMessage(handler: (message: IncomingMessage) => void): () => void;
    ingest(event: MessageEvent): void;
  };
  readonly profile: {
    fetch(userIds: Id[]): Promise<UserProfile[]>;
  };
}

/**
 * Replays one conversation's history into a log.
 *
 * The walk mirrors the thread's own "Load earlier" (lib/migo/use-chat.ts): pages fetch backwards
 * from the newest, each event is ingested so it decrypts through the shared path, and the cursor
 * continues downward from each page's `fromSeq`. Sender names resolve after the walk, in one
 * batched profile fetch over exactly the senders the replay collected.
 */
export async function collectConversationLog(
  client: ChatLogClient,
  summary: ConversationSummary,
  selfId: Id | null,
): Promise<ConversationLog> {
  const collected: IncomingMessage[] = [];
  // The replay can deliver one message twice — once buffered from an earlier load and once in a
  // fetched page — the same way the thread's own `upsert` deduplicates by id. A log is a
  // transcript, not an audit of the transport, so the first delivery wins and repeats drop.
  const seen = new Set<Id>();
  const stop = client.messaging.onMessage((message) => {
    if (message.conversationId === summary.conversationId && !seen.has(message.messageId)) {
      seen.add(message.messageId);
      collected.push(message);
    }
  });
  try {
    let cursor = 0;
    for (let page = 0; page < MAX_EXPORT_PAGES; page += 1) {
      const response = await client.sync.fetch(summary.conversationId, cursor, EXPORT_PAGE, {
        backwards: true,
      });
      for (const event of response.messages) {
        client.messaging.ingest(event);
      }
      cursor = response.fromSeq;
      if (!response.more) {
        break;
      }
    }
  } finally {
    stop();
  }

  const senders = [...new Set(collected.map((message) => message.senderId))];
  const profiles = await client.profile.fetch(senders);
  const names = new Map<Id, string>(
    profiles.map((profile) => [profile.userId, profile.displayName]),
  );
  const nameOf = (senderId: Id): string => {
    if (selfId !== null && senderId === selfId) {
      return 'You';
    }
    return names.get(senderId) ?? 'Unknown';
  };

  return buildConversationLog(summary.conversationId, summary.title ?? 'Chat', collected, nameOf);
}

/**
 * Replays every conversation's history into logs, one conversation at a time.
 *
 * Sequential on purpose: the messaging layer's decrypt delivery is shared state, and two
 * concurrent replays would interleave their pages through the same listener registry for no gain —
 * the export's cost is the network, which one conversation at a time already keeps busy. A
 * conversation whose replay throws does not end the export; it is skipped, and its absence is the
 * caller's to report.
 */
export async function collectAllConversationLogs(
  client: ChatLogClient,
  summaries: readonly ConversationSummary[],
  selfId: Id | null,
): Promise<ConversationLog[]> {
  const logs: ConversationLog[] = [];
  for (const summary of summaries) {
    try {
      logs.push(await collectConversationLog(client, summary, selfId));
    } catch {
      // See the doc above: one conversation's failure skips it, not the export.
    }
  }
  return logs;
}
