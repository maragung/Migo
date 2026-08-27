/**
 * The workload catalogue and the registry that names it. Two kinds of thing are proved here.
 *
 * The registry contract: each scenario is registered exactly once under its own name, an unknown
 * name resolves to nothing (so the runner can refuse it), and the minimum-VU floors are what the
 * runner enforces. The workload behaviour, driven through a structural double of the SDK client so
 * no network is touched: the steps run in the documented order and shape (presence flips
 * Online/Away; a sender streams sealed text with an incrementing sequence), a per-op failure is
 * counted rather than swallowed, and messaging setup pairs adjacent connected VUs — warning, not
 * crashing, on an odd count.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ContentType, ConversationKind, PresenceState, TransportError } from '@migo/sdk';

import { Logger } from '../logger.js';
import { RunContext } from '../run-context.js';
import { getScenario, scenarioNames } from '../scenarios.js';
import type { Scenario } from '../scenarios.js';
import { Metrics } from '../stats.js';
import type { VirtualUser } from '../virtual-user.js';

const QUIET = new Logger('quiet');
const future = (): number => performance.now() + 60_000;

interface VuHooks {
  connected?: boolean;
  onPresence?: (state: PresenceState) => void;
  presenceFails?: boolean;
  onSend?: (conversationId: string, content: unknown) => void;
  onStart?: (kind: ConversationKind, members: readonly unknown[]) => void;
  startFails?: boolean;
  onWatch?: (id: string) => void;
}

/** A VirtualUser whose client only records what a scenario asks of it. */
function makeVu(index: number, hooks: VuHooks = {}): VirtualUser {
  const client = {
    accountId: `acct-${index}`,
    presence: {
      setPresence: (state: PresenceState): Promise<void> => {
        hooks.onPresence?.(state);
        return hooks.presenceFails
          ? Promise.reject(new TransportError('presence failed'))
          : Promise.resolve();
      },
    },
    messaging: {
      send: (conversationId: string, content: unknown): Promise<void> => {
        hooks.onSend?.(conversationId, content);
        return Promise.resolve();
      },
    },
    startConversation: (kind: ConversationKind, members: readonly unknown[]) => {
      hooks.onStart?.(kind, members);
      return hooks.startFails
        ? Promise.reject(new TransportError('start failed'))
        : Promise.resolve({ conversationId: `conv-${index}` });
    },
    watchConversation: (id: string): Promise<void> => {
      hooks.onWatch?.(id);
      return Promise.resolve();
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

function scenario(name: string): Scenario {
  const found = getScenario(name);
  if (found === undefined) throw new Error(`no scenario named ${name}`);
  return found;
}

/** Capture stderr for the duration of an async body (the odd-count warning goes there). */
async function withStderr(body: () => Promise<void>): Promise<string> {
  // Bound, not referenced bare: restoring an unbound `write` would leave a method without its
  // stream as `this`.
  const original = process.stderr.write.bind(process.stderr);
  let captured = '';
  process.stderr.write = (chunk: unknown) => {
    captured += String(chunk);
    return true;
  };
  try {
    await body();
  } finally {
    process.stderr.write = original;
  }
  return captured;
}

test('every scenario is registered once, under its own name', () => {
  const names = scenarioNames();
  assert.deepEqual(names, ['connect', 'presence', 'messaging']);
  assert.equal(new Set(names).size, names.length, 'no duplicate registrations');
  for (const name of names) assert.equal(scenario(name).name, name);
});

test('an unknown scenario name resolves to undefined so the runner can refuse it', () => {
  assert.equal(getScenario('does-not-exist'), undefined);
  assert.equal(getScenario(''), undefined);
});

test('the minimum-VU floors are the documented ones', () => {
  assert.equal(scenario('connect').minVus, 1);
  assert.equal(scenario('presence').minVus, 1);
  assert.equal(scenario('messaging').minVus, 2); // needs a pair
});

test('connect is a hold-only scenario: no prepare work, no workloads', async () => {
  const connect = scenario('connect');
  await assert.doesNotReject(() =>
    connect.prepare([], new RunContext(new Metrics(), QUIET, 0, future())),
  );
  assert.deepEqual(connect.workloads([makeVu(0), makeVu(1)]), []);
});

test('presence builds one workload per connected VU, skipping the disconnected', () => {
  const vus = [makeVu(0), makeVu(1, { connected: false }), makeVu(2)];
  assert.equal(scenario('presence').workloads(vus).length, 2);
});

test('a presence workload flips Online/Away in order and counts each success', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const states: PresenceState[] = [];
  const vu = makeVu(0, {
    onPresence: (state) => {
      states.push(state);
      if (states.length === 3) ctx.interrupt();
    },
  });
  const [workload] = scenario('presence').workloads([vu]);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);
  assert.deepEqual(states, [PresenceState.Online, PresenceState.Away, PresenceState.Online]);
  assert.equal(metrics.operation('presence').ok, 3);
});

test('a failing presence op is counted by class, not swallowed', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  let calls = 0;
  const vu = makeVu(0, {
    presenceFails: true,
    onPresence: () => {
      calls += 1;
      if (calls === 3) ctx.interrupt();
    },
  });
  const [workload] = scenario('presence').workloads([vu]);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);
  const op = metrics.operation('presence');
  assert.equal(op.ok, 0);
  assert.equal(op.errors, 3);
  assert.deepEqual(op.errorsByClass, [['transport', 3]]);
});

