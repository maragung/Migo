/**
 * The messaging domain's in-place message mutations: edit and reaction.
 *
 * Like {@link domains.test.ts}, every test drives a real {@link Rpc} over the {@link
 * RecordingTransport} double, so both halves of each method are exercised against the generated
 * codecs: what the domain *sent* is decoded back out of the recorded frame body, and what the
 * domain *returned* resolves from a reply the test encoded. The crypto layers are the real ones
 * only so the domain can be constructed; an edit and a reaction carry envelopes the caller sealed
 * beforehand, so the domain's job here is verbatim pass-through — the recorded frame must contain
 * the exact bytes handed in, untouched by any re-seal.
 *
 * The gap-scheduling tests drive the same double from the other direction, with {@link
 * RecordingTransport.emit} standing in for the server's fan-out: what the domain does with a
 * sequence number that lands above its watermark (ask the {@link GapFiller} once, hold the ask
 * while a fill runs, stall after a fill that moved nothing, continue after one that did) is pure
 * accounting, pinned here at the seam the client implements.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ContentType,
  decodeBody,
  encodeBody,
  GroupCrypto,
  MessagingDomain,
  Rpc,
  SessionCrypto,
} from '../src/index.js';
import type { DeviceAddress, DeviceDirectory, GapFiller, TextContent } from '../src/index.js';
import { OP, MessageKind } from '@migo/protocol';
import {
  decodeMessageEdit,
  decodeMessageSend,
  decodeReactionSet,
  encodeAcknowledged,
  encodeMessageAccepted,
  encodeMessageEvent,
} from '@migo/protocol';
import type { MessageEvent } from '@migo/protocol';

import { RecordingTransport, StaticBundleSource, bundleFrom, idOf, newStore } from './harness.js';

const CONVERSATION = idOf(1);
const MESSAGE = idOf(2);
/** Bytes a caller sealed before calling: the domain must pass them through verbatim. */
const SEALED = new Uint8Array([9, 8, 7, 6, 5]);

/** Builds a messaging domain over one recording transport, with per-opcode canned replies. */
function rig(
  replies: Map<number, (body: Uint8Array) => Uint8Array>,
  gapFiller?: GapFiller,
): {
  transport: RecordingTransport;
  messaging: MessagingDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  const store = newStore();
  const sessionCrypto = new SessionCrypto(store, new StaticBundleSource(bundleFrom(store)));
  const groupCrypto = new GroupCrypto(store);
  const directory: DeviceDirectory = {
    recipientDevices(): Promise<DeviceAddress[]> {
      return Promise.resolve([]);
    },
  };
  return {
    transport,
    messaging: new MessagingDomain(
      rpc,
      sessionCrypto,
      groupCrypto,
      directory,
      undefined,
      gapFiller,
    ),
  };
}

/** The frame recorded at `index`, narrowed to present (see domains.test.ts for the rationale). */
function sentAt(
  transport: RecordingTransport,
  index: number,
): { opcode: number; body: Uint8Array } {
  const frame = transport.sent[index];
  assert.ok(frame !== undefined, `expected a recorded frame at index ${index}`);
  return frame;
}

