/**
 * The workload catalogue and the registry that names it. Two kinds of thing are proved here.
 *
 * The registry contract: each scenario is registered exactly once under its own name, an unknown
 * name resolves to nothing (so the runner can refuse it), and the minimum-VU floors are what the
 * runner enforces. The workload behaviour, driven through a structural double of the SDK client so
 * no network is touched: the steps run in the documented order and shape (presence flips
 * Online/Away; a sender streams sealed text with an incrementing sequence; a calls pair walks
 * invite → auto-answer → SDP relay → ICE → end; the outage settle rules on exactly-once
 * delivery), a per-op failure is counted rather than swallowed, and setup pairs adjacent connected
 * VUs — warning, not crashing, on an odd count.
 *
 * The full-scale scenarios' listeners are driven the same way their real counterparts are: the
 * double captures the handler the scenario registers, and the test invokes it with the event the
 * server would have pushed — so what is under test is the scenario's own wiring, not the SDK's.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CallEndReason,
  ContentType,
  ConversationKind,
  MediaKind,
  PresenceState,
  TransportError,
} from '@migo/sdk';

import { Logger } from '../logger.js';
import { RunContext } from '../run-context.js';
import { getScenario, scenarioNames } from '../scenarios.js';
import type { Scenario } from '../scenarios.js';
import { Metrics } from '../stats.js';
import type { VirtualUser } from '../virtual-user.js';

const QUIET = new Logger('quiet');
const future = (): number => performance.now() + 60_000;

/** The slice of a call event the scenario's listeners read; the double's events carry these. */
interface CallEventDouble {
  readonly callId: string;
  readonly callerDevice?: string;
  readonly fromDevice?: string;
}

interface VuHooks {
  connected?: boolean;
  onPresence?: (state: PresenceState) => void;
  presenceFails?: boolean;
  onSend?: (conversationId: string, content: unknown, options?: unknown) => void;
  onStart?: (kind: ConversationKind, members: readonly unknown[]) => void;
  startFails?: boolean;
  onWatch?: (id: string) => void;
  // The listeners the full-scale scenarios register, captured for the test to fire.
  incomingCall?: (event: CallEventDouble) => void;
  sdpRelay?: (event: CallEventDouble) => void;
  iceRelay?: (event: CallEventDouble) => void;
  message?: (message: { messageId: string }) => void;
  // Calls-surface observations.
  onInvite?: (callId: string) => void;
  inviteFails?: boolean;
  onAnswer?: (callId: string) => void;
  onRelaySdp?: (callId: string, toDevice: string) => void;
  onRelayIce?: (callId: string, toDevice: string) => void;
  onEnd?: (callId: string, reason: CallEndReason) => void;
  // Media-surface observations.
  onUpload?: (options: unknown, bytes: Uint8Array) => void;
  // Outage-surface observations and state.
  onQueued?: (conversationId: string, content: unknown, options?: unknown) => void;
  outboxSize?: number;
  connectionState?: 'ready' | 'reconnecting';
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
      send: (conversationId: string, content: unknown, options?: unknown): Promise<void> => {
        hooks.onSend?.(conversationId, content, options);
        return Promise.resolve();
      },
      onMessage: (handler: (message: { messageId: string }) => void): (() => void) => {
        hooks.message = handler;
        return () => {};
      },
    },
    calls: {
      onIncomingCall: (handler: (event: CallEventDouble) => void): (() => void) => {
        hooks.incomingCall = handler;
        return () => {};
      },
      onSdp: (handler: (event: CallEventDouble) => void): (() => void) => {
        hooks.sdpRelay = handler;
        return () => {};
      },
      onIce: (handler: (event: CallEventDouble) => void): (() => void) => {
        hooks.iceRelay = handler;
        return () => {};
      },
      invite: (
        _conversationId: string,
        _calleeId: string,
        _mediaKind: number,
        _offer: Uint8Array,
        callId: string,
      ): Promise<void> => {
        hooks.onInvite?.(callId);
        return hooks.inviteFails
          ? Promise.reject(new TransportError('invite refused'))
          : Promise.resolve();
      },
      answer: (callId: string, _answer: Uint8Array): Promise<void> => {
        hooks.onAnswer?.(callId);
        return Promise.resolve();
      },
      sendSdp: (callId: string, toDevice: string, _sdp: Uint8Array): Promise<void> => {
        hooks.onRelaySdp?.(callId, toDevice);
        return Promise.resolve();
      },
      sendIce: (callId: string, toDevice: string, _candidates: Uint8Array): Promise<void> => {
        hooks.onRelayIce?.(callId, toDevice);
        return Promise.resolve();
      },
      end: (callId: string, reason: CallEndReason): Promise<void> => {
        hooks.onEnd?.(callId, reason);
        return Promise.resolve();
      },
      cancel: (_callId: string): Promise<void> => Promise.resolve(),
    },
    media: {
      upload: (options: unknown, bytes: Uint8Array): Promise<{ mediaId: string }> => {
        hooks.onUpload?.(options, bytes);
        return Promise.resolve({ mediaId: `media-${index}` });
      },
    },
    sendQueued: (conversationId: string, content: unknown, options?: unknown): Promise<void> => {
      hooks.onQueued?.(conversationId, content, options);
      return Promise.resolve();
    },
    outbox: {
      get size(): number {
        return hooks.outboxSize ?? 0;
      },
    },
    get connectionState(): string {
      return hooks.connectionState ?? 'ready';
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
    // The hooks bag itself, so a test can fire the listeners the scenario registered on this VU.
    hooks,
  } as unknown as VirtualUser;
}

