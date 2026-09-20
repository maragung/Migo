/**
 * The four doors every direct-pair scenario goes through, in the order they are knocked on.
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
 *
 * The third is the call policy, and it is a separate door rather than a second reading of the
 * first: `who_can_message` gates a message and `who_can_call_voice` gates a ring, the call columns
 * are the user's own to set, and the server answers a refusal with `BLOCKED` in an ordinary reply
 * instead of an error — which is how a calls step came to report four thousand placed calls on a
 * run where no callee was ever rung. It is opened for both call kinds before any call is placed,
 * and it is asserted here because nothing else in the harness would notice it going missing.
 *
 * The fourth is the receiver's own membership, and it is the one nobody knocked on twice: the
 * receiver used to *watch* the conversation it had just been invited to and nothing more, which
 * subscribes a topic and leaves the membership cache empty. The SDK rotates and redistributes the
 * sender key on the invite event that follows, that redistribution needs an audience, and an
 * audience lookup on an unprimed conversation throws — the `sdk` wall every step of the nightly
 * carried, exactly once per receiving device. The receiver lists now, as a real client's connect
 * does, and both halves of that are asserted below: that the list is asked for, and that a list
 * which does not carry the conversation fails here as a `setup` error rather than surfacing one
 * event later as a membership error.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ConversationKind, TransportError } from '@migo/sdk';

import { Logger } from '../logger.js';
import { openCallsToStrangers, openDirectConversations, openPrivacyToStrangers } from '../pairs.js';
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
  onProfile?: (patch: Record<string, unknown>) => void;
  profileFails?: boolean;
  startFails?: boolean;
  /** Called when this VU starts a conversation, with the members it addressed. */
  onStart?: (members: readonly unknown[]) => void;
  /**
   * The conversation ids this VU's list answers with.
   *
   * Defaults to the conversation the pair's sender created — the even VU immediately to its left,
   * whose `startConversation` mints `conv-<its index>` — because that is the fixture's whole point
   * for the receiver role: a client invited to a conversation, listing, and finding it there.
   */
  list?: readonly string[];
}

/**
 * A VirtualUser whose client records only what the pair setup asks of it.
 *
 * The `timeline` array is shared by every double in a test and is what makes the ordering
 * assertion possible: the profile writes, the conversation creations and the receivers' list reads
 * all append to it, so the test can see that no conversation was attempted before every opening was
 * written, and that a receiver listed before it was asked to hold anything.
 */
function makeVu(index: number, timeline: string[], hooks: VuHooks = {}): VirtualUser {
  const client = {
    accountId: `acct-${index}`,
    profile: {
      updateProfile: (patch: Record<string, unknown>): Promise<unknown> => {
        timeline.push(`profile:${index}`);
        hooks.onProfile?.(patch);
        return hooks.profileFails
          ? Promise.reject(new TransportError('profile update refused'))
          : Promise.resolve({ userId: `acct-${index}` });
      },
    },
    // Bound but unused: the double records that a conversation was started and who it addressed,
    // and the pair setup only ever opens Direct ones — the kind is asserted where it belongs, in
    // the scenario tests, rather than re-asserted here through a parameter this file never reads.
    startConversation: (_kind: ConversationKind, members: readonly unknown[]) => {
      timeline.push(`start:${index}`);
      hooks.onStart?.(members);
      return hooks.startFails
        ? Promise.reject(new TransportError('start refused'))
        : Promise.resolve({ conversationId: `conv-${index}` });
    },
    // Bound but unused by the pair setup, which lists rather than watches; kept so the double is
    // still the shape the SDK's client is, and so a test that reached for it would find a function
    // rather than an `undefined`.
    watchConversation: (): Promise<void> => Promise.resolve(),
    loadConversations: (): Promise<{ conversations: { conversationId: string }[] }> => {
      timeline.push(`list:${index}`);
      const ids = hooks.list ?? [`conv-${index - 1}`];
      return Promise.resolve({
        conversations: ids.map((conversationId) => ({ conversationId })),
      });
    },
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
  const a = makeVu(0, timeline, { onProfile: (patch) => patches.push(patch) });
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
  assert.deepEqual(timeline.filter((event) => event.startsWith('profile:')).sort(), [
    'profile:0',
    'profile:1',
    'profile:2',
    'profile:3',
  ]);
  // By prefix, not by exact match: every entry carries an index (`profile:0`, `start:2`), so
  // `indexOf('profile:')` finds nothing and compares -1 against -1 — an assertion that can only
  // fail, which is how CI read it. A failing assertion is the cheap half of that mistake; the
  // expensive half would have been a passing one.
  const firstStart = timeline.findIndex((event) => event.startsWith('start:'));
  const lastOpening = timeline.reduce(
    (last, event, index) => (event.startsWith('profile:') ? index : last),
    -1,
  );
  assert.ok(
    firstStart !== -1 && lastOpening < firstStart,
    `a conversation was attempted before the opening was written: ${timeline.join(', ')}`,
  );
  // One entry, not one per pair: `patches` collects VU 0's writes alone, because VU 0 is the only
  // double this test hands an `onProfile` hook. The two-roles-per-pair claim is the timeline above,
  // which names all four VUs; what this line pins is the patch itself: one field, on the documented
  // surface, the value "everyone", and nothing else about the account re-stated (an absent field is
  // left untouched, so a wider patch would be the harness volunteering opinions nobody asked it
  // for). Two entries here would be asserting a second write this double cannot see -- which is how
  // this line read when it was written, and CI is where that was found out.
  assert.deepEqual(patches, [{ whoCanMessage: EVERYONE }]);
  assert.equal(metrics.operation('setup').errors, 0);
  assert.equal(a.conversationId, 'conv-0');
  assert.equal(c.conversationId, 'conv-2');

  // The receivers listed: one list per receiver, never for a sender, which is the fourth door (see
  // the file docstring). Sorted, and ordered per pair rather than by first-and-last, because the
  // pairs run concurrently through `runPool` — the two pairs' events interleave, so the only order
  // the run actually guarantees is that a receiver lists after *its own* sender created the
  // conversation it is looking for. A list read before that create would find nothing and would
  // have primed nothing, so a bare count would pass on a run that answered no invite at all.
  assert.deepEqual(timeline.filter((event) => event.startsWith('list:')).sort(), [
    'list:1',
    'list:3',
  ]);
  for (const [sender, receiver] of [
    ['start:0', 'list:1'],
    ['start:2', 'list:3'],
  ] as const) {
    const created = timeline.indexOf(sender);
    const listed = timeline.indexOf(receiver);
    assert.ok(
      created !== -1 && listed > created,
      `${receiver} did not follow ${sender}: ${timeline.join(', ')}`,
    );
  }
});