test('messaging: editMessage sends MESSAGE_EDIT with the sealed replacement verbatim', async () => {
  const { transport, messaging } = rig(
    new Map([[OP.MESSAGE_EDIT, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await messaging.editMessage(CONVERSATION, MESSAGE, SEALED);

  assert.equal(transport.sent.length, 1);
  assert.equal(sentAt(transport, 0).opcode, OP.MESSAGE_EDIT);
  // The edit names the message and its conversation, and carries the replacement envelope
  // unchanged: the domain never re-seals what the caller already sealed.
  assert.deepEqual(decodeBody(decodeMessageEdit, sentAt(transport, 0).body), {
    messageId: MESSAGE,
    conversationId: CONVERSATION,
    envelope: SEALED,
  });
});

test('messaging: sendReaction sends REACTION_SET addressed to the target message', async () => {
  const { transport, messaging } = rig(
    new Map([[OP.REACTION_SET, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await messaging.sendReaction(MESSAGE, CONVERSATION, SEALED);

  assert.equal(transport.sent.length, 1);
  assert.equal(sentAt(transport, 0).opcode, OP.REACTION_SET);
  // The server learns only *that* a reaction was set on the target message inside this
  // conversation — which emoji lives sealed inside the envelope.
  assert.deepEqual(decodeBody(decodeReactionSet, sentAt(transport, 0).body), {
    targetMessageId: MESSAGE,
    conversationId: CONVERSATION,
    envelope: SEALED,
  });
});

test('messaging: send reuses a caller-supplied message id and mints one without', async () => {
  // The server's send idempotency is keyed on the message id, so a retry that re-sends the same
  // message must carry the same id or it becomes a second row. The domain cannot invent that
  // policy — it does not know whether its caller is retrying — so it takes the id through
  // SendOptions and mints a fresh one only when the caller supplied none.
  const accepted = { messageId: idOf(2), conversationId: CONVERSATION, seq: 7, createdAt: 0 };
  const replies = new Map([[OP.MESSAGE_SEND, () => encodeBody(encodeMessageAccepted, accepted)]]);

  const text: TextContent = { type: ContentType.Text, text: 'a retry-safe send' };

  // Supplied: the recorded MESSAGE_SEND carries exactly that id — the retry's id.
  const chosen = rig(replies);
  await chosen.messaging.send(CONVERSATION, text, { messageId: MESSAGE });
  assert.equal(chosen.transport.sent.length, 1);
  assert.equal(sentAt(chosen.transport, 0).opcode, OP.MESSAGE_SEND);
  const withSupplied = decodeBody(decodeMessageSend, sentAt(chosen.transport, 0).body);
  assert.equal(withSupplied.messageId, MESSAGE);

  // Omitted: a minted id, which by construction differs from every id the test fixed.
  const minted = rig(replies);
  await minted.messaging.send(CONVERSATION, text);
  assert.equal(minted.transport.sent.length, 1);
  const withMinted = decodeBody(decodeMessageSend, sentAt(minted.transport, 0).body);
  assert.notEqual(withMinted.messageId, MESSAGE);
  assert.notEqual(withMinted.messageId, 0n, 'an omitted id minted to the zero id');
});

/** A synthetic pushed event, as the gap tests feed the live path. The envelope is filler on purpose: the watermark accounting the tests pin happens before dispatch, and the pending buffer the filler lands in is bounded and dies with the domain. */
function pushedEvent(seq: number): MessageEvent {
  return {
    messageId: idOf(0x10_00 + seq),
    conversationId: CONVERSATION,
    seq,
    senderId: idOf(20),
    senderDevice: idOf(0x9001),
    kind: MessageKind.Text,
    envelope: new Uint8Array([seq]),
    createdAt: 1_700_000_000_000,
  };
}

test('messaging: a detected gap asks the gap filler once, and a stalled ask waits for the watermark to move', async () => {
  const asked: number[] = [];
  // A holder rather than a bare `let`: the assignment happens inside the filler's closure,
  // which the compiler cannot see at the release site.
  const pending: { release: () => void } = { release: () => {} };
  const filler: GapFiller = {
    fillGap(_conversationId, toSeq) {
      asked.push(toSeq);
      return new Promise<void>((resolve) => {
        pending.release = resolve;
      });
    },
  };
  const { transport, messaging } = rig(new Map(), filler);
  messaging.start();
  const deliver = (seq: number): void => {
    transport.emit(OP.MESSAGE_EVENT, encodeBody(encodeMessageEvent, pushedEvent(seq)));
  };

  // Contiguous delivery asks nothing: there is no hole to fill.
  deliver(1);
  deliver(2);
  deliver(3);
  assert.equal(asked.length, 0);

  // The first above-gap event asks, for the top of what has arrived.
  deliver(7);
  assert.equal(asked.length, 1);
  assert.equal(asked[0], 7);

  // Events while the fill is in flight are that fill's business, not new asks.
  deliver(8);
  deliver(9);
  assert.equal(asked.length, 1);

  // The fill resolves without moving the watermark (the server had nothing for the hole):
  // stalled, and later above-gap events re-ask nothing.
  pending.release();
  await new Promise((resolve) => setTimeout(resolve, 0));
  deliver(10);
  assert.equal(asked.length, 1, 'a stalled fill is not re-asked per event');

  // The watermark moves — 4 arrives live, closing part of the hole from below — which lifts
  // the stall without asking anything by itself...
  deliver(4);
  assert.equal(asked.length, 1);

  // ...so the next above-gap event asks again, for the new top.
  deliver(11);
  assert.equal(asked.length, 2);
  assert.equal(asked[1], 11);
});

test('messaging: a fill that made progress continues on its own when events arrive above its target', async () => {
  const asked: number[] = [];
  const pages: { land: (seqs: number[]) => void } = { land: () => {} };
  let transport: RecordingTransport | undefined;
  const filler: GapFiller = {
    fillGap(_conversationId, toSeq) {
      asked.push(toSeq);
      return new Promise<void>((resolve) => {
        pages.land = (seqs) => {
          for (const seq of seqs) {
            transport?.emit(OP.MESSAGE_EVENT, encodeBody(encodeMessageEvent, pushedEvent(seq)));
          }
          resolve();
        };
      });
    },
  };
  const rigged = rig(new Map(), filler);
  transport = rigged.transport;
  const messaging = rigged.messaging;
  messaging.start();

  messaging.ingest(pushedEvent(1));
  messaging.ingest(pushedEvent(2));
  messaging.ingest(pushedEvent(3));

  // The fill is asked for the hole up to 7; while it runs, 8 arrives above its target.
  messaging.ingest(pushedEvent(7));
  assert.deepEqual(asked, [7]);
  messaging.ingest(pushedEvent(8));

  // The fill's pages land and move the watermark — only part of the way, but progress: no
  // stall is recorded, and the events that arrived above the target are a new hole the domain
  // schedules by itself as the fill ends, without waiting for another event to notice.
  pages.land([4, 5]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(asked, [7, 8], 'the fill continued to the new top on its own');
  assert.equal(messaging.watermark(CONVERSATION), 5);

  // The continuation closes what is left, and the accounting agrees.
  pages.land([6, 7, 8]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(messaging.watermark(CONVERSATION), 8);
  assert.equal(asked.length, 2);
});
