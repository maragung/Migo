/**
 * The bots domain: what its requests carry and what its methods return.
 *
 * Every test drives a real {@link Rpc} over the {@link RecordingTransport} double, so both halves
 * of each method are exercised against the generated codecs: what the domain *sent* is decoded
 * back out of the recorded frame body, and what the domain *returned* is decoded from a reply the
 * test encoded. The event listener is exercised the same way, through the double's event
 * injection.
 *
 * Four assertions carry protocol weight beyond shape:
 *
 *   1. **The list request is empty.** `BOT_LIST` answers for the session's own account and takes
 *      no owner parameter, so a client must not smuggle one in — the body on the wire has to
 *      decode as the empty `BotListReq`. A version of this method that sent an account id would
 *      pass every other test here and would be the whole of a cross-account read if the node ever
 *      honoured it.
 *   2. **Rotation sends only the id.** `BOT_ROTATE`'s payload is one field, and a client cannot
 *      name the replacement token — it does not have one until the reply arrives. The test pins
 *      that the body decodes as exactly `{ botId }`.
 *   3. **Scopes are replaced, not merged, and the caller's array is not aliased.** An empty list
 *      must reach the wire as an empty list (the way a permission picker clears every box), and
 *      mutating the array after the call must not change what was sent.
 *   4. **A slug the SDK does not define is still sendable.** {@link BOT_SCOPES} is the SDK's
 *      client-side vocabulary, not an enforcement point: the node is what refuses an unknown
 *      slug, and it must be given the chance to. A domain that filtered the list itself would
 *      silently grant less than it was asked to, which is the failure the node's refusal exists
 *      to prevent.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { decodeBody, encodeBody, BotsDomain, BOT_SCOPES, NO_SCOPES, Rpc } from '../src/index.js';
import type { BotScope } from '../src/index.js';
import { OP } from '@migo/protocol';
import {
  decodeBotCommand,
  decodeBotListReq,
  decodeBotPause,
  decodeBotRegister,
  decodeBotRotate,
  decodeBotScopes,
  encodeBotEvent,
  encodeBotListResponse,
  encodeBotView,
} from '@migo/protocol';

import { RecordingTransport, idOf } from './harness.js';

/** Builds a domain over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
  transport: RecordingTransport;
  bots: BotsDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return { transport, bots: new BotsDomain(rpc) };
}

const BOT = idOf(31);

/** The bot view a reply carries, with every optional field exercised. */
function viewReply(overrides: Partial<Parameters<typeof encodeBotView>[1]> = {}): Uint8Array {
  return encodeBody(encodeBotView, {
    botId: BOT,
    name: 'Weather',
    token: 'mgt_0123456789abcdef',
    paused: false,
    scopes: ['send_messages'],
    ...overrides,
  });
}

test('register sends the handle and the display name, and returns the one-time token', async () => {
  const { transport, bots } = rig(new Map([[OP.BOT_REGISTER, () => viewReply()]]));

  const view = await bots.register('weather', 'Weather');

  assert.equal(transport.sent.length, 1);
  const frame = transport.sent[0];
  assert.ok(frame, 'the domain sent the register frame');
  assert.equal(frame.opcode, OP.BOT_REGISTER);
  const sent = decodeBody(decodeBotRegister, frame.body);
  assert.deepEqual(sent, { username: 'weather', displayName: 'Weather' });

  assert.equal(view.botId, BOT);
  assert.equal(view.name, 'Weather');
  assert.equal(view.token, 'mgt_0123456789abcdef');
  assert.equal(view.paused, false);
  assert.deepEqual(view.scopes, ['send_messages']);
});

test('list sends an empty request and returns the bots the node named', async () => {
  const { transport, bots } = rig(
    new Map([
      [
        OP.BOT_LIST,
        () =>
          encodeBody(encodeBotListResponse, {
            bots: [
              { botId: BOT, name: 'Weather', paused: false, scopes: [] },
              { botId: idOf(32), name: 'Clock', paused: true, scopes: ['read_members'] },
            ],
          }),
      ],
    ]),
  );

  const listed = await bots.list();

  // The whole request: no owner, no filter, nothing a later build could honour as a query for
  // somebody else's bots.
  const sent = decodeBody(decodeBotListReq, transport.sent[0]!.body);
  assert.deepEqual(sent, {});

  assert.equal(listed.length, 2);
  assert.equal(listed[0]?.botId, BOT);
  assert.equal(listed[1]?.name, 'Clock');
  assert.equal(listed[1]?.paused, true);
  assert.deepEqual(listed[1]?.scopes, ['read_members']);
});

test('list leaves a node that omits the tagged fields undecoded rather than inventing them', async () => {
  // A node built before pausing existed omits both tagged fields. They must surface as absent,
  // not as a paused bot holding no permissions — the difference between "this build did not say"
  // and "this bot is off" is one a management screen has to render honestly.
  const { bots } = rig(
    new Map([
      [
        OP.BOT_LIST,
        () => encodeBody(encodeBotListResponse, { bots: [{ botId: BOT, name: 'Old' }] }),
      ],
    ]),
  );

  const listed = await bots.list();

  assert.equal(listed[0]?.paused, undefined);
  assert.equal(listed[0]?.scopes, undefined);
});