test('a receiver whose list does not carry the conversation it was invited to is a setup error', async () => {
  // The other half of the fourth door. A client that is invited and lists and does not find the
  // conversation has been told two contradictory things by the server, and the harness must fail
  // here — as a `setup` error, counted like every other pair refusal — rather than carry on into a
  // receiver that holds a conversation it can neither seal for nor read. Left to the SDK this is
  // the `op 53` membership error one event later, which names the client rather than the cause.
  const timeline: string[] = [];
  const a = makeVu(0, timeline);
  const b = makeVu(1, timeline, { list: [] });
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());

  await openDirectConversations([{ sender: a, receiver: b }], ctx);

  assert.equal(metrics.operation('setup').errors, 1, 'the missing conversation is counted once');
  assert.ok(timeline.includes('list:1'), 'the receiver did list; the answer was what was wrong');
  // The sender's own half still stands: the conversation exists and the caller holds it, which is
  // why a scenario's remaining pairs are unaffected by one receiver's bad answer.
  assert.equal(a.conversationId, 'conv-0');
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
  assert.deepEqual(timeline, ['profile:0', 'profile:1', 'profile:2']);
});

test('a refused opening is counted as a setup error and its pair is still attempted', async () => {
  const timeline: string[] = [];
  const a = makeVu(0, timeline, { profileFails: true });
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

test('the call policy is opened as well, on both roles, before any call is placed', async () => {
  // The third door, and the one that was missing: `who_can_message` does not gate a call. A fresh
  // account's `who_can_call_voice` is `Friends`, the callee's own policy is what refuses an invite,
  // and the refusal comes back as `BLOCKED` in an ordinary reply rather than as an error — so a
  // calls step whose pairs are strangers reported 4,000 resolved invites and rang nobody, with
  // `call-answer` absent from its report altogether. Both columns are asserted, not just the audio
  // one the scenario currently drives, because the video column gates the same way.
  const timeline: string[] = [];
  const patches: Record<string, unknown>[] = [];
  const a = makeVu(0, timeline, { onProfile: (patch) => patches.push(patch) });
  const b = makeVu(1, timeline);
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());

  await openCallsToStrangers([{ sender: a, receiver: b }], ctx);

  assert.deepEqual(patches, [{ whoCanCallVoice: EVERYONE, whoCanCallVideo: EVERYONE }]);
  // Both roles, for the reason the message opening writes both: which VU is the caller is a
  // scenario's business, and the gate is read on the callee.
  assert.deepEqual(timeline, ['profile:0', 'profile:1']);
  assert.equal(metrics.operation('setup').errors, 0);
});

test('a VU whose call policy cannot be written is counted, not thrown', async () => {
  const timeline: string[] = [];
  const a = makeVu(0, timeline, { profileFails: true });
  const b = makeVu(1, timeline);
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());

  await assert.doesNotReject(() => openCallsToStrangers([{ sender: a, receiver: b }], ctx));

  assert.equal(metrics.operation('setup').errors, 1);
  assert.deepEqual(metrics.operation('setup').errorsByClass, [['transport', 1]]);
});
