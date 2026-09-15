/**
 * The brief-section-172 full-scale load scenarios, in the shapes a nightly runner can honestly
 * drive against one real node.
 *
 * The brief's list — ten thousand idle sessions, a thousand messages a second, one room with ten
 * thousand members, a thousand simultaneous calls, a thousand simultaneous voice-note uploads, and
 * mass sync after an outage — names six load shapes. Two of them the existing scenarios already
 * are, scaled by the runner's flags: `connect` is the idle-session shape (`--vus 10000`) and
 * `messaging` is the message-rate shape (senders × `--rate` = the per-second total). What this
 * module adds are the four shapes nothing drove yet, plus the integrity verdicts that make the
 * outage scenario a gate rather than a benchmark:
 *
 * - `fanout` — one conversation holding as many members as the product admits, with every member
 *   measuring send-to-deliver latency. The honest ceiling is 256, not the brief's ten thousand:
 *   a room is capped at 50 members (public rooms fixed at 33, managed rooms at a
 *   friendship-earned ceiling of 50 — migo-rooms' capacity model) and a group conversation at 256
 *   (`MAX_GROUP_MEMBERS` in the store's model). The ten-thousand-member room needs a product
 *   change to the room capacity tier, not a loadgen change, and stays SPEC.
 * - `calls` — pairs place, answer, relay through and end real calls, with sealed placeholder
 *   offers. The media plane of a Migo call is peer-to-peer WebRTC that never crosses a server, so
 *   the signaling exercised here *is* the entire server-visible call surface; brief section 165
 *   ships exactly this placeholder-sealed shape until the call crypto layer lands, and a load
 *   test cannot honestly claim more than the surface that exists.
 * - `voice-notes` — every VU uploads real voice notes through the full ticket/PUT/commit
 *   lifecycle. The bytes are a synthesized WAV — a genuine `RIFF…WAVE` file, because the media
 *   service sniffs magic bytes at commit and refuses unrecognised content, so a run of empty
 *   buffers would be a run of refusals.
 * - `outage` — paired senders stream through the offline outbox while the *runner* kills and
 *   restarts the node mid-run (loadgen is a client and cannot restart the server it talks to;
 *   the orchestration belongs to `tools/load/run-full.sh`). The scenario's settle phase then
 *   demands the section-158 contract: every acknowledged message delivered, none delivered
 *   twice, every session back to ready.
 *
 * Every scenario here reports the metric families the brief names — p50/p95/p99 wherever a
 * latency applies, and the wire-byte summary the report turns into bytes per user per minute.
 * The server-side families (memory per session, dropped frames) are the runner's to read from
 * `/metrics`, because a client cannot measure the node it is loading.
 *
 * Per-run state is held in module-level variables assigned by `prepare` and read by `workloads`
 * and `settle`. One loadgen process runs exactly one scenario once, so the singleton is safe; the
 * alternative (threading a state object through the {@link Scenario} interface) would widen the
 * interface every simple scenario implements for the sake of the four that need it.
 */

import {
  CallEndReason,
  CallMediaKind,
  ContentType,
  ConversationKind,
  MediaKind,
  newId,
} from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { openDirectConversations, pairUp } from './pairs.js';
import { runPool } from './pool.js';
import type { RunContext } from './run-context.js';
import { sleep } from './run-context.js';
import type { Scenario } from './scenarios.js';
import type { VirtualUser } from './virtual-user.js';

/** How often a bounded wait wakes to re-check its condition. */
const POLL_MS = 250;

/** How many members may be subscribed concurrently during fanout setup. */
const FANOUT_SUBSCRIBE_CONCURRENCY = 32;

// ---------------------------------------------------------------------------
// fanout: one conversation at the product's member ceiling
// ---------------------------------------------------------------------------

/**
 * The most members one conversation may hold, mirroring `MAX_GROUP_MEMBERS` in the server's store
 * model. Duplicated here because the number is a product ceiling the SDK does not export, and a
 * run that asked for more would be a run of refusals: the extra VUs stay idle and the scenario
 * warns, so the nightly shape records what the product actually admits.
 */
