/**
 * The workloads a run can drive, and the registry that names them.
 *
 * A {@link Scenario} is two phases. `prepare` runs once after every VU is connected, to wire up
 * shared state that steady-state work depends on — pairing conversation partners, subscribing the
 * receiving side. `workloads` then returns one paced loop per active VU; the runner races them all
 * against the deadline. Splitting it this way keeps setup cost (which is real, but one-time) out of
 * the steady-state latency numbers.
 */

import { ContentType, ConversationKind, PresenceState } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { runPool } from './pool.js';
import type { RunContext } from './run-context.js';
import { classifyError } from './stats.js';
import type { VirtualUser } from './virtual-user.js';

/** A per-VU loop that runs, self-paced, until the run's deadline. */
export type Workload = (ctx: RunContext) => Promise<void>;

export interface Scenario {
  readonly name: string;
  readonly description: string;
  /** Fewest VUs the scenario is meaningful with (messaging needs a pair). */
  readonly minVus: number;
  prepare(vus: readonly VirtualUser[], ctx: RunContext): Promise<void>;
  workloads(vus: readonly VirtualUser[]): Workload[];
}

/** Conversations to open concurrently during messaging setup — enough to be quick, not a stampede. */
const SETUP_CONCURRENCY = 16;

/**
 * Hold N concurrent gateway sessions open for the duration.
 *
 * There is no steady-state operation: connecting and staying connected *is* the workload. It answers
 * "how many simultaneous sessions can a node carry, and how does connection setup latency behave as
 * the pool grows" — the connect-phase digest the runner records for every scenario is the payload.
 */
const connect: Scenario = {
  name: 'connect',
  description: 'Register and hold N concurrent gateway sessions for the duration.',
  minVus: 1,
  async prepare() {
    // Nothing to wire up: the sessions are already open, and holding them is the whole test.
  },
  workloads() {
    return [];
  },
};

/**
 * Every VU flips its presence between Online and Away at the target rate.
 *
 * Presence fan-out is a distinct server path from messaging — no conversation, no sealing, but a
 * broadcast to every subscriber — so it is worth loading on its own.
 */
const presence: Scenario = {
  name: 'presence',
  description: 'Every VU flips presence Online/Away at the target rate.',
  minVus: 1,
  async prepare() {
    // Each VU already subscribes to its own user topic on connect; nothing more is needed.
  },
  workloads(vus) {
    return vus
      .filter((vu) => vu.connected)
      .map((vu) => {
        let online = true;
        return (ctx: RunContext) =>
          ctx.paceLoop(() => {
            const state = online ? PresenceState.Online : PresenceState.Away;
            online = !online;
            return ctx.measure('presence', () => vu.client.presence.setPresence(state));
          });
      });
  },
};

/**
 * Pairs of VUs hold a direct end-to-end conversation; the even VU of each pair streams messages.
 *
 * `prepare` pairs adjacent connected VUs, has the sender open the conversation (which distributes
 * the sender key and subscribes it), and has the receiver watch it so the inbound decrypt path is
 * exercised too. Steady state then measures true send-to-ack latency — the first send folds in
 * sender-key distribution, exactly as a real first message would.
 */
const messaging: Scenario = {
  name: 'messaging',
  description: 'Pairs of VUs hold a direct E2E conversation; senders stream sealed messages.',
  minVus: 2,
  async prepare(vus, ctx) {
    const pairs: Array<{ sender: VirtualUser; receiver: VirtualUser }> = [];
    for (let i = 0; i + 1 < vus.length; i += 2) {
      const sender = vus[i];
      const receiver = vus[i + 1];
      if (sender?.connected === true && receiver?.connected === true)
        pairs.push({ sender, receiver });
    }
    if (vus.filter((vu) => vu.connected).length % 2 === 1) {
      ctx.log.warn('an odd number of VUs connected; one has no partner and will stay idle');
    }

    await runPool(pairs, SETUP_CONCURRENCY, async ({ sender, receiver }) => {
      try {
        const summary = await sender.client.startConversation(ConversationKind.Direct, [
          receiver.client.accountId,
        ]);
        sender.conversationId = summary.conversationId;
        sender.partner = receiver;
        await receiver.client.watchConversation(summary.conversationId);
      } catch (error) {
        ctx.metrics.recordError('setup', classifyError(error));
        ctx.log.debug(`pair ${sender.index}/${receiver.index} setup failed`);
      }
    });
  },
  workloads(vus) {
    // Narrow to senders that actually hold a conversation, capturing the id so the loop needs no
    // non-null assertion later.
    const senders: Array<{ vu: VirtualUser; conversationId: Id }> = [];
    for (const vu of vus) {
      if (vu.connected && vu.conversationId !== undefined) {
        senders.push({ vu, conversationId: vu.conversationId });
      }
    }
    return senders.map(({ vu, conversationId }) => {
      let seq = 0;
      return (ctx: RunContext) =>
        ctx.paceLoop(() => {
          seq += 1;
          const text = `loadgen ${vu.index} #${seq}`;
          return ctx.measure('send', () =>
            vu.client.messaging.send(conversationId, { type: ContentType.Text, text }),
          );
        });
    });
  },
};

const REGISTRY = new Map<string, Scenario>([
  [connect.name, connect],
  [presence.name, presence],
  [messaging.name, messaging],
]);

export function getScenario(name: string): Scenario | undefined {
  return REGISTRY.get(name);
}

export function scenarioNames(): string[] {
  return [...REGISTRY.keys()];
}
