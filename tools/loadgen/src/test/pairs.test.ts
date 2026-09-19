/**
 * The two doors every direct-pair scenario goes through, in the order they are knocked on.
 *
 * The first is privacy. A freshly registered account answers `who_can_message = friends`, and the
 * messaging service enforces that setting at the conversation's door and again on every send, so
 * two strangers cannot open a direct conversation at all. That is what the first honest full-scale
 * run measured: four steps that connected every session, held their entire window, and did none of
 * the work they are named for, with `remote:PRIVACY_RESTRICTED` on every `setup` operation. The
 * opening therefore has to happen *before* the first conversation is created, it has to reach both
 * roles of a pair, and it must not reach VUs that no pair named — the connect scenario's ten
 * thousand idle sessions are measured for the cost of holding a session, and a profile write per
 * session would be this scenario's work leaking into that one.
 *
 * The second door is the conversation itself, and the rule asserted here is the one that keeps a
 * half-formed pair measurable: a refusal is counted as a `setup` error and the other pairs still
 * form, rather than the whole scenario losing its run to one account's bad luck.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ConversationKind, TransportError } from '@migo/sdk';

import { Logger } from '../logger.js';
import { openDirectConversations, openPrivacyToStrangers } from '../pairs.js';
import type { VuPair } from '../pairs.js';
import { RunContext } from '../run-context.js';
import { Metrics } from '../stats.js';
import type { VirtualUser } from '../virtual-user.js';

const QUIET = new Logger('quiet');
const future = (): number => performance.now() + 60_000;

/** The value the visibility fields share for "everyone" (`0 nobody / 1 friends / 2 everyone`). */
const EVERYONE = 2;

interface VuHooks {
  connected?: boolean;
  /** Every `updateProfile` patch this VU was handed, in order. */
  onPrivacy?: (patch: Record<string, unknown>) => void;
  privacyFails?: boolean;
  startFails?: boolean;
  /** Called when this VU starts a conversation, with the members it addressed. */
  onStart?: (members: readonly unknown[]) => void;
}

/**
 * A VirtualUser whose client records only what the pair setup asks of it.
 *
 * The `timeline` array is shared by every double in a test and is what makes the ordering
 * assertion possible: both the privacy writes and the conversation creations append to it, so the
 * test can see that no conversation was attempted before every opening was written.
 */
function makeVu(index: number, timeline: string[], hooks: VuHooks = {}): VirtualUser {
  const client = {
    accountId: `acct-${index}`,
    profile: {
      updateProfile: (patch: Record<string, unknown>): Promise<unknown> => {
        timeline.push(`privacy:${index}`);
        hooks.onPrivacy?.(patch);
        return hooks.privacyFails
          ? Promise.reject(new TransportError('profile update refused'))
          : Promise.resolve({ userId: `acct-${index}` });
      },
    },
    startConversation: (kind: ConversationKind, members: readonly unknown[]) => {
      timeline.push(`start:${index}`);
      hooks.onStart?.(members);
      return hooks.startFails
        ? Promise.reject(new TransportError('start refused'))
        : Promise.resolve({ conversationId: `conv-${index}` });
    },
    watchConversation: (): Promise<void> => Promise.resolve(),
  };
  return {
    index,
    connected: hooks.connected ?? true,
    partner: undefined,
    conversationId: undefined,
    client,
  } as unknown as VirtualUser;
}

test('every paired VU is opened to messages from strangers before the first conversation exists', async () => {
  const timeline: string[] = [];
  const patches: Record<string, unknown>[] = [];
  const a = makeVu(0, timeline, { onPrivacy: (patch) => patches.push(patch) });
  const b = makeVu(1, timeline);
  const c = makeVu(2, timeline);
  const d = makeVu(3, timeline);
  const pairs: VuPair[] = [
    { sender: a, receiver: b },
    { sender: c, receiver: d },
  ];
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());

  await openDirectConversations(pairs, ctx);

  // The gate is the recipient's setting and the send path re-asks it, so both roles are written —
  // a scenario that later sends the other way must not discover the door by walking into it.
  assert.deepEqual(timeline.filter((event) => event.startsWith('privacy:')).sort(), [
    'privacy:0',
    'privacy:1',
    'privacy:2',
    'privacy:3',
  ]);
  assert.ok(
    timeline.lastIndexOf('privacy:') < timeline.indexOf('start:'),
    `a conversation was attempted before the opening was written: ${timeline.join(', ')}`,
  );
  // One field, on the documented patch surface: the value is "everyone", and nothing else about
  // the account is re-stated (an absent field is left untouched, so a wider patch would be the
  // harness volunteering opinions nobody asked it for).
  assert.deepEqual(patches, [{ whoCanMessage: EVERYONE }, { whoCanMessage: EVERYONE }]);
  assert.equal(metrics.operation('setup').errors, 0);
  assert.equal(a.conversationId, 'conv-0');
  assert.equal(c.conversationId, 'conv-2');
});

test('a VU that two pairs share is opened once, and a VU in no pair never at all', async () => {
  const timeline: string[] = [];
  // `shared` is on both sides of two pairs, so it would be written twice by a naive walk of the
  // pair list; `idle` is connected and named by nothing, which is the shape of every VU in the
  // connect scenario — ten thousand sessions whose profile this setup must not touch.
  const shared = makeVu(0, timeline);
  const one = makeVu(1, timeline);
  const two = makeVu(2, timeline);
  const idle = makeVu(3, timeline);
  const pairs: VuPair[] = [
    { sender: shared, receiver: one },
    { sender: shared, receiver: two },
  ];
  const ctx = new RunContext(new Metrics(), QUIET, 0, future());

  await openPrivacyToStrangers(pairs, ctx);

  // The idle VU is a live, connected session that no pair names — the shape every VU has in the
  // connect scenario — and it is absent from the timeline for that reason alone.
  assert.equal(idle.connected, true, 'the idle VU is connected; being unnamed is what spares it');
  assert.deepEqual(timeline, ['privacy:0', 'privacy:1', 'privacy:2']);
});

test('a refused opening is counted as a setup error and its pair is still attempted', async () => {
  const timeline: string[] = [];
  const a = makeVu(0, timeline, { privacyFails: true });
  const b = makeVu(1, timeline);
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());

  // Not thrown: one VU's refusal must not cost the run the pairs that could still form.
  await assert.doesNotReject(() => openDirectConversations([{ sender: a, receiver: b }], ctx));

  assert.equal(metrics.operation('setup').errors, 1);
  assert.deepEqual(metrics.operation('setup').errorsByClass, [['transport', 1]]);
  assert.ok(
    timeline.includes('start:0'),
    'the pair is attempted anyway, and fails on its own terms',
  );
});