/** The hooks bag a double was built with — the door into its captured listeners. */
function hooksOf(vu: VirtualUser): VuHooks {
  return (vu as unknown as { hooks: VuHooks }).hooks;
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
  assert.deepEqual(names, [
    'connect',
    'presence',
    'messaging',
    'fanout',
    'calls',
    'voice-notes',
    'outage',
  ]);
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
  assert.equal(scenario('fanout').minVus, 2); // a sender and at least one receiver
  assert.equal(scenario('calls').minVus, 2); // needs a pair
  assert.equal(scenario('voice-notes').minVus, 2); // needs a conversation
  assert.equal(scenario('outage').minVus, 2); // needs a pair
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

// ---------------------------------------------------------------------------
// fanout: one group at the member ceiling, one sender, everyone receiving
// ---------------------------------------------------------------------------

test('fanout setup creates one group conversation of every other account and warms it up', async () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, future());
  const starts: Array<{ kind: ConversationKind; members: readonly unknown[] }> = [];
  const sends: unknown[] = [];
  const vus = [
    makeVu(0, {
      onStart: (kind, members) => starts.push({ kind, members }),
      onSend: (_conversationId, content) => sends.push(content),
    }),
    makeVu(1),
    makeVu(2),
  ];
  await scenario('fanout').prepare(vus, ctx);

  assert.equal(starts.length, 1, 'exactly one group conversation');
  assert.equal(starts[0]?.kind, ConversationKind.Group);
  assert.deepEqual(starts[0]?.members, ['acct-1', 'acct-2']);
  assert.equal(vus[0]?.conversationId, 'conv-0');
  // The warm-up send paid the sender-key distribution and is the only send during setup.
  assert.equal(sends.length, 1);
  assert.deepEqual(sends[0], { type: ContentType.Text, text: 'loadgen fanout warm-up' });
});

test('fanout setup warns when more VUs connect than a group may hold', async () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, future());
  const starts: Array<{ kind: ConversationKind; members: readonly unknown[] }> = [];
  // 257 connected VUs: one over the product ceiling of 256, so one must stay idle.
  const vus = Array.from({ length: 257 }, (_unused, i) =>
    makeVu(i, i === 0 ? { onStart: (kind, members) => starts.push({ kind, members }) } : {}),
  );
  const stderr = await withStderr(() => scenario('fanout').prepare(vus, ctx));
  assert.ok(stderr.includes('stay idle'));
  assert.equal(starts.length, 1);
  // The sender plus 255 named members is a full group; the 257th VU stays idle.
  assert.equal(starts[0]?.members.length, 255);
});

