/**
 * What the shell is allowed to remember and claim about a room.
 *
 * Rooms and conversations are separate frames on the wire, joined only at the moment of the
 * join reply, so the shell keeps its own projection — and its rules are the ones this file
 * pins, because each would silently degrade under a plausible refactor:
 *
 *   1. **A room-state event is a delta, not a snapshot.** The server sends only the fields that
 *      changed; applying it as a whole record would blank the topic every time the online count
 *      moved. The test sends a counter-only delta after a topic was set and asserts the topic
 *      survives.
 *   2. **A room row is named by the shell's record, not the summary's title.** The conversation
 *      list leaves `title` unset for rooms, so the row's name comes from the join (or the
 *      remembered copy) — and it wears the `#` glyph that says "room" before the row is opened.
 *   3. **The persisted copy is the account's.** Room names map where an account spends its time;
 *      a stored copy naming another account is discarded on load rather than merged, so the
 *      next account on the same device inherits nothing.
 *   4. **A remembered room is stored only in IndexedDB.** The audit rule that governs keys and
 *      grants governs this record too, so the test watches the forbidden surfaces while the
 *      store writes.
 *   5. **A room's capacity is a fact the shell carries and the label renders.** The join sets the
 *      ceiling and a state delta can move it, each without disturbing the counters or the topic;
 *      the label reads "here/max" only when a real maximum is known, and the bare head-count
 *      otherwise.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import { ConversationKind, EncryptionMode, MemberChange } from '@migo/sdk';
import type { Id, RoomJoinResponse, RoomMemberEvent, RoomSummary } from '@migo/sdk';

import {
  applyRoomState,
  capacityLabel,
  departedRoomOf,
  roomInfoOf,
} from '../src/lib/migo/rooms-provider.js';
import { roomRowTitle } from '../src/components/conversation-list.js';
import { clearRoomInfo, loadRoomInfo, saveRoomInfo } from '../src/lib/storage/room-info-store.js';
import { installFakeIndexedDb, installRecordingWebStorage } from './support/dom-stubs.js';
import type { FakeIndexedDb, RecordingWebStorage } from './support/dom-stubs.js';

const ROOM: RoomSummary = {
  roomId: 'room_1' as Id,
  publicId: 'MGO-ROOM',
  kind: 1,
  name: 'Observatory',
  memberCount: 12,
  onlineCount: 3,
  topic: 'What is above us',
};

function joined(overrides: Partial<RoomJoinResponse> = {}): RoomJoinResponse {
  return {
    room: ROOM,
    conversationId: 'conv_1' as Id,
    encryption: EncryptionMode.None,
    lastSeq: 41,
    ...overrides,
  };
}

let idb: FakeIndexedDb;
let webStorage: RecordingWebStorage;

beforeEach(() => {
  idb = installFakeIndexedDb();
  webStorage = installRecordingWebStorage();
});

afterEach(() => {
  idb.restore();
  webStorage.restore();
});

test('a join reply projects onto the shell\u2019s room record with only the fields the wire set', () => {
  const info = roomInfoOf(joined());
  assert.equal(info.roomId, ROOM.roomId);
  assert.equal(info.conversationId, 'conv_1' as Id);
  assert.equal(info.name, 'Observatory');
  assert.equal(info.topic, 'What is above us');
  assert.equal(info.memberCount, 12);
  assert.equal(info.onlineCount, 3);
  // A join of a room with no topic carries no topic key at all, not an undefined-valued one.
  const bare = roomInfoOf(joined({ room: { ...ROOM, topic: undefined } }));
  assert.ok(!('topic' in bare), 'an absent topic must stay absent');
});

test('a room-state delta replaces only the fields it carries', () => {
  let info = roomInfoOf(joined());
  // First the topic arrives on its own; then the counters move without mentioning it.
  info = applyRoomState(info, { roomId: ROOM.roomId, topic: 'Look up instead' });
  assert.equal(info.topic, 'Look up instead');
  info = applyRoomState(info, { roomId: ROOM.roomId, onlineCount: 7, memberCount: 13 });
  assert.equal(info.onlineCount, 7);
  assert.equal(info.memberCount, 13);
  assert.equal(info.topic, 'Look up instead', 'a counter delta must not blank the topic');
  // A delta that carries nothing but the room it names changes nothing: absence is "unchanged",
  // the exact reading that makes the delta shape worth having.
  const untouched = applyRoomState(info, { roomId: ROOM.roomId });
  assert.deepEqual(untouched, info);
});

test('a join carries the room’s capacity, and a state delta moves it without touching the rest', () => {
  const info = roomInfoOf(joined({ room: { ...ROOM, maxMembers: 33 } }));
  assert.equal(info.maxMembers, 33);
  // A capacity the room never stated stays absent, not an undefined-valued key.
  const bare = roomInfoOf(joined());
  assert.ok(!('maxMembers' in bare), 'an absent capacity must stay absent');
  // A delta that raises the ceiling moves only it; the counters and topic it does not name hold.
  const raised = applyRoomState(info, { roomId: ROOM.roomId, maxMembers: 50 });
  assert.equal(raised.maxMembers, 50);
  assert.equal(raised.onlineCount, 3, 'a capacity delta must not disturb the online count');
  assert.equal(raised.topic, 'What is above us', 'a capacity delta must not blank the topic');
});

test('a capacity label reads "here/max", and degrades to the bare head-count when max is unknown', () => {
  assert.equal(capacityLabel(2, 33), '2/33');
  assert.equal(capacityLabel(0, 33), '0/33');
  // An unknown or nonsensical maximum is never shown as a denominator: the count stands alone.
  assert.equal(capacityLabel(2, undefined), '2');
  assert.equal(capacityLabel(2, 0), '2');
  // An unknown online count is zero people here, not a blank.
  assert.equal(capacityLabel(undefined, 33), '0/33');
  assert.equal(capacityLabel(undefined, undefined), '0');
});

test('a room row is titled by the glyph and the room record\u2019s name, with honest fallbacks', () => {
  const summary = {
    conversationId: 'conv_1' as Id,
    kind: ConversationKind.Room,
    encryption: EncryptionMode.None,
    lastSeq: 41,
    readSeq: 41,
  };
  assert.equal(roomRowTitle(summary, roomInfoOf(joined())), '# Observatory');
  // A summary that kept a title (the join flow sets one) is used when no record is held.
  assert.equal(roomRowTitle({ ...summary, title: 'From the join' }, null), '# From the join');
  // A room the shell neither joined nor remembers is still a room — anonymous, but honest.
  assert.equal(roomRowTitle(summary, null), '# Room');
});

test('the remembered rooms persist to IndexedDB under their account, and only there', async () => {
  const info = roomInfoOf(joined());
  await saveRoomInfo('acct_1' as Id, { [info.conversationId]: info });

  const stored = await loadRoomInfo();
  assert.equal(stored?.accountId, 'acct_1' as Id);
  assert.deepEqual(stored?.rooms[info.conversationId], info);

  // A different account's copy is simply not this account's: the store hands it back whole and
  // the provider discards it — the test pins the store's half, that the copy is account-keyed
  // and readable, so the discard decision has something to discard.
  assert.notEqual(stored?.accountId, 'acct_2');

  // The record reached the sanctioned store, and not one write touched a forbidden surface.
  assert.ok(idb.store.size > 0, 'the room record never reached IndexedDB');
  assert.equal(webStorage.writes().length, 0, 'a room record was written to a forbidden surface');

  await clearRoomInfo();
  assert.equal(await loadRoomInfo(), undefined);
});

// departedRoomOf: the one frame a removed member gets to keep. The server publishes the
// removal event and then takes the room's topics away, so the projection below is the whole
// client-side story of audit area 4 — the event naming this account with Kicked, Banned, or
// Left must identify the room to drop (record, watch, sidebar row, open thread), and nothing
// else may: a join or a return keeps the account seated, a disconnect revokes nothing, and a
// departure naming someone else is not this shell's to act on.

const ME = 'acct_me' as Id;
const OTHER = 'acct_other' as Id;
const ROOM_TWO = 'room_2' as Id;
const CONV_TWO = 'conv_2' as Id;

/** The bridge map the provider holds: one watched room, mapped to its conversation. */
const byRoomId = new Map<Id, Id>([
  [ROOM.roomId, 'conv_1' as Id],
  [ROOM_TWO, CONV_TWO],
]);