test('rotate sends the bot id alone and returns the replacement token', async () => {
  const { transport, bots } = rig(
    new Map([[OP.BOT_ROTATE, () => viewReply({ token: 'mgt_replacement' })]]),
  );

  const view = await bots.rotate(BOT);

  const frame = transport.sent[0];
  assert.ok(frame, 'the domain sent the rotate frame');
  assert.equal(frame.opcode, OP.BOT_ROTATE);
  assert.deepEqual(decodeBody(decodeBotRotate, frame.body), { botId: BOT });
  assert.equal(view.token, 'mgt_replacement');
  // Rotation moves nothing else, and the view says so.
  assert.equal(view.name, 'Weather');
});

test('setPaused sends the flag both ways rather than implying it from the call', async () => {
  const { transport, bots } = rig(
    new Map([
      [OP.BOT_PAUSE, (body) => viewReply({ paused: decodeBody(decodeBotPause, body).paused })],
    ]),
  );

  const paused = await bots.setPaused(BOT, true);
  assert.deepEqual(decodeBody(decodeBotPause, transport.sent[0]!.body), {
    botId: BOT,
    paused: true,
  });
  assert.equal(paused.paused, true);

  const resumed = await bots.setPaused(BOT, false);
  assert.deepEqual(decodeBody(decodeBotPause, transport.sent[1]!.body), {
    botId: BOT,
    paused: false,
  });
  assert.equal(resumed.paused, false);
});

test('setScopes sends exactly the set given, copies it, and does not filter it', async () => {
  const { transport, bots } = rig(
    new Map([
      [OP.BOT_SCOPES, (body) => viewReply({ scopes: decodeBody(decodeBotScopes, body).scopes })],
    ]),
  );

  const wanted = ['send_messages', 'read_members', 'not_a_scope_this_build_knows'];
  const view = await bots.setScopes(BOT, wanted as unknown as readonly BotScope[]);

  // Exactly what was asked for, in order, with nothing dropped: the node is the authority on
  // which slugs exist, and a domain that pre-filtered would deny it the chance to refuse.
  assert.deepEqual(decodeBody(decodeBotScopes, transport.sent[0]!.body), {
    botId: BOT,
    scopes: wanted,
  });
  assert.deepEqual(view.scopes, wanted);

  // Clearing every box is a complete request, not an omitted field.
  await bots.setScopes(BOT, NO_SCOPES);
  assert.deepEqual(decodeBody(decodeBotScopes, transport.sent[1]!.body), {
    botId: BOT,
    scopes: [],
  });

  // The caller's array is copied, so a later mutation cannot rewrite what was sent.
  const mutable = ['moderate'];
  await bots.setScopes(BOT, mutable as unknown as readonly BotScope[]);
  mutable.push('manage_games');
  assert.deepEqual(decodeBody(decodeBotScopes, transport.sent[2]!.body), {
    botId: BOT,
    scopes: ['moderate'],
  });
});

test('every slug the SDK offers is one the node defines, spelled the way it spells them', () => {
  // The vocabulary is duplicated on purpose — the wire carries strings, so the protocol cannot
  // know which ones mean something — and this is the only thing that keeps the copy honest.
  assert.deepEqual(
    [...BOT_SCOPES],
    [
      'read_messages',
      'send_messages',
      'moderate',
      'manage_games',
      'read_members',
      'send_announcements',
    ],
  );
  assert.equal(new Set(BOT_SCOPES).size, BOT_SCOPES.length, 'no slug is offered twice');
});

test('command omits args when none are given and carries them in order when they are', async () => {
  const { transport, bots } = rig(new Map([[OP.BOT_COMMAND, () => new Uint8Array()]]));

  await bots.command(BOT, 'forecast');
  assert.deepEqual(decodeBody(decodeBotCommand, transport.sent[0]!.body), {
    botId: BOT,
    command: 'forecast',
  });

  await bots.command(BOT, 'forecast', ['tomorrow', 'jakarta']);
  assert.deepEqual(decodeBody(decodeBotCommand, transport.sent[1]!.body), {
    botId: BOT,
    command: 'forecast',
    args: ['tomorrow', 'jakarta'],
  });
});

// Delivery is synchronous — `emit` walks the transport's subscribers on the calling stack — so
// this test is deliberately not async: there is nothing to await, and an `async` wrapper here
// would suggest the event arrives on a later tick when the point is that it does not.
test('bot events reach a registered handler and stop reaching it once unsubscribed', () => {
  const { transport, bots } = rig(new Map());
  bots.start();

  const seen: string[] = [];
  const off = bots.onBotEvent((event) => seen.push(event.event));

  transport.emit(OP.BOT_EVENT, encodeBody(encodeBotEvent, { botId: BOT, event: 'webhook_failed' }));
  assert.deepEqual(seen, ['webhook_failed']);

  off();
  transport.emit(OP.BOT_EVENT, encodeBody(encodeBotEvent, { botId: BOT, event: 'webhook_failed' }));
  assert.deepEqual(seen, ['webhook_failed'], 'an unsubscribed handler hears nothing more');

  // Stopping the domain drops the transport subscription, so a later event reaches nobody at all.
  bots.stop();
  transport.emit(OP.BOT_EVENT, encodeBody(encodeBotEvent, { botId: BOT, event: 'webhook_failed' }));
  assert.deepEqual(seen, ['webhook_failed']);
});
