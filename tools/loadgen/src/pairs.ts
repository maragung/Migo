/**
 * Pairing and conversation setup shared by every scenario whose unit is a two-party conversation.
 *
 * Extracted from the messaging scenario when section 172's full-scale shapes (calls, voice notes,
 * the outage integrity run) turned out to need the exact same wiring: open every paired VU to
 * messages from a stranger, pair adjacent connected VUs, open one direct E2E conversation per
 * pair, and count failures as `setup` errors rather than throwing — a scenario whose half-finished
 * pairs still produce a meaningful measurement must not lose the pairs that did form to one
 * refusal.
 */

import { ConversationKind } from '@migo/sdk';

import { runPool } from './pool.js';
import type { RunContext } from './run-context.js';
import { classifyError, describeError } from './stats.js';
import type { VirtualUser } from './virtual-user.js';

/** Conversations to open concurrently during setup — enough to be quick, not a stampede. */
const SETUP_CONCURRENCY = 16;

/**
 * The `who_can_message` value that admits a stranger, in the 0 nobody / 1 friends / 2 everyone
 * numbering the visibility fields share (`clients/web`'s privacy screen offers the same three).
 */
const VISIBILITY_EVERYONE = 2;

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
  await openPrivacyToStrangers(pairs, ctx);
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

/**
 * Opens every paired VU's `who_can_message` to everyone, because a load run's pairing needs it and
 * a freshly registered account does not have it.
 *
 * Registration writes `who_can_message = friends` (`migo-auth`'s own default), and the messaging
 * service enforces that setting twice: at the door, where `create_conversation` refuses a direct
 * conversation the recipient's setting does not admit, and again on every send, so a setting
 * tightened later withholds the next message rather than being a setting about the past. Two
 * throwaway accounts registered a moment ago are strangers to each other, so *every* direct-pair
 * scenario was refused with `PRIVACY_RESTRICTED` before a conversation existed — which is the gap
 * the judge named in the first honest full-scale run, where four steps connected every session,
 * held their whole window, and did none of the work they are named for:
 *
 *   judge: step 'calls': the scenario "calls" counted no successful operation of its own — 1000
 *   success(es) in lifecycle phases and 0 at the work it is named for, which is a run that
 *   connected and then did nothing [no-measurement]
 *
 * This is a user's setting being set through the SDK's own `updateProfile`, not a policy being
 * bypassed: "Everyone" is a value the product offers, and a harness measuring message relay has to
 * say who may message it. `tools/nnode`'s sync check reaches the same door from the other side —
 * it seeds the friendship rows a direct message needs — on the shared principle that a fixture the
 * product already knows how to express beats a scenario that measures its own refusal.
 *
 * Both roles are written, not only the receiver: the gate is the recipient's setting, and which VU
 * is the recipient is a scenario's business rather than this function's.
 */
export async function openPrivacyToStrangers(
  pairs: readonly VuPair[],
  ctx: RunContext,
): Promise<void> {
  const vus = new Set<VirtualUser>();
  for (const { sender, receiver } of pairs) {
    vus.add(sender);
    vus.add(receiver);
  }
  await runPool([...vus], SETUP_CONCURRENCY, async (vu) => {
    try {
      await vu.client.profile.updateProfile({ whoCanMessage: VISIBILITY_EVERYONE });
    } catch (error) {
      ctx.metrics.recordError('setup', classifyError(error), describeError(error));
      ctx.log.debug(`VU ${vu.index} could not open who_can_message: ${describeError(error)}`);
    }
  });
}