const GROUP_MEMBER_CEILING = 256;

/** How long the settle phase waits for in-flight fan-out deliveries before ruling. */
const FANOUT_SETTLE_MS = 15_000;

/** The per-run bookkeeping shared by the fanout scenario's three phases. */
interface FanoutRun {
  /** The VU that created the group and sends; undefined when setup failed. */
  readonly sender: VirtualUser;
  readonly conversationId: Id;
  /** When each steady-state message was handed to send, by the id the scenario minted. */
  readonly sendStarts: Map<Id, number>;
  /** How many copies of each acknowledged message have arrived, across all receivers. */
  readonly deliveries: Map<Id, number>;
  /** Message ids the server acknowledged. Everything here must reach every receiver. */
  readonly acked: Set<Id>;
  /** Members whose onMessage listener is armed and whose watch succeeded. */
  receivers: number;
}

let fanoutRun: FanoutRun | undefined;

/**
 * One sender, many receivers: the send-to-deliver latency of a message fanned out to every member
 * of one group conversation.
 *
 * `prepare` builds the largest single fanout group the product admits (a group conversation of up
 * to {@link GROUP_MEMBER_CEILING} members), subscribes every member, and pays the one-time
 * sender-key distribution with an unmeasured warm-up send — that distribution is real cost a
 * first message in a fresh group pays, but it is one-time, and the number this scenario exists to
 * measure is the fan-out, so it lands in the setup phase where every other one-time cost already
 * lands. Steady state is a single sender streaming sealed messages at the target rate while every
 * receiver times when its copy arrived; the settle phase then demands each acknowledged message
 * reached every receiver — fan-out that silently loses a member is loss the error budget cannot
 * see, because the sender's send succeeded.
 */
