/**
 * The new request/response domains: social, economy, the notifications inbox, and the profile edit.
 *
 * Each test drives a domain through a real {@link Rpc} over the {@link RecordingTransport} double, so
 * both halves of every method are exercised against the generated codecs: what the domain *sent* is
 * decoded back out of the recorded frame body (a mismatched struct would fail to decode or decode
 * wrong), and what the domain *returned* is decoded from a reply the test encoded. The
 * friend-event listener is exercised the same way, through the double's event injection.
 *
 * The ack test deserves a note. `acknowledgeNotifications` takes a Unix-millisecond watermark, but
 * the wire carries a notification *id* whose embedded time prefix the server reads as the watermark.
 * The test asserts the synthesised id really does carry `through` in its prefix — an id that silently
 * dropped the timestamp would ack nothing, and the bell would never clear.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { idUnixMs } from '@migo/wire';
import {
  decodeBody,
  encodeBody,
  EconomyDomain,
  NotificationsDomain,
  ProfileDomain,
  Rpc,
  SocialDomain,
} from '../src/index.js';
import { OP } from '@migo/protocol';
import {
  decodeFriendRespond,
  decodeFriendTarget,
  decodeGiftSend,
  decodeInboxReq,
  decodeLeaderboardReq,
  decodeLedgerReq,
  decodeNotificationAck,
  decodeProfileUpdate,
  decodeRelationshipListReq,
  decodeSearchReq,
  encodeAcknowledged,
  encodeFriendEvent,
  encodeGiftCatalogueResponse,
  encodeGiftSendResult,
  encodeInboxResponse,
  encodeLeaderboardResponse,
  encodeLedgerResponse,
  encodeProfileResponse,
  encodeProgressionWire,
  encodeBadgesResponse,
  encodeRelationshipList,
  encodeSearchResponse,
  encodeUserProfile,
  encodeWalletView,
} from '@migo/protocol';
import type { InboxItem, UserProfile } from '@migo/protocol';

import { RecordingTransport, idOf } from './harness.js';

/** Builds a domain triple over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
  transport: RecordingTransport;
  social: SocialDomain;
  notifications: NotificationsDomain;
  economy: EconomyDomain;
  profile: ProfileDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return {
    transport,
    social: new SocialDomain(rpc),
    notifications: new NotificationsDomain(rpc),
    economy: new EconomyDomain(rpc),
    profile: new ProfileDomain(rpc),
  };
}

/**
 * The frame recorded at `index`, narrowed to present.
 *
 * Every call site has already awaited the request that produced it, so an absent frame is a bug in
 * the test itself; asserting it here keeps the decode call sites free of both `!` and `?.` (either
 * of which fights one of the two checkers this suite runs under).
 */
function sentAt(
  transport: RecordingTransport,
  index: number,
): { opcode: number; body: Uint8Array } {
  const frame = transport.sent[index];
  assert.ok(frame !== undefined, `expected a recorded frame at index ${index}`);
  return frame;
}

const USER = idOf(7);
const OTHER = idOf(9);
/** A Unix-ms instant after the Migo epoch (2024-01-01), so timestamps round-trip through the codec. */
const AT = 1_767_225_600_000;