test('messaging setup pairs adjacent connected VUs and subscribes each receiver', async () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, future());
  const starts: Array<{ kind: ConversationKind; members: readonly unknown[] }> = [];
  const watched: string[] = [];
  const vus = [
    makeVu(0, { onStart: (kind, members) => starts.push({ kind, members }) }),
    makeVu(1, { onWatch: (id) => watched.push(id) }),
    makeVu(2, { onStart: (kind, members) => starts.push({ kind, members }) }),
    makeVu(3, { onWatch: (id) => watched.push(id) }),
  ];
  await scenario('messaging').prepare(vus, ctx);

  assert.equal(vus[0]?.conversationId, 'conv-0');
  assert.equal(vus[0]?.partner, vus[1]);
  assert.equal(vus[2]?.conversationId, 'conv-2');
  assert.equal(vus[2]?.partner, vus[3]);
  assert.equal(vus[1]?.conversationId, undefined, 'the receiver does not become a sender');
  // Each conversation is Direct, addressed to the receiver's account.
  assert.deepEqual(starts, [
    { kind: ConversationKind.Direct, members: ['acct-1'] },
    { kind: ConversationKind.Direct, members: ['acct-3'] },
  ]);
  assert.deepEqual(watched.sort(), ['conv-0', 'conv-2']);
});

test('messaging setup warns on an odd number of connected VUs and pairs the rest', async () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, future());
  const vus = [makeVu(0), makeVu(1), makeVu(2)];
  const stderr = await withStderr(() => scenario('messaging').prepare(vus, ctx));
  assert.ok(stderr.includes('odd number of VUs'));
  assert.equal(vus[0]?.conversationId, 'conv-0'); // the pair still formed
  assert.equal(vus[2]?.conversationId, undefined); // the odd one out stays idle
});

test('a failure during messaging setup is counted as a setup error, not thrown', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const vus = [makeVu(0, { startFails: true }), makeVu(1)];
  await assert.doesNotReject(() => scenario('messaging').prepare(vus, ctx));
  assert.equal(metrics.operation('setup').errors, 1);
  assert.equal(vus[0]?.conversationId, undefined);
});

test('a messaging workload streams sealed text with an incrementing sequence, in order', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const sends: Array<[string, unknown]> = [];
  const sender = makeVu(0, {
    onSend: (conversationId, content) => {
      sends.push([conversationId, content]);
      if (sends.length === 2) ctx.interrupt();
    },
  });
  const receiver = makeVu(1);
  await scenario('messaging').prepare([sender, receiver], ctx);
  const workloads = scenario('messaging').workloads([sender, receiver]);
  assert.equal(workloads.length, 1, 'only the sender holds a conversation');
  const [workload] = workloads;
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);
  assert.deepEqual(sends, [
    ['conv-0', { type: ContentType.Text, text: 'loadgen 0 #1' }],
    ['conv-0', { type: ContentType.Text, text: 'loadgen 0 #2' }],
  ]);
  assert.equal(metrics.operation('send').ok, 2);
});