const fanout: Scenario = {
  name: 'fanout',
  description:
    'One group conversation at the product member ceiling (256); one sender, every member ' +
    'measures send-to-deliver latency.',
  minVus: 2,
  async prepare(vus, ctx) {
    const connected = vus.filter((vu) => vu.connected);
    const sender = connected[0];
    if (sender === undefined) return;
    // The ceiling counts the creator, so 255 named members plus the sender is a full group.
    const members = connected.slice(1, GROUP_MEMBER_CEILING);
    if (connected.length > GROUP_MEMBER_CEILING) {
      ctx.log.warn(
        `${connected.length} VUs connected but a group conversation holds at most ` +
          `${GROUP_MEMBER_CEILING} members; ${connected.length - GROUP_MEMBER_CEILING} stay idle`,
      );
    }
    if (members.length === 0) {
      ctx.log.warn('no receiver connected; nothing to fan out to');
      return;
    }

    try {
      const summary = await sender.client.startConversation(
        ConversationKind.Group,
        members.map((vu) => vu.client.accountId),
        { title: 'loadgen fanout' },
      );
      sender.conversationId = summary.conversationId;
      const run: FanoutRun = {
        sender,
        conversationId: summary.conversationId,
        sendStarts: new Map(),
        deliveries: new Map(),
        acked: new Set(),
        receivers: 0,
      };
      fanoutRun = run;

      // Subscribe every member and arm its delivery timer. The listener only times ids the
      // steady-state loop minted, so the warm-up below is invisible to the metrics. A member
      // whose watch fails is a setup error but not a wasted run: it simply is not a receiver.
      await runPool(members, FANOUT_SUBSCRIBE_CONCURRENCY, async (vu) => {
        vu.client.messaging.onMessage((message) => {
          const started = run.sendStarts.get(message.messageId);
          if (started === undefined) return;
          ctx.metrics.latency('fanout-deliver').record(performance.now() - started);
          ctx.metrics.recordOk('fanout-deliver');
          run.deliveries.set(message.messageId, (run.deliveries.get(message.messageId) ?? 0) + 1);
        });
        try {
          await vu.client.watchConversation(summary.conversationId);
          run.receivers += 1;
        } catch (error) {
          ctx.metrics.recordError(
            'setup',
            error instanceof Error ? error.name : 'unknown',
            String(error).slice(0, 200),
          );
        }
      });

      // The warm-up send: distributes the sender key to every member device, so measured
      // steady-state sends pay only the per-message cost. Its delivery is not counted (no
      // sendStarts entry), but it must succeed — a failed distribution means later sends would
      // arrive undecryptable, which is a setup failure, not a fan-out measurement.
      await sender.client.messaging.send(summary.conversationId, {
        type: ContentType.Text,
        text: 'loadgen fanout warm-up',
      });
    } catch (error) {
      ctx.metrics.recordError(
        'setup',
        error instanceof Error ? error.name : 'unknown',
        String(error).slice(0, 200),
      );
      fanoutRun = undefined;
    }
  },
  workloads() {
    if (fanoutRun === undefined) return [];
    const run = fanoutRun;
    let seq = 0;
    return [
      (ctx: RunContext) =>
        ctx.paceLoop(async () => {
          seq += 1;
          const messageId = newId();
          run.sendStarts.set(messageId, performance.now());
          const acked = await ctx.measure('send', () =>
            run.sender.client.messaging.send(
              run.conversationId,
              { type: ContentType.Text, text: `loadgen fanout #${seq}` },
              { messageId },
            ),
          );
          if (acked) run.acked.add(messageId);
        }),
    ];
  },
  async settle(_vus, ctx) {
    if (fanoutRun === undefined) return;
    const run = fanoutRun;
    if (run.receivers === 0) return;
    // Wait, bounded, until every acknowledged message has one delivery per receiver — the last
    // send before the deadline is still crossing the wire when the workloads stop.
    const expected = run.acked.size * run.receivers;
    const landedAt = (): number =>
      [...run.acked].reduce((sum, id) => sum + (run.deliveries.get(id) ?? 0), 0);
    const until = performance.now() + FANOUT_SETTLE_MS;
    // An interrupt stops the waiting, never the verdict: whatever has landed by then is ruled on.
    while (landedAt() < expected && performance.now() < until && !ctx.interrupted)
      await sleep(POLL_MS);

    ctx.log.info(
      `fanout verdict: ${run.acked.size} acknowledged sends, ${landedAt()} of ${expected} ` +
        `deliveries across ${run.receivers} receivers`,
    );
    // One verdict sample per message: ok when every receiver got its copy, an error naming the
    // shortfall when one did not. A fan-out that loses or doubles a member is exactly the
    // regression the send-side error budget is blind to.
    for (const id of run.acked) {
      const copies = run.deliveries.get(id) ?? 0;
      if (copies === run.receivers) {
        ctx.metrics.recordOk('fanout-verdict');
      } else {
        ctx.metrics.recordError(
          'fanout-verdict',
          copies < run.receivers ? 'short-fanout' : 'over-fanout',
          `message reached ${copies} of ${run.receivers} receivers`,
        );
      }
    }
  },
};

// ---------------------------------------------------------------------------
// calls: the signaling plane of a thousand simultaneous calls
// ---------------------------------------------------------------------------

/**
 * The size of the sealed placeholder offer and answer, in bytes. An SDP offer with ICE candidates
 * seals to roughly this size; section 165's placeholder material is opaque to the server, so the
 * load generator picks a realistic size and a deterministic fill — no randomness, so two runs of
 * the same shape send the same bytes.
 */
const SEALED_PLACEHOLDER_LEN = 1024;

/** The size of a batched sealed ICE candidate relay, chosen the same way as the offer. */
const SEALED_ICE_LEN = 256;

/** How long a pair holds a call in the Connected state before hanging up. */
const CALL_HOLD_MS = 5_000;

/** How long the caller waits for the callee's SDP relay before declaring the cycle failed. */
const CALL_SETUP_TIMEOUT_MS = 15_000;