test('fanout rules one ok verdict per acknowledged message that reached every receiver', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const sentIds: string[] = [];
  const senderHooks: VuHooks = {
    // The warm-up send carries no messageId, so only steady-state sends land here.
    onSend: (_conversationId, _content, options) => {
      const id = (options as { messageId?: string } | undefined)?.messageId;
      if (id !== undefined && sentIds.push(id) === 2) ctx.interrupt();
    },
  };
  const sender = makeVu(0, senderHooks);
  const receiverA = makeVu(1);
  const receiverB = makeVu(2);
  const vus = [sender, receiverA, receiverB];
  const fanout = scenario('fanout');
  await fanout.prepare(vus, ctx);

  const [workload] = fanout.workloads(vus);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);
  assert.equal(sentIds.length, 2, 'two steady-state sends before the interrupt');

  // Every acknowledged message delivered to both receivers, as the server would push.
  for (const id of sentIds) {
    hooksOf(receiverA).message?.({ messageId: id });
    hooksOf(receiverB).message?.({ messageId: id });
  }

  await fanout.settle?.(vus, new RunContext(metrics, QUIET, 0, future()));
  const verdict = metrics.operation('fanout-verdict');
  assert.equal(verdict.ok, 2);
  assert.equal(verdict.errors, 0);
  assert.equal(metrics.operation('fanout-deliver').ok, 4, '2 messages x 2 receivers');
});

test('fanout verdict names a short fan-out and an over fan-out per message', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const sentIds: string[] = [];
  const senderHooks: VuHooks = {
    onSend: (_conversationId, _content, options) => {
      const id = (options as { messageId?: string } | undefined)?.messageId;
      if (id !== undefined && sentIds.push(id) === 2) ctx.interrupt();
    },
  };
  const sender = makeVu(0, senderHooks);
  const receiverA = makeVu(1);
  const receiverB = makeVu(2);
  const vus = [sender, receiverA, receiverB];
  const fanout = scenario('fanout');
  await fanout.prepare(vus, ctx);
  const [workload] = fanout.workloads(vus);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);

  // First message over-delivered (3 copies), second short (1 of 2): the landed total still
  // equals expected, so the settle wait exits immediately and the per-message verdicts carry
  // the diagnosis.
  const [first, second] = sentIds;
  const deliver = (vu: VirtualUser, id: string | undefined, times: number): void => {
    for (let i = 0; i < times; i += 1) hooksOf(vu).message?.({ messageId: id ?? '' });
  };
  deliver(receiverA, first, 2);
  deliver(receiverB, first, 1);
  deliver(receiverA, second, 1);

  await fanout.settle?.(vus, new RunContext(metrics, QUIET, 0, future()));
  const verdict = metrics.operation('fanout-verdict');
  assert.equal(verdict.ok, 0);
  assert.equal(verdict.errors, 2);
  assert.deepEqual(verdict.errorsByClass, [
    ['over-fanout', 1],
    ['short-fanout', 1],
  ]);
});

// ---------------------------------------------------------------------------
// calls: the signaling lifecycle, driven end to end through the listeners
// ---------------------------------------------------------------------------

/**
 * Runs one calls cycle against doubles that play the far end: the callee auto-answers from its
 * captured invite listener, the answer relay resolves the caller's captured SDP listener, and the
 * ICE relay lands on the callee's captured ICE listener — exactly the events the server pushes.
 */
test('a calls pair walks invite, auto-answer, SDP relay, ICE relay and end', async () => {
  const metrics = new Metrics();
  // A deadline just past the invite: the hold loop notices it on its first wake and every invite
  // still gets its end, which is the property under test.
  const ctx = new RunContext(metrics, QUIET, 0, performance.now() + 100);
  const ended: Array<[string, CallEndReason]> = [];

  const senderHooks: VuHooks = {};
  const receiverHooks: VuHooks = {};
  const sender = makeVu(0, senderHooks);
  const receiver = makeVu(1, receiverHooks);
  const vus = [sender, receiver];
  const calls = scenario('calls');
  await calls.prepare(vus, ctx);
  assert.equal(vus[0]?.conversationId, 'conv-0', 'the pair holds one conversation');

  // The far end's behaviour, wired through the hooks the doubles expose.
  senderHooks.onInvite = (callId) => {
    // The callee rings and auto-answers, one microtask later — the listener the scenario
    // registered on the receiver.
    queueMicrotask(() => receiverHooks.incomingCall?.({ callId, callerDevice: 'dev-caller' }));
  };
  receiverHooks.onRelaySdp = (callId) => {
    // The answer relay reaches the caller's SDP listener, naming the answering device.
    senderHooks.sdpRelay?.({ callId, fromDevice: 'dev-callee' });
  };
  senderHooks.onRelayIce = (callId) => {
    receiverHooks.iceRelay?.({ callId });
  };
  senderHooks.onEnd = (callId, reason) => {
    ended.push([callId, reason]);
  };

  const [workload] = calls.workloads(vus);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);

  for (const label of [
    'call-invite',
    'call-answer',
    'call-sdp',
    'call-setup',
    'call-ice',
    'call-ice-deliver',
    'call-end',
  ]) {
    assert.equal(metrics.operation(label).ok, 1, `${label} happened exactly once`);
    assert.equal(metrics.operation(label).errors, 0, `${label} never failed`);
  }
  assert.equal(ended.length, 1);
  assert.equal(ended[0]?.[1], CallEndReason.ByCaller);
  assert.ok(metrics.operation('call-setup').latency.count > 0, 'setup latency was measured');
});