function memberEvent(change: MemberChange, userId: Id, roomId: Id = ROOM.roomId): RoomMemberEvent {
  return { roomId, userId, change, joined: change === MemberChange.Joined, memberCount: 11 };
}

test('a kick, a ban, and a leave naming this account each name the room to drop', () => {
  for (const change of [MemberChange.Kicked, MemberChange.Banned, MemberChange.Left]) {
    const gone = departedRoomOf(memberEvent(change, ME), ME, byRoomId);
    assert.deepEqual(
      gone,
      { roomId: ROOM.roomId, conversationId: 'conv_1' as Id },
      `${MemberChange[change]} naming self must cost the room its surface`,
    );
  }
});

test('the dropped room names both halves: the room the record is keyed by, the conversation the row is', () => {
  const gone = departedRoomOf(memberEvent(MemberChange.Kicked, ME, ROOM_TWO), ME, byRoomId);
  assert.deepEqual(gone, { roomId: ROOM_TWO, conversationId: CONV_TWO });
});

test('a join, a return, or a disconnect naming this account keeps the room', () => {
  for (const change of [MemberChange.Joined, MemberChange.Reconnected, MemberChange.Disconnected]) {
    assert.equal(
      departedRoomOf(memberEvent(change, ME), ME, byRoomId),
      null,
      `${MemberChange[change]} naming self is not a departure`,
    );
  }
});

test('a departure naming someone else is the room’s business, not this shell’s', () => {
  for (const change of [MemberChange.Kicked, MemberChange.Banned, MemberChange.Left]) {
    assert.equal(departedRoomOf(memberEvent(change, OTHER), ME, byRoomId), null);
  }
});

test('a legacy event with no change is read as no departure, even when its joined flag is false', () => {
  // The legacy flag predates the change enum; a modern server's removals always carry the
  // change, so a changeless frame must not guess a departure off the flag alone.
  const legacy: RoomMemberEvent = {
    roomId: ROOM.roomId,
    userId: ME,
    joined: false,
    memberCount: 11,
  };
  assert.equal(departedRoomOf(legacy, ME, byRoomId), null);
});

test('no account or no held room means nothing to drop', () => {
  assert.equal(departedRoomOf(memberEvent(MemberChange.Kicked, ME), null, byRoomId), null);
  // A room the map does not hold was never watched, so no frame for it can arrive — but the
  // projection still refuses to invent a conversation for one that somehow does.
  assert.equal(
    departedRoomOf(memberEvent(MemberChange.Kicked, ME, 'room_unknown' as Id), ME, byRoomId),
    null,
  );
});