/**
 * Deterministic placeholder bytes of `len`, standing in for the sealed offer, answer, or
 * candidate batch. Stable across runs by construction: byte i is a fixed function of i.
 */
function placeholderBytes(len: number): Uint8Array {
  const bytes = new Uint8Array(len);
  for (let i = 0; i < len; i += 1) bytes[i] = (i * 31 + 7) & 0xff;
  return bytes;
}

const PLACEHOLDER_OFFER = placeholderBytes(SEALED_PLACEHOLDER_LEN);
const PLACEHOLDER_ANSWER = placeholderBytes(SEALED_PLACEHOLDER_LEN);
const PLACEHOLDER_ICE = placeholderBytes(SEALED_ICE_LEN);

/** One pair's call-cycle bookkeeping: the caller's pending relay wait and relay timers. */
interface CallPair {
  readonly sender: VirtualUser;
  readonly receiver: VirtualUser;
  readonly conversationId: Id;
  /** The caller's wait for the callee's SDP relay, by callId; resolves with the callee device. */
  readonly pendingSdp: Map<Id, { invitedAt: number; resolve: (device: Id) => void }>;
  /** When the caller handed its ICE batch to sendIce, by callId, for the receiver's timer. */
  readonly iceSentAt: Map<Id, number>;
}

let callsRun: CallPair[] | undefined;

/**
 * Pairs place, answer, relay through and end calls continuously — the whole signaling lifecycle
 * of a 1:1 call, at pair-shaped concurrency.
 *
 * "A thousand simultaneous calls" is a thousand participants mid-call: with the hold window the
 * cycle spends most of its wall-clock between invite and end, so N pairs in their loops hold N
 * concurrent calls — the nightly shape drives `--vus 1000` for five hundred of them. The callee
 * half is driven from its invite listener (auto-answer and SDP reply, exactly what a ringing
 * client does), the caller half from the workload loop, and the measured latencies are the ones
 * a caller feels: invite round trip, the answer relay's arrival (`call-setup` — ring to
 * connected), the ICE relay's arrival at the far end (`call-ice-deliver`), and hang-up.
 *
 * The sealed blobs are placeholder material of realistic size (section 165); the media plane is
 * WebRTC between the two devices and never touches the server, so nothing here is a stub of a
 * server surface — the signaling plane is the whole server surface of a call.
 */