test('a refused invite disarms the SDP wait instead of hanging the pair on a phantom timeout', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const senderHooks: VuHooks = {
    inviteFails: true,
    onInvite: () => {
      // One refused cycle is enough to prove the point; stop before a second.
      ctx.interrupt();
    },
  };
  const sender = makeVu(0, senderHooks);
  const receiver = makeVu(1);
  const vus = [sender, receiver];
  const calls = scenario('calls');
  await calls.prepare(vus, ctx);
  const [workload] = calls.workloads(vus);
  if (workload === undefined) throw new Error('expected a workload');
  // Must resolve promptly: a wait left armed would only clear on the 15 s setup timeout.
  await workload(ctx);
  assert.equal(metrics.operation('call-invite').errors, 1);
  assert.equal(
    metrics.operation('call-setup').errors,
    0,
    'a refused invite is not a setup timeout',
  );
});

// ---------------------------------------------------------------------------
// voice-notes: the full upload lifecycle with genuinely valid bytes
// ---------------------------------------------------------------------------

test('a voice-notes workload uploads a valid WAV through the full lifecycle options', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const uploads: Array<[unknown, Uint8Array]> = [];
  const senderHooks: VuHooks = {
    onUpload: (options, bytes) => {
      uploads.push([options, bytes]);
      if (uploads.length === 1) ctx.interrupt();
    },
  };
  const sender = makeVu(0, senderHooks);
  const receiver = makeVu(1);
  const vus = [sender, receiver];
  const voiceNotes = scenario('voice-notes');
  await voiceNotes.prepare(vus, ctx);
  // Both halves of the pair are members, so both upload into the one conversation.
  assert.equal(vus[0]?.conversationId, 'conv-0');
  assert.equal(vus[1]?.conversationId, 'conv-0');
  const workloads = voiceNotes.workloads(vus);
  assert.equal(workloads.length, 2, 'both halves upload');

  const [workload] = workloads;
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);
  assert.equal(uploads.length, 1);
  assert.equal(metrics.operation('voice-upload').ok, 1);

  const [options, bytes] = uploads[0] ?? [undefined, undefined];
  if (bytes === undefined) throw new Error('expected uploaded bytes');
  assert.deepEqual(options, {
    kind: MediaKind.VoiceNote,
    contentType: 'audio/wav',
    size: bytes.length,
    conversationId: 'conv-0',
    durationMs: 1_000,
  });
  // A genuinely valid WAV: the sniffer reads magic bytes at commit, and filler would be refused.
  assert.equal(bytes.length, 44 + 32_000, 'one second of 16 kHz mono 16-bit PCM plus the header');
  const text = (offset: number, length: number): string =>
    String.fromCharCode(...bytes.slice(offset, offset + length));
  assert.equal(text(0, 4), 'RIFF');
  assert.equal(text(8, 4), 'WAVE');
});

// ---------------------------------------------------------------------------
// outage: the offline outbox, the flush, and the exactly-once verdict
// ---------------------------------------------------------------------------

