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
 *      still look right. The custom status rides the same rule and the same wire, so it is pinned
 *      here too: it joins the patch only when the box moved, and clearing it sends the empty string
 *      the server reads as "erase" rather than dropping the field, which would mean "keep".
 *   2. **A freshly joined room has no unread history.** `joinedRoomSummary` sets `readSeq` to the
 *      join handle's `lastSeq`. A projection that left `readSeq` at zero would badge every joined
 *      room as unread and send the user hunting for messages they have not missed.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ConversationKind, EncryptionMode } from '@migo/sdk';
import type { Id, RoomJoinResponse, RoomSummary, UserProfile } from '@migo/sdk';

import { buildProfilePatch } from '../src/components/profile-panel.js';
import { joinedRoomSummary } from '../src/lib/migo/use-join-room.js';

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
  whoCanCallVoice: '',
  whoCanCallVideo: '',
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

test('the two call audiences are decided separately and only when chosen', () => {
  // Section 180's split, pinned where it can regress silently: one control per kind, so
  // video set to Nobody joins the patch while the voice line the user never touched stays
  // out of it — and a patch that named neither leaves both where they are.
  assert.deepEqual(
    buildProfilePatch(
      PROFILE,
      { displayName: 'Ada', bio: '' },
      {
        ...UNCHANGED,
        whoCanCallVideo: '0',
      },
    ),
    { whoCanCallVideo: 0 },
  );
  assert.deepEqual(
    buildProfilePatch(
      PROFILE,
      { displayName: 'Ada', bio: '' },
      {
        ...UNCHANGED,
        whoCanCallVoice: '2',
        whoCanCallVideo: '0',
      },
    ),
    { whoCanCallVoice: 2, whoCanCallVideo: 0 },
  );
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

test('a birth year is sent only when it differs from the one the profile carries', () => {
  // The wire echoes the year back now, so "same year" is knowable and must not be
  // re-sent — the same leave-what-you-never-touched rule as the text fields.
  const withYear = { ...PROFILE, birthYear: 1990 };
  assert.deepEqual(
    buildProfilePatch(withYear, { displayName: 'Ada', bio: '' }, UNCHANGED, {
      birthYear: '1990',
    }),
    {},
  );
  assert.deepEqual(
    buildProfilePatch(withYear, { displayName: 'Ada', bio: '' }, UNCHANGED, {
      birthYear: '1991',
    }),
    { birthYear: 1991 },
  );
  // A draft that names no plausible year is a typo, not a change to send.
  assert.deepEqual(
    buildProfilePatch(PROFILE, { displayName: 'Ada', bio: '' }, UNCHANGED, {
      birthYear: 'carbuncle',
    }),
    {},
  );
});

test('a custom status joins the patch only when the box moved, on the profile wire', () => {
  // The status is a profile column now, not a presence publish: the panel saves it in the same
  // patch as everything else, and the profile the panel loaded carries the current value, so a
  // save that only changed the display name must not re-state it. The presence wire would refuse
  // the field outright, so a patch that omitted it would be a save that silently did nothing.
  const withStatus = { ...PROFILE, customStatus: 'in a meeting' };
  assert.deepEqual(
    buildProfilePatch(withStatus, { displayName: 'Ada', bio: '' }, UNCHANGED, {
      customStatus: 'in a meeting',
    }),
    {},
  );
  // Surrounding whitespace is the same status, trimmed the way the server stores it.
  assert.deepEqual(
    buildProfilePatch(withStatus, { displayName: 'Ada', bio: '' }, UNCHANGED, {
      customStatus: '  in a meeting  ',
    }),
    {},
  );
  assert.deepEqual(
    buildProfilePatch(withStatus, { displayName: 'Ada', bio: '' }, UNCHANGED, {
      customStatus: 'heading out',
    }),
    { customStatus: 'heading out' },
  );
  // An absent option and an absent field are both "leave it alone".
  assert.deepEqual(
    buildProfilePatch(withStatus, { displayName: 'Ada', bio: '' }, UNCHANGED, {}),
    {},
  );
});

test('emptying the status box is an edit that clears it, not a no-op', () => {
  // The wire reads an absent field as "keep", so a clear has to send the empty string. A patch
  // builder that treated empty as "nothing to say" would leave the old status on the server and
  // the box would refill on the next load.
  const withStatus = { ...PROFILE, customStatus: 'in a meeting' };
  assert.deepEqual(
    buildProfilePatch(withStatus, { displayName: 'Ada', bio: '' }, UNCHANGED, { customStatus: '' }),
    { customStatus: '' },
  );
  // And clearing a status that was already empty changes nothing.
  assert.deepEqual(
    buildProfilePatch(PROFILE, { displayName: 'Ada', bio: '' }, UNCHANGED, { customStatus: '  ' }),
    {},
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