const calls: Scenario = {
  name: 'calls',
  description:
    'Pairs drive the full 1:1 call signaling lifecycle — invite, answer, SDP and ICE relays, ' +
    'end — with sealed placeholder offers (section 165), holding calls concurrently.',
  minVus: 2,
  async prepare(vus, ctx) {
    const pairs = pairUp(vus);
    if (vus.filter((vu) => vu.connected).length % 2 === 1) {
      ctx.log.warn('an odd number of VUs connected; one has no partner and will stay idle');
    }
    await openDirectConversations(pairs, ctx);

    const formed: CallPair[] = [];
    for (const { sender, receiver } of pairs) {
      if (sender.conversationId === undefined) continue;
      const pair: CallPair = {
        sender,
        receiver,
        conversationId: sender.conversationId,
        pendingSdp: new Map(),
        iceSentAt: new Map(),
      };
      formed.push(pair);

      // The callee, driven from its own listener: answer the ring, then relay the sealed answer
      // back to the caller's device. This is the entire callee-side signaling a real client does.
      receiver.client.calls.onIncomingCall((event) => {
        void (async () => {
          const answered = await ctx.measure('call-answer', () =>
            receiver.client.calls.answer(event.callId, PLACEHOLDER_ANSWER),
          );
          if (answered) {
            await ctx.measure('call-sdp', () =>
              receiver.client.calls.sendSdp(event.callId, event.callerDevice, PLACEHOLDER_ANSWER),
            );
          }
        })();
      });
      // The caller's SDP arrival: the ring-to-connected moment. Timed against the invite, and
      // the relay's fromDevice is the target the caller's own ICE batch needs.
      sender.client.calls.onSdp((event) => {
        const pending = pair.pendingSdp.get(event.callId);
        if (pending === undefined) return;
        pair.pendingSdp.delete(event.callId);
        ctx.metrics.latency('call-setup').record(performance.now() - pending.invitedAt);
        ctx.metrics.recordOk('call-setup');
        pending.resolve(event.fromDevice);
      });
      // The callee's ICE arrival: the far end of the caller's relay, timed against sendIce.
      receiver.client.calls.onIce((event) => {
        const sentAt = pair.iceSentAt.get(event.callId);
        if (sentAt === undefined) return;
        ctx.metrics.latency('call-ice-deliver').record(performance.now() - sentAt);
        ctx.metrics.recordOk('call-ice-deliver');
      });
    }
    callsRun = formed;
  },
  workloads() {
    if (callsRun === undefined) return [];
    return callsRun.map((pair) => {
      return async (ctx: RunContext) => {
        while (!ctx.deadlineReached()) {
          const callId = newId();
          const invitedAt = performance.now();
          // Ring to connected: the callee's answer relay resolving the wait. The wait is armed
          // *before* the invite leaves — the relay can beat the invite's own reply back to this
          // device, and a wait armed after would drop a fast answer into a phantom timeout — and
          // bounded, because a signaling plane that lost an answer must fail the cycle, not hang
          // the pair. An invite the server refused disarms its own wait; that failure was already
          // tallied under `call-invite` and must not double-count as a setup timeout.
          let inviteFailed = false;
          const calleeDevice = await new Promise<Id | undefined>((resolve) => {
            const timer = setTimeout(() => {
              pair.pendingSdp.delete(callId);
              resolve(undefined);
            }, CALL_SETUP_TIMEOUT_MS);
            pair.pendingSdp.set(callId, {
              invitedAt,
              resolve: (device) => {
                clearTimeout(timer);
                resolve(device);
              },
            });
            void ctx
              .measure('call-invite', () =>
                pair.sender.client.calls.invite(
                  pair.conversationId,
                  pair.receiver.client.accountId,
                  CallMediaKind.Audio,
                  PLACEHOLDER_OFFER,
                  callId,
                ),
              )
              .then((invited) => {
                if (invited) return;
                inviteFailed = true;
                if (pair.pendingSdp.delete(callId)) clearTimeout(timer);
                resolve(undefined);
              });
          });
          if (inviteFailed) continue;
          if (calleeDevice === undefined) {
            ctx.metrics.recordError(
              'call-setup',
              'timeout',
              `no SDP relay within ${CALL_SETUP_TIMEOUT_MS}ms of the invite`,
            );
            // Best-effort cancel: the invite expires server-side anyway, and a failed cancel
            // after a failed cycle must not mask the setup verdict with a second error.
            await pair.sender.client.calls.cancel(callId).catch(() => undefined);
            continue;
          }

          pair.iceSentAt.set(callId, performance.now());
          await ctx.measure('call-ice', () =>
            pair.sender.client.calls.sendIce(callId, calleeDevice, PLACEHOLDER_ICE),
          );
          // The hold: the window in which the pair counts as a call in progress. Wakes early on
          // the deadline so the run never ends mid-call — every invite gets its end.
          const holdUntil = performance.now() + CALL_HOLD_MS;
          while (!ctx.deadlineReached() && performance.now() < holdUntil) await sleep(POLL_MS);
          pair.iceSentAt.delete(callId);
          await ctx.measure('call-end', () =>
            pair.sender.client.calls.end(callId, CallEndReason.ByCaller),
          );
        }
      };
    });
  },
};

// ---------------------------------------------------------------------------
// voice-notes: a thousand simultaneous upload lifecycles
// ---------------------------------------------------------------------------

/** One second of 16 kHz mono 16-bit PCM, the payload of the synthesized voice note. */
const VOICE_NOTE_SAMPLE_RATE = 16_000;

/** The full synthesized voice note: a 44-byte WAV header plus one second of PCM samples. */
const VOICE_NOTE_BYTES = buildWav(VOICE_NOTE_SAMPLE_RATE);