test('an outage pair delivers exactly once and settles clean, flush included', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const sentIds: string[] = [];
  const queuedTexts: string[] = [];
  const senderHooks: VuHooks = {
    onQueued: (_conversationId, content, options) => {
      const text = (content as { text?: string } | undefined)?.text ?? '';
      queuedTexts.push(text);
      const id = (options as { messageId?: string } | undefined)?.messageId;
      if (id !== undefined && sentIds.push(id) === 2) ctx.interrupt();
    },
  };
  const sender = makeVu(0, senderHooks);
  const receiver = makeVu(1);
  const vus = [sender, receiver];
  const outage = scenario('outage');
  await outage.prepare(vus, ctx);

  const [workload] = outage.workloads(vus);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);
  assert.equal(sentIds.length, 2, 'two outbox sends before the interrupt');

  const receiverHooks = hooksOf(receiver);
  for (const id of sentIds) receiverHooks.message?.({ messageId: id });

  // The runner never settles an interrupted run, so settle gets a fresh context —
  // the same one production would hand it.
  await outage.settle?.(vus, new RunContext(metrics, QUIET, 0, future()));
  const verdict = metrics.operation('outage-verdict');
  assert.equal(verdict.ok, 1);
  assert.equal(verdict.errors, 0);
  // The trailing flush ran — one extra send whose whole purpose is to arm the gap-fill sync.
  assert.equal(queuedTexts.length, 3);
  assert.ok(queuedTexts[2]?.includes('settle'), 'the flush send is the last one');
  assert.equal(metrics.operation('deliver').ok, 2);
});

test('the outage verdict names a duplicate delivery and a session that never resumed', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const sentIds: string[] = [];
  const senderHooks: VuHooks = {
    onQueued: (_conversationId, _content, options) => {
      const id = (options as { messageId?: string } | undefined)?.messageId;
      if (id !== undefined && sentIds.push(id) === 1) ctx.interrupt();
    },
  };
  const sender = makeVu(0, senderHooks);
  // The receiver never made it back after the restart.
  const receiver = makeVu(1, { connectionState: 'reconnecting' });
  const vus = [sender, receiver];
  const outage = scenario('outage');
  await outage.prepare(vus, ctx);
  const [workload] = outage.workloads(vus);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);

  const receiverHooks = hooksOf(receiver);
  const [id] = sentIds;
  // Delivered twice — the exactly-once contract broken by the redelivery side.
  receiverHooks.message?.({ messageId: id ?? '' });
  receiverHooks.message?.({ messageId: id ?? '' });

  await outage.settle?.(vus, new RunContext(metrics, QUIET, 0, future()));
  const verdict = metrics.operation('outage-verdict');
  assert.equal(verdict.ok, 0);
  assert.equal(verdict.errors, 1);
  const sample = verdict.errorSamples[0]?.[1] ?? '';
  assert.ok(sample.includes('duplicated'), `the sample names the duplicate: ${sample}`);
  assert.ok(sample.includes('not ready'), `the sample names the dead session: ${sample}`);
});

test('an acknowledged message that never arrives is ruled missing, not excused', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const sentIds: string[] = [];
  const senderHooks: VuHooks = {
    onQueued: (_conversationId, _content, options) => {
      const id = (options as { messageId?: string } | undefined)?.messageId;
      if (id !== undefined && sentIds.push(id) === 2) ctx.interrupt();
    },
  };
  const sender = makeVu(0, senderHooks);
  const receiver = makeVu(1);
  const vus = [sender, receiver];
  const outage = scenario('outage');
  await outage.prepare(vus, ctx);
  const [workload] = outage.workloads(vus);
  if (workload === undefined) throw new Error('expected a workload');
  await workload(ctx);

  // Only the first message is delivered; the second is lost by the resume.
  const receiverHooks = hooksOf(receiver);
  receiverHooks.message?.({ messageId: sentIds[0] ?? '' });

  // The delivery wait is bounded but long; an interrupt stops the waiting so the verdict
  // lands promptly — the same escape a Ctrl-C gets. The settle context is fresh, the way
  // the runner hands it over, and this interrupt is aimed at that context alone.
  const settleCtx = new RunContext(metrics, QUIET, 0, future());
  const stop = setTimeout(() => settleCtx.interrupt(), 50);
  await outage.settle?.(vus, settleCtx);
  clearTimeout(stop);

  const verdict = metrics.operation('outage-verdict');
  assert.equal(verdict.ok, 0);
  assert.equal(verdict.errors, 1);
  const sample = verdict.errorSamples[0]?.[1] ?? '';
  assert.ok(sample.includes('missing'), `the sample names the loss: ${sample}`);
});
