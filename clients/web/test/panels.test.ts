/**
 * What the section panels are allowed to send and claim.
 *
 * Two rules carry privacy or correctness weight and would silently regress under a "helpful"
 * refactor, so they are pinned here as pure functions:
 *
 *   1. **A profile edit must not re-state privacy.** `buildProfilePatch` is the whole save surface:
 *      only fields that visibly moved join the patch, and a privacy select joins it only when an
 *      audience was explicitly chosen. A regression that pre-filled the selects with a default, or
 *      that sent the whole form object, would rewrite `who_can_message` for a user who only came to
 *      fix their display name — invisible to any rendering test, because the display name would
 *      still look right.
 *   2. **A freshly joined room has no unread history.** `joinedRoomSummary` sets `readSeq` to the
 *      join handle's `lastSeq`. A projection that left `readSeq` at zero would badge every joined
 *      room as unread and send the user hunting for messages they have not missed.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ConversationKind, EncryptionMode } from '@migo/sdk';
import type { Id, RoomJoinResponse, RoomSummary, UserProfile } from '@migo/sdk';

import { buildProfilePatch } from '../src/components/profile-panel.js';
import { joinedRoomSummary } from '../src/components/discover-panel.js';

const PROFILE: UserProfile = {
  userId: 'user_1' as Id,
  publicId: 'MGO-TEST',
  username: 'ada',
  displayName: 'Ada',
};

const UNCHANGED = {
  showLastSeen: '',
  whoCanMessage: '',
  whoCanAdd: '',
};

test('a profile patch with nothing moved is empty, so no request is sent at all', () => {
  const patch = buildProfilePatch(PROFILE, { displayName: 'Ada', bio: '' }, UNCHANGED);
  assert.deepEqual(patch, {});
});

test('editing only the display name sends only the display name', () => {
  const patch = buildProfilePatch(PROFILE, { displayName: 'Ada Lovelace  ', bio: '' }, UNCHANGED);
  // The name is trimmed; the privacy fields the user never saw stay absent from the patch.
  assert.deepEqual(patch, { displayName: 'Ada Lovelace' });
});

test('an explicitly chosen privacy audience joins the patch as its numeric value', () => {
  const patch = buildProfilePatch(
    PROFILE,
    { displayName: 'Ada', bio: '' },
    {
      ...UNCHANGED,
      showLastSeen: '0',
      whoCanAdd: '1',
    },
  );
  assert.deepEqual(patch, { showLastSeen: 0, whoCanAdd: 1 });
});

test('a bio is sent when it changed and the stored profile had none', () => {
  const withBio = { ...PROFILE, bio: 'Analyst' };
  assert.deepEqual(
    buildProfilePatch(withBio, { displayName: 'Ada', bio: 'Analyst' }, UNCHANGED),
    {},
  );
  assert.deepEqual(
    buildProfilePatch(withBio, { displayName: 'Ada', bio: 'Analyst (retired)' }, UNCHANGED),
    { bio: 'Analyst (retired)' },
  );
});

test('a joined room projects to a Room conversation read at its tip', () => {
  const room: RoomSummary = {
    roomId: 'room_1' as Id,
    publicId: 'MGO-ROOM',
    kind: 1,
    name: 'Observatory',
    memberCount: 12,
    onlineCount: 3,
  };
  const joined: RoomJoinResponse = {
    room,
    conversationId: 'conv_1' as Id,
    encryption: EncryptionMode.None,
    lastSeq: 41,
  };
  const summary = joinedRoomSummary(joined);
  assert.equal(summary.conversationId, joined.conversationId);
  assert.equal(summary.kind, ConversationKind.Room);
  assert.equal(summary.title, 'Observatory');
  // The anti-phantom-unread rule: the room is joined at its tip.
  assert.equal(summary.readSeq, joined.lastSeq);
  assert.equal(summary.lastSeq, joined.lastSeq);
  // No avatar on the room means no avatar key at all, not an undefined-valued one.
  assert.ok(!('avatarUrl' in summary));
});

test('a room with an avatar keeps it in the projection', () => {
  const joined: RoomJoinResponse = {
    room: {
      roomId: 'room_2' as Id,
      publicId: 'MGO-ROOM2',
      kind: 1,
      name: 'Greenhouse',
      memberCount: 2,
      onlineCount: 1,
      avatarUrl: 'https://cdn.example/avatar.png',
    },
    conversationId: 'conv_2' as Id,
    encryption: EncryptionMode.None,
    lastSeq: 0,
  };
  assert.equal(joinedRoomSummary(joined).avatarUrl, 'https://cdn.example/avatar.png');
});