/** The playing time the upload declares for the note, in milliseconds. */
const VOICE_NOTE_DURATION_MS = 1_000;

/**
 * Builds a minimal, genuinely valid WAV file of `samples` mono 16-bit PCM samples.
 *
 * The media service sniffs magic bytes at commit and refuses content it cannot identify
 * (migo-media's sniffer), so an honest voice-note upload must carry a real `RIFF…WAVE` header —
 * filler bytes would turn the run into a wall of VALIDATION_FAILED refusals that measure the
 * sniffer, not the upload path. The samples themselves are a deterministic ramp: the sniffer
 * reads the header, not the audio, and a fixed pattern keeps two runs byte-identical.
 */
function buildWav(samples: number): Uint8Array {
  const dataLen = samples * 2;
  const bytes = new Uint8Array(44 + dataLen);
  const view = new DataView(bytes.buffer);
  const writeText = (offset: number, text: string): void => {
    for (let i = 0; i < text.length; i += 1) view.setUint8(offset + i, text.charCodeAt(i));
  };
  writeText(0, 'RIFF');
  view.setUint32(4, 36 + dataLen, true);
  writeText(8, 'WAVE');
  writeText(12, 'fmt ');
  view.setUint32(16, 16, true); // the fmt chunk's own size
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, VOICE_NOTE_SAMPLE_RATE, true);
  view.setUint32(28, VOICE_NOTE_SAMPLE_RATE * 2, true); // byte rate
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  writeText(36, 'data');
  view.setUint32(40, dataLen, true);
  for (let i = 0; i < samples; i += 1) view.setInt16(44 + i * 2, (i % 256) * 100 - 12_800, true);
  return bytes;
}

/** The per-run state of the voice-notes scenario: which conversations uploads ride into. */
interface VoiceNotesRun {
  /** Every VU that may upload: both halves of every formed pair (both are members). */
  readonly uploaders: Array<{ vu: VirtualUser; conversationId: Id }>;
}

let voiceNotesRun: VoiceNotesRun | undefined;

/**
 * Every VU uploads voice notes through the full media lifecycle — ticket over the gateway, bytes
 * over HTTP, commit with the content hash — as fast as each upload completes.
 *
 * "A thousand simultaneous uploads" wants a thousand lifecycles in flight, not a thousand per
 * second: closed-loop (`--rate 0`) is the honest shape, one upload in flight per VU, so the
 * nightly run drives `--vus 1000` and the concurrency is the VU count. The measured latency is
 * the whole user-visible upload (begin to commit); the data-plane PUT rides plain HTTP and is
 * deliberately *not* in the gateway wire-byte counters (section 171 counts the socket), so the
 * byte budget covers the control plane only, and the runner reads the server's
 * `migo_media_bytes_committed_total` for the data plane.
 */
const voiceNotes: Scenario = {
  name: 'voice-notes',
  description:
    'Every VU uploads synthesized-but-valid WAV voice notes through the full ' +
    'ticket/PUT/commit lifecycle, closed-loop (one upload in flight per VU).',
  minVus: 2,
  async prepare(vus, ctx) {
    const pairs = pairUp(vus);
    if (vus.filter((vu) => vu.connected).length % 2 === 1) {
      ctx.log.warn('an odd number of VUs connected; one has no partner and will stay idle');
    }
    await openDirectConversations(pairs, ctx);

    // Both halves of a pair upload into their one conversation: membership, not role, is what
    // the media service authorizes, and doubling the uploaders per conversation halves the
    // conversations a run must open for the same upload concurrency.
    const uploaders: VoiceNotesRun['uploaders'] = [];
    for (const { sender, receiver } of pairs) {
      if (sender.conversationId === undefined) continue;
      uploaders.push({ vu: sender, conversationId: sender.conversationId });
      receiver.conversationId = sender.conversationId;
      uploaders.push({ vu: receiver, conversationId: sender.conversationId });
    }
    voiceNotesRun = { uploaders };
  },
  workloads() {
    if (voiceNotesRun === undefined) return [];
    return voiceNotesRun.uploaders.map(({ vu, conversationId }) => {
      return (ctx: RunContext) =>
        ctx.paceLoop(() =>
          ctx.measure('voice-upload', () =>
            vu.client.media.upload(
              {
                kind: MediaKind.VoiceNote,
                contentType: 'audio/wav',
                size: VOICE_NOTE_BYTES.length,
                conversationId,
                durationMs: VOICE_NOTE_DURATION_MS,
              },
              VOICE_NOTE_BYTES,
            ),
          ),
        );
    });
  },
};

