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
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  decodeBody,
  encodeBody,
  GroupCrypto,
  MessagingDomain,
  Rpc,
  SessionCrypto,
} from '../src/index.js';
import type { DeviceAddress, DeviceDirectory } from '../src/index.js';
import { OP } from '@migo/protocol';
import { decodeMessageEdit, decodeReactionSet, encodeAcknowledged } from '@migo/protocol';

import { RecordingTransport, StaticBundleSource, bundleFrom, idOf, newStore } from './harness.js';

const CONVERSATION = idOf(1);
const MESSAGE = idOf(2);
/** Bytes a caller sealed before calling: the domain must pass them through verbatim. */
const SEALED = new Uint8Array([9, 8, 7, 6, 5]);

/** Builds a messaging domain over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
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
  return { transport, messaging: new MessagingDomain(rpc, sessionCrypto, groupCrypto, directory) };
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
