/**
 * Pairing and conversation setup shared by every scenario whose unit is a two-party conversation.
 *
 * Extracted from the messaging scenario when section 172's full-scale shapes (calls, voice notes,
 * the outage integrity run) turned out to need the exact same wiring: pair adjacent connected
 * VUs, open one direct E2E conversation per pair, and count failures as `setup` errors rather
 * than throwing — a scenario whose half-finished pairs still produce a meaningful measurement
 * must not lose the pairs that did form to one refusal.
 */

import { ConversationKind } from '@migo/sdk';

import { runPool } from './pool.js';
import type { RunContext } from './run-context.js';
import { classifyError, describeError } from './stats.js';
import type { VirtualUser } from './virtual-user.js';

/** Conversations to open concurrently during setup — enough to be quick, not a stampede. */
const SETUP_CONCURRENCY = 16;

/** One two-party unit of work: the even VU is the sender, the odd VU the receiver. */
export interface VuPair {
  readonly sender: VirtualUser;
  readonly receiver: VirtualUser;
}

/**
 * Pairs adjacent connected VUs, for every scenario whose unit is a two-party conversation.
 *
 * The pairing is positional (0+1, 2+3, …) and skips the disconnected; an odd count simply leaves
 * the last VU idle, which the caller may warn about.
 */
export function pairUp(vus: readonly VirtualUser[]): VuPair[] {
  const pairs: VuPair[] = [];
  for (let i = 0; i + 1 < vus.length; i += 2) {
    const sender = vus[i];
    const receiver = vus[i + 1];
    if (sender?.connected === true && receiver?.connected === true)
      pairs.push({ sender, receiver });
  }
  return pairs;
}

/**
 * Opens one direct E2E conversation per pair: the sender starts it (which distributes the sender
 * key and subscribes the sender) and the receiver watches it, so the inbound decrypt path is
 * exercised too. Sets each sender's `conversationId` and `partner`; failures are tallied under
 * `setup`, never thrown.
 */
export async function openDirectConversations(
  pairs: readonly VuPair[],
  ctx: RunContext,
): Promise<void> {
  await runPool(pairs, SETUP_CONCURRENCY, async ({ sender, receiver }) => {
    try {
      const summary = await sender.client.startConversation(ConversationKind.Direct, [
        receiver.client.accountId,
      ]);
      sender.conversationId = summary.conversationId;
      sender.partner = receiver;
      await receiver.client.watchConversation(summary.conversationId);
    } catch (error) {
      ctx.metrics.recordError('setup', classifyError(error), describeError(error));
      ctx.log.debug(`pair ${sender.index}/${receiver.index} setup failed`);
    }
  });
}