// ---------------------------------------------------------------------------
// outage: mass sync after the runner kills the node mid-run
// ---------------------------------------------------------------------------

/** How long the settle phase waits, all windows together, before ruling on integrity. */
const OUTAGE_SETTLE_MS = 60_000;

/** How long deliveries are given to land after the outboxes drain. */
const OUTAGE_DELIVERY_SETTLE_MS = 10_000;

/** How many trailing flush sends fly at once. */
const OUTAGE_FLUSH_CONCURRENCY = 16;

/** One pair's integrity bookkeeping across the outage. */
interface OutagePair {
  readonly sender: VirtualUser;
  readonly receiver: VirtualUser;
  readonly conversationId: Id;
  /** When each message was handed to the outbox, by the scenario-minted id. */
  readonly sendStarts: Map<Id, number>;
  /** Message ids the server acknowledged — everything here must reach the receiver. */
  readonly acked: Set<Id>;
  /** How many times each sent message arrived at the receiver. */
  readonly received: Map<Id, number>;
}

let outageRun: OutagePair[] | undefined;

/**
 * Pairs keep a conversation running while the runner kills and restarts the node underneath them.
 *
 * The senders stream through the offline outbox (`sendQueued`), which is the section-158 client
 * contract under test: a send composed while the link is down is queued, not failed, and its
 * idempotency key makes the after-restart retry safe. The receivers count arrivals by message id.
 * When the deadline passes, the settle phase drains the outboxes, pushes one trailing send per
 * pair (a live arrival above the watermark gap is what triggers the client's sync — without it, a
 * pair whose last message raced the kill would wait forever for a fetch nothing asked for), and
 * then rules: every acknowledged message delivered exactly once, every session back to ready.
 * Duplicates are counted downstream of the SDK's idempotent dispatch, which is part of the system
 * under test — a regression in the server's at-least-once redelivery or the client's dedup both
 * surface here as a count above one.
 *
 * The send latency percentiles of this scenario include the outage itself, by design: a send
 * parked for the restart takes as long as it takes, and hiding that would make the p99 a lie.
 */
