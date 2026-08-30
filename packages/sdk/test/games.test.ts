/**
 * The games domain: what its requests carry and what its methods return.
 *
 * Like {@link domains.test.ts}, every test drives a real {@link Rpc} over the {@link
 * RecordingTransport} double, so both halves of each method are exercised against the generated
 * codecs: what the domain *sent* is decoded back out of the recorded frame body (a mismatched
 * struct would fail to decode or decode wrong), and what the domain *returned* is decoded from a
 * reply the test encoded. The event listener is exercised the same way, through the double's
 * event injection.
 *
 * Two assertions carry protocol weight beyond shape:
 *
 *   1. **The catalogue request is empty.** `GAME_CATALOGUE` reuses the gift catalogue's empty
 *      request struct, so a client must not smuggle filters into it — the body on the wire has
 *      to decode as the empty `GiftCatalogueReq`.
 *   2. **A retry reuses the action id.** `submit` mints ids from a per-game counter, and the
 *      protocol's replay protection only works if an idempotent retry passes the *previous* id
 *      rather than accepting a newly minted one. The test pins both halves: the auto-minted
 *      sequence and the explicit override.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { decodeBody, encodeBody, GamesDomain, Rpc } from '../src/index.js';
import { OP } from '@migo/protocol';
import {
  decodeGameAction,
  decodeGameId,
  decodeGameStart,
  decodeGiftCatalogueReq,
  encodeAcknowledged,
  encodeGameCatalogueResponse,
  encodeGameEvent,
  encodeGameViewWire,
} from '@migo/protocol';

import { RecordingTransport, idOf } from './harness.js';

/** Builds a domain over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
  transport: RecordingTransport;
  games: GamesDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return { transport, games: new GamesDomain(rpc) };
}

const CONVERSATION = idOf(21);
const GAME = idOf(22);
const PLAYER = idOf(7);

/** The opening view a `GAME_START` reply carries, with every optional field exercised. */
function viewReply(): Uint8Array {
  return encodeBody(encodeGameViewWire, {
    gameId: GAME,
    kind: 2,
    conversationId: CONVERSATION,
    status: 0,
    players: [PLAYER],
    stateVersion: 1,
    board: '1-100:7',
    turnOf: PLAYER,
    yourTurn: true,
  });
}

test('games: getCatalogue sends the empty request and decodes the entries', async () => {
  const { transport, games } = rig(
    new Map([
      [
        OP.GAME_CATALOGUE,
        () =>
          encodeBody(encodeGameCatalogueResponse, {
            games: [
              { slug: 'guess_number', kind: 2, minPlayers: 1, maxPlayers: 1 },
              { slug: 'tic_tac_toe', kind: 0, minPlayers: 2, maxPlayers: 2 },
            ],
          }),
      ],
    ]),
  );
  const entries = await games.getCatalogue();
  assert.equal(transport.sent[0]?.opcode, OP.GAME_CATALOGUE);
  // The request body must decode as the empty struct: the catalogue is the node's own, and the
  // opcode reuses the gift catalogue's request type precisely because it has nothing to say.
  assert.deepEqual(
    decodeBody(decodeGiftCatalogueReq, transport.sent[0]?.body ?? new Uint8Array()),
    {},
  );
  assert.deepEqual(entries, [
    { slug: 'guess_number', kind: 2, minPlayers: 1, maxPlayers: 1 },
    { slug: 'tic_tac_toe', kind: 0, minPlayers: 2, maxPlayers: 2 },
  ]);
});

test('games: startGame names the conversation and the slug, and returns the opening view', async () => {
  const { transport, games } = rig(new Map([[OP.GAME_START, viewReply]]));
  const view = await games.startGame(CONVERSATION, 'guess_number');
  assert.equal(transport.sent[0]?.opcode, OP.GAME_START);
  assert.deepEqual(decodeBody(decodeGameStart, transport.sent[0]?.body ?? new Uint8Array()), {
    conversationId: CONVERSATION,
    slug: 'guess_number',
  });
  assert.equal(view.gameId, GAME);
  assert.equal(view.conversationId, CONVERSATION);
  assert.equal(view.yourTurn, true);
  assert.equal(view.turnOf, PLAYER);
  assert.equal(view.board, '1-100:7');
});

test('games: getView names only the game, and returns the redacted view', async () => {
  const { transport, games } = rig(new Map([[OP.GAME_VIEW, viewReply]]));
  const view = await games.getView(GAME);
  assert.equal(transport.sent[0]?.opcode, OP.GAME_VIEW);
  assert.deepEqual(decodeBody(decodeGameId, transport.sent[0]?.body ?? new Uint8Array()), {
    gameId: GAME,
  });
  assert.equal(view.gameId, GAME);
});

test('games: submit mints monotonic action ids per game, and an explicit id overrides', async () => {
  const { transport, games } = rig(
    new Map([[OP.GAME_ACTION, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await games.submit(GAME, CONVERSATION, 'guess', { args: ['50'] });
  await games.submit(GAME, CONVERSATION, 'guess', { args: ['25'] });
  await games.submit(GAME, CONVERSATION, 'guess', { actionId: 1, args: ['50'] });

  const first = decodeBody(decodeGameAction, transport.sent[0]?.body ?? new Uint8Array());
  const second = decodeBody(decodeGameAction, transport.sent[1]?.body ?? new Uint8Array());
  const retry = decodeBody(decodeGameAction, transport.sent[2]?.body ?? new Uint8Array());
  assert.equal(first.actionId, 1, 'the first action of a game must carry id 1');
  assert.equal(second.actionId, 2, 'the next action must advance the per-game counter');
  assert.equal(retry.actionId, 1, 'an explicit action id must override the minted one');
  assert.deepEqual(retry.args, ['50'], 'the retried action must repeat its arguments verbatim');
  // Two different games keep independent counters.
  const other = idOf(23);
  await games.submit(other, CONVERSATION, 'guess', {});
  assert.equal(
    decodeBody(decodeGameAction, transport.sent[3]?.body ?? new Uint8Array()).actionId,
    1,
    'a second game must start its counter afresh',
  );
});

test('games: onGameEvent delivers decoded events once started, and stops cleanly', () => {
  const { transport, games } = rig(new Map());
  const seen: string[] = [];
  const off = games.onGameEvent((event) => seen.push(event.event));

  games.start();
  transport.emit(
    OP.GAME_EVENT,
    encodeBody(encodeGameEvent, {
      gameId: GAME,
      roomId: CONVERSATION,
      stateVersion: 3,
      event: 'moved',
      actorId: PLAYER,
    }),
  );
  assert.deepEqual(seen, ['moved']);

  games.stop();
  transport.emit(
    OP.GAME_EVENT,
    encodeBody(encodeGameEvent, {
      gameId: GAME,
      roomId: CONVERSATION,
      stateVersion: 4,
      event: 'finished',
    }),
  );
  assert.equal(seen.length, 1, 'an event after stop() must not be delivered');
  off();
});
