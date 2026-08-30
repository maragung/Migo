/**
 * The rooms domain: what each room-management request carries, and what its replies decode to.
 *
 * Like {@link domains.test.ts}, every test drives a real {@link Rpc} over the {@link
 * RecordingTransport} double, so both halves of each method are exercised against the generated
 * codecs: what the domain *sent* is decoded back out of the recorded frame body (a mismatched
 * struct would fail to decode or decode wrong), and what the domain *returned* is decoded from a
 * reply the test encoded.
 *
 * Two assertions carry protocol weight beyond shape:
 *
 *   1. **A create is a join.** `ROOM_CREATE` replies with the same `RoomJoinResponse` `ROOM_JOIN`
 *      does, because creation is entry — the creator is the room's first member — so the method
 *      must resolve with the conversation handle, not a bare acknowledgement.
 *   2. **Absent page parameters stay absent.** The roster request's `limit` and `after` encode by
 *      presence, so an unbounded read must leave both off the wire entirely rather than send
 *      zeros — a zero `limit` is a client-bound page of nothing, and a zero `after` is a cursor
 *      naming an account that was never seen.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { decodeBody, encodeBody, RoomKind, RoomRole, RoomsDomain, Rpc } from '../src/index.js';
import { OP } from '@migo/protocol';
import {
  decodeRoomCreate,
  decodeRosterReq,
  encodeRoomJoinResponse,
  encodeRosterResponse,
} from '@migo/protocol';
import type { RoomJoinResponse, RoomSummary } from '@migo/protocol';

import { RecordingTransport, idOf } from './harness.js';

const ROOM = idOf(1);
const CONVERSATION = idOf(2);
const OWNER = idOf(3);
const MEMBER = idOf(4);
/** A Unix-ms instant after the Migo epoch (2024-01-01), so timestamps round-trip through the codec. */
const AT = 1_767_225_600_000;

/** Builds a domain over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
  transport: RecordingTransport;
  rooms: RoomsDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return { transport, rooms: new RoomsDomain(rpc) };
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

/** The room summary these tests' join replies carry: public, the creator owning it. */
const ROOM_SUMMARY: RoomSummary = {
  roomId: ROOM,
  publicId: 'MGO-ROOM',
  kind: RoomKind.Public,
  name: 'Espresso Bar',
  memberCount: 1,
  onlineCount: 1,
  myRole: RoomRole.Owner,
};

/** A `RoomJoinResponse` for the room these tests create or join, as the server would reply. */
function joinReply(): Uint8Array {
  const response: RoomJoinResponse = {
    room: ROOM_SUMMARY,
    conversationId: CONVERSATION,
    encryption: 3,
    lastSeq: 0,
  };
  return encodeBody(encodeRoomJoinResponse, response);
}

test('rooms: create sends ROOM_CREATE with slug, name, kind, and topic', async () => {
  const { transport, rooms } = rig(new Map([[OP.ROOM_CREATE, joinReply]]));
  const joined = await rooms.create('espresso', 'Espresso Bar', RoomKind.Public, 'single origin');

  assert.equal(transport.sent.length, 1);
  assert.equal(sentAt(transport, 0).opcode, OP.ROOM_CREATE);
  const request = decodeBody(decodeRoomCreate, sentAt(transport, 0).body);
  assert.deepEqual(request, {
    slug: 'espresso',
    name: 'Espresso Bar',
    kind: RoomKind.Public,
    topic: 'single origin',
  });

  // Creation is entry: the reply is the same join handle ROOM_JOIN returns, and the caller
  // starts reading and writing the room through the conversation id it carries.
  assert.deepEqual(joined, {
    room: ROOM_SUMMARY,
    conversationId: CONVERSATION,
    encryption: 3,
    lastSeq: 0,
  });
});

test('rooms: create leaves the topic off the wire when none is given', async () => {
  const { transport, rooms } = rig(new Map([[OP.ROOM_CREATE, joinReply]]));
  await rooms.create('espresso', 'Espresso Bar', RoomKind.Managed);
  const request = decodeBody(decodeRoomCreate, sentAt(transport, 0).body);
  assert.equal(request.topic, undefined, 'an absent topic must not ride as an empty string');
  assert.equal(request.maxMembers, undefined);
});

test('rooms: getRoster pages with the limit and the after cursor', async () => {
  const after = idOf(41);
  const { transport, rooms } = rig(
    new Map([
      [
        OP.ROOM_ROSTER,
        () =>
          encodeBody(encodeRosterResponse, {
            members: [
              { accountId: OWNER, role: RoomRole.Owner, joinedAt: AT },
              { accountId: MEMBER, role: RoomRole.Member, joinedAt: AT },
            ],
          }),
      ],
    ]),
  );
  const members = await rooms.getRoster(ROOM, 25, after);

  assert.equal(sentAt(transport, 0).opcode, OP.ROOM_ROSTER);
  assert.deepEqual(decodeBody(decodeRosterReq, sentAt(transport, 0).body), {
    roomId: ROOM,
    limit: 25,
    after,
  });

  // Highest role first, as the wire promises; the role is the raw RoomRole number.
  assert.deepEqual(members, [
    { accountId: OWNER, role: RoomRole.Owner, joinedAt: AT },
    { accountId: MEMBER, role: RoomRole.Member, joinedAt: AT },
  ]);
});

test('rooms: getRoster sends only the room when unbounded', async () => {
  const { transport, rooms } = rig(
    new Map([[OP.ROOM_ROSTER, () => encodeBody(encodeRosterResponse, { members: [] })]]),
  );
  const members = await rooms.getRoster(ROOM);
  assert.deepEqual(members, []);
  // Both page parameters encode by presence; a zero limit or zero cursor would be a page the
  // caller never asked for, so they must be absent rather than defaulted.
  assert.deepEqual(decodeBody(decodeRosterReq, sentAt(transport, 0).body), { roomId: ROOM });
});