const outage: Scenario = {
  name: 'outage',
  description:
    'Pairs stream through the offline outbox while the runner restarts the node mid-run; the ' +
    'settle phase demands full delivery, no duplicates, and every session resumed.',
  minVus: 2,
  async prepare(vus, ctx) {
    const pairs = pairUp(vus);
    if (vus.filter((vu) => vu.connected).length % 2 === 1) {
      ctx.log.warn('an odd number of VUs connected; one has no partner and will stay idle');
    }
    await openDirectConversations(pairs, ctx);

    const formed: OutagePair[] = [];
    for (const { sender, receiver } of pairs) {
      if (sender.conversationId === undefined) continue;
      const pair: OutagePair = {
        sender,
        receiver,
        conversationId: sender.conversationId,
        sendStarts: new Map(),
        acked: new Set(),
        received: new Map(),
      };
      receiver.client.messaging.onMessage((message) => {
        const started = pair.sendStarts.get(message.messageId);
        if (started === undefined) return;
        ctx.metrics.latency('deliver').record(performance.now() - started);
        ctx.metrics.recordOk('deliver');
        pair.received.set(message.messageId, (pair.received.get(message.messageId) ?? 0) + 1);
      });
      formed.push(pair);
    }
    outageRun = formed;
  },
  workloads() {
    if (outageRun === undefined) return [];
    return outageRun.map((pair) => {
      let seq = 0;
      return (ctx: RunContext) =>
        ctx.paceLoop(async () => {
          seq += 1;
          const messageId = newId();
          pair.sendStarts.set(messageId, performance.now());
          const acked = await ctx.measure('send', () =>
            pair.sender.client.sendQueued(
              pair.conversationId,
              { type: ContentType.Text, text: `loadgen outage ${pair.sender.index} #${seq}` },
              { messageId },
            ),
          );
          if (acked) pair.acked.add(messageId);
        });
    });
  },
  async settle(_vus, ctx) {
    if (outageRun === undefined) return;
    const pairs = outageRun;
    const deadline = performance.now() + OUTAGE_SETTLE_MS;

    // Drain: every outbox empty (a queued send still waiting is a send the verdict would
    // wrongly call lost — or worse, wrongly excuse). An interrupt stops the waiting.
    while (performance.now() < deadline && !ctx.interrupted) {
      const pending = pairs.reduce((sum, pair) => sum + (pair.sender.client.outbox?.size ?? 0), 0);
      if (pending === 0) break;
      await sleep(POLL_MS);
    }

    // The trailing send per pair: arms the receiver's gap-fill sync, which is what pulls
    // anything that raced the kill. Not counted in the verdict — it exists to make the sync
    // happen, and its own delivery is the sync's business. Bounded by the settle deadline:
    // against a node that never came back these sends ride the outbox's full retry curve, and
    // an unbounded drain would hold the run open for a failure the verdict reports anyway.
    await runPool(pairs, OUTAGE_FLUSH_CONCURRENCY, async (pair) => {
      if (performance.now() >= deadline || ctx.interrupted) return;
      await ctx.measure('send', () =>
        pair.sender.client.sendQueued(pair.conversationId, {
          type: ContentType.Text,
          text: `loadgen outage ${pair.sender.index} settle`,
        }),
      );
    });

    // Deliveries: wait, bounded, until every acknowledged message has arrived.
    const missingAt = (): number =>
      pairs.reduce(
        (sum, pair) => sum + [...pair.acked].filter((id) => !pair.received.has(id)).length,
        0,
      );
    const deliveryUntil = Math.min(performance.now() + OUTAGE_DELIVERY_SETTLE_MS, deadline);
    while (missingAt() > 0 && performance.now() < deliveryUntil && !ctx.interrupted)
      await sleep(POLL_MS);

    // The verdict. One ok per clean pair, one error per violated pair naming what broke, so the
    // report's error samples say "pair 12: 3 messages missing" rather than "integrity failed".
    let missing = 0;
    let duplicates = 0;
    let unready = 0;
    for (const pair of pairs) {
      const pairMissing = [...pair.acked].filter((id) => !pair.received.has(id)).length;
      const pairDuplicates = [...pair.received.entries()].filter(
        ([id, count]) => pair.acked.has(id) && count > 1,
      ).length;
      const pairReady =
        pair.sender.client.connectionState === 'ready' &&
        pair.receiver.client.connectionState === 'ready';
      missing += pairMissing;
      duplicates += pairDuplicates;
      if (!pairReady) unready += 1;
      if (pairMissing === 0 && pairDuplicates === 0 && pairReady) {
        ctx.metrics.recordOk('outage-verdict');
      } else {
        ctx.metrics.recordError(
          'outage-verdict',
          'integrity',
          `pair ${pair.sender.index}/${pair.receiver.index}: ${pairMissing} missing, ` +
            `${pairDuplicates} duplicated${pairReady ? '' : ', session not ready'}`,
        );
      }
    }
    ctx.log.info(
      `outage verdict: ${pairs.length} pairs, ${missing} messages missing, ` +
        `${duplicates} duplicated, ${unready} pair(s) with a session not back to ready`,
    );
  },
};

export { calls, fanout, outage, voiceNotes };