test('social: friendRequest sends FRIEND_REQUEST addressed to the target', async () => {
  const { transport, social } = rig(
    new Map([[OP.FRIEND_REQUEST, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await social.friendRequest(OTHER);
  assert.equal(transport.sent.length, 1);
  assert.equal(transport.sent[0]?.opcode, OP.FRIEND_REQUEST);
  assert.deepEqual(decodeBody(decodeFriendTarget, sentAt(transport, 0).body), { userId: OTHER });
});

test('social: friendRespond carries the accept flag', async () => {
  const { transport, social } = rig(
    new Map([[OP.FRIEND_RESPOND, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await social.friendRespond(OTHER, false);
  assert.equal(transport.sent[0]?.opcode, OP.FRIEND_RESPOND);
  assert.deepEqual(decodeBody(decodeFriendRespond, sentAt(transport, 0).body), {
    userId: OTHER,
    accept: false,
  });
});

test('social: blockUser sends BLOCK_SET with a FriendTarget body', async () => {
  const { transport, social } = rig(
    new Map([[OP.BLOCK_SET, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await social.blockUser(OTHER);
  assert.equal(transport.sent[0]?.opcode, OP.BLOCK_SET);
  assert.deepEqual(decodeBody(decodeFriendTarget, sentAt(transport, 0).body), { userId: OTHER });
});

test('social: listRelationships decodes the entry list', async () => {
  const { transport, social } = rig(
    new Map([
      [
        OP.RELATIONSHIP_LIST,
        () =>
          encodeBody(encodeRelationshipList, {
            entries: [
              { userId: OTHER, kind: 1 },
              { userId: idOf(10), kind: 3 },
            ],
          }),
      ],
    ]),
  );
  const entries = await social.listRelationships();
  assert.equal(transport.sent[0]?.opcode, OP.RELATIONSHIP_LIST);
  assert.deepEqual(entries, [
    { userId: OTHER, kind: 1 },
    { userId: idOf(10), kind: 3 },
  ]);
});

test('social: suggestions and search both decode a SearchResponse', async () => {
  const suggested = { accountId: OTHER, username: 'ada', displayName: 'Ada', mutualFriends: 2 };
  const response = () => encodeBody(encodeSearchResponse, { results: [suggested] });
  const { transport, social } = rig(
    new Map([
      [OP.SUGGESTIONS, response],
      [OP.SEARCH, response],
    ]),
  );

  const suggestions = await social.suggestions();
  assert.deepEqual(suggestions, [suggested]);

  const results = await social.search('ad', 5);
  assert.deepEqual(results, [suggested]);
  // The search request carries the query and the limit; a missing limit must stay absent so the
  // server applies its own default rather than a client-guessed zero.
  const request = decodeBody(decodeSearchReq, sentAt(transport, 1).body);
  assert.deepEqual(request, { query: 'ad', limit: 5 });
});

test('social: listAllRelationships asks for the server page and returns every kind', async () => {
  const { transport, social } = rig(
    new Map([
      [
        OP.RELATIONSHIP_LIST,
        () =>
          encodeBody(encodeRelationshipList, {
            entries: [
              { userId: OTHER, kind: 1 },
              { userId: idOf(10), kind: 4 },
              { userId: idOf(11), kind: 5 },
              { userId: idOf(12), kind: 6 },
            ],
          }),
      ],
    ]),
  );
  const entries = await social.listAllRelationships();

  // Zero is the wire's "no client bound": the server applies its own page, so every kind the
  // graph holds — friends, follows, blocks, favourites — comes back mixed for the caller to
  // filter, rather than truncated to a client-guessed ceiling.
  assert.deepEqual(decodeBody(decodeRelationshipListReq, sentAt(transport, 0).body), { limit: 0 });
  assert.deepEqual(entries, [
    { userId: OTHER, kind: 1 },
    { userId: idOf(10), kind: 4 },
    { userId: idOf(11), kind: 5 },
    { userId: idOf(12), kind: 6 },
  ]);
});

test('social: onFriendEvent delivers decoded events once started, and stops cleanly', () => {
  const { transport, social } = rig(new Map());
  const seen: Array<{ userId: string; state: string }> = [];
  const off = social.onFriendEvent((event) => seen.push(event));

  social.start();
  transport.emit(
    OP.FRIEND_EVENT,
    encodeBody(encodeFriendEvent, { userId: OTHER, state: 'accepted' }),
  );
  assert.deepEqual(seen, [{ userId: OTHER, state: 'accepted' }]);

  social.stop();
  transport.emit(
    OP.FRIEND_EVENT,
    encodeBody(encodeFriendEvent, { userId: OTHER, state: 'request' }),
  );
  assert.equal(seen.length, 1, 'an event after stop() must not be delivered');
  off();
});

test('notifications: listNotifications sends the limit and decodes the items', async () => {
  const items: InboxItem[] = [
    {
      id: idOf(1),
      kind: 'friend_request',
      at: AT,
      actorId: OTHER,
    },
  ];
  const { transport, notifications } = rig(
    new Map([[OP.NOTIFICATION_LIST, () => encodeBody(encodeInboxResponse, { items })]]),
  );
  const listed = await notifications.listNotifications(20);
  assert.deepEqual(listed, items);
  assert.deepEqual(decodeBody(decodeInboxReq, sentAt(transport, 0).body), { limit: 20 });
});

test('notifications: acknowledgeNotifications carries the watermark in the id time prefix', async () => {
  const through = 1_735_689_600_000; // 2025-01-01T00:00:00Z
  const { transport, notifications } = rig(
    new Map([[OP.NOTIFICATION_ACK, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await notifications.acknowledgeNotifications(through);
  const request = decodeBody(decodeNotificationAck, sentAt(transport, 0).body);
  assert.equal(idUnixMs(request.id), through);
});

test('economy: getBalance and getGiftCatalogue decode their responses', async () => {
  const { economy } = rig(
    new Map([
      [OP.BALANCE_FETCH, () => encodeBody(encodeWalletView, { balance: 100, points: 5 })],
      [
        OP.GIFT_CATALOGUE,
        () =>
          encodeBody(encodeGiftCatalogueResponse, {
            gifts: [{ sku: 'rose', name: 'Rose', price: 10, category: 'flora' }],
          }),
      ],
    ]),
  );
  assert.deepEqual(await economy.getBalance(), { balance: 100, points: 5 });
  assert.deepEqual(await economy.getGiftCatalogue(), [
    { sku: 'rose', name: 'Rose', price: 10, category: 'flora' },
  ]);
});

test('economy: sendGift encodes sku, recipient, and the optional conversation', async () => {
  const conversation = idOf(21);
  const { transport, economy } = rig(
    new Map([[OP.GIFT_SEND, () => encodeBody(encodeGiftSendResult, { ok: true })]]),
  );
  const result = await economy.sendGift('rose', OTHER, conversation);
  assert.deepEqual(result, { ok: true });
  assert.deepEqual(decodeBody(decodeGiftSend, sentAt(transport, 0).body), {
    gift: 'rose',
    recipient: OTHER,
    conversationId: conversation,
  });
});

test('economy: getLedger decodes entries and forwards the limit', async () => {
  const { transport, economy } = rig(
    new Map([
      [
        OP.LEDGER_HISTORY,
        () =>
          encodeBody(encodeLedgerResponse, {
            entries: [
              {
                txId: idOf(2),
                reason: 'gift_sent',
                amount: 10,
                balanceAfter: 90,
                at: AT,
              },
            ],
          }),
      ],
    ]),
  );
  const entries = await economy.getLedger(5);
  assert.equal(entries.length, 1);
  assert.equal(entries[0]?.reason, 'gift_sent');
  assert.deepEqual(decodeBody(decodeLedgerReq, sentAt(transport, 0).body), { limit: 5 });
});

test('economy: getProgression and getBadges answer for any account', async () => {
  const { economy } = rig(
    new Map([
      [
        OP.PROGRESSION,
        () =>
          encodeBody(encodeProgressionWire, {
            accountId: OTHER,
            xp: 1200,
            level: 3,
            xpIntoLevel: 200,
            xpForNextLevel: 400,
          }),
      ],
      [
        OP.BADGES,
        () => encodeBody(encodeBadgesResponse, { badges: [{ badgeCode: 'early', awardedAt: AT }] }),
      ],
    ]),
  );
  assert.deepEqual(await economy.getProgression(OTHER), {
    accountId: OTHER,
    xp: 1200,
    level: 3,
    xpIntoLevel: 200,
    xpForNextLevel: 400,
  });
  assert.deepEqual(await economy.getBadges(OTHER), [{ badgeCode: 'early', awardedAt: AT }]);
});

test('economy: getLeaderboard reads a board page, strongest first', async () => {
  const ranks = [
    { position: 1, accountId: OTHER, xp: 9000, level: 9 },
    { position: 2, accountId: idOf(11), xp: 8000, level: 8 },
  ];
  const { transport, economy } = rig(
    new Map([[OP.LEADERBOARD, () => encodeBody(encodeLeaderboardResponse, { ranks })]]),
  );
  const read = await economy.getLeaderboard('xp', 10);

  assert.equal(sentAt(transport, 0).opcode, OP.LEADERBOARD);
  assert.deepEqual(decodeBody(decodeLeaderboardReq, sentAt(transport, 0).body), {
    board: 'xp',
    limit: 10,
  });
  assert.deepEqual(read, ranks);
});

test('economy: getLeaderboard without a limit leaves it off the wire', async () => {
  const { transport, economy } = rig(
    new Map([[OP.LEADERBOARD, () => encodeBody(encodeLeaderboardResponse, { ranks: [] })]]),
  );
  await economy.getLeaderboard('reputation');
  // The limit encodes by presence, so an omitted one must stay absent: the server's own page —
  // not a client-guessed zero — is what bounds the read.
  assert.deepEqual(decodeBody(decodeLeaderboardReq, sentAt(transport, 0).body), {
    board: 'reputation',
  });
});

test('profile: updateProfile sends only the fields the patch carries', async () => {
  const updated: UserProfile = {
    userId: USER,
    publicId: 'MGO-TEST',
    username: 'ada',
    displayName: 'Ada Lovelace',
  };
  const { transport, profile } = rig(
    new Map([[OP.PROFILE_UPDATE, () => encodeBody(encodeUserProfile, updated)]]),
  );
  const reply = await profile.updateProfile({ displayName: 'Ada Lovelace', showLastSeen: 0 });
  assert.deepEqual(reply, updated);
  // Only the display name and the privacy field are on the wire; every other field stays absent so
  // the server keeps its current value for it.
  const request = decodeBody(decodeProfileUpdate, sentAt(transport, 0).body);
  assert.deepEqual(request, { displayName: 'Ada Lovelace', showLastSeen: 0 });
});

test('profile: fetch still batch-fetches after the extension', async () => {
  const { profile } = rig(
    new Map([[OP.PROFILE_FETCH, () => encodeBody(encodeProfileResponse, { profiles: [] })]]),
  );
  assert.deepEqual(await profile.fetch([OTHER]), []);
});
