/**
 * What another person's profile card is allowed to show and do.
 *
 * The card is the modal's whole body, fed by the wire's profile plus the economy's standing
 * facts. Three rules carry correctness weight and would silently regress under a "helpful"
 * refactor:
 *
 *   1. **A blocked person is not messageable.** The Send Message control must vanish (not merely
 *      misfire) when the viewer blocks the person, and the block control must render its
 *      "Blocked" state disabled — the wire has no unblock, so a clickable "Blocked" would be a
 *      promise the protocol does not keep.
 *   2. **Standing facts degrade, they never break the card.** Level, XP, and badges arrive from
 *      a different service than the profile; when they are absent the card still renders the
 *      person, with the standing lines simply missing.
 *   3. **Every wire field the card shows is the person's own public fact.** Name, @username,
 *      bio, country, public id — and never the viewer's.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { PresenceState } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { UserProfileCard } from '../src/components/user-profile-modal.js';

const ADA = {
  userId: 'user_ada' as Id,
  publicId: 'MGO-ADA42',
  username: 'ada',
  displayName: 'Ada Lovelace',
  bio: 'Analyst of engines.',
  country: 'GB',
  presence: PresenceState.Online,
  avatarUrl: undefined,
};

test('the card shows the profile\u2019s public facts and its standing', () => {
  const markup = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      progression={{
        accountId: ADA.userId,
        xp: 1200,
        level: 3,
        xpIntoLevel: 200,
        xpForNextLevel: 400,
      }}
      badges={[
        { badgeCode: 'early_adopter', awardedAt: 0 },
        { badgeCode: 'gifted', awardedAt: 1 },
      ]}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
    />,
  );

  for (const expect of ['Ada Lovelace', '@ada', 'Analyst of engines.', '🌍 GB', 'MGO-ADA42']) {
    assert.ok(markup.includes(expect), `the card lost its "${expect}" fact`);
  }
  assert.ok(markup.includes('Level 3'), 'the level line is missing');
  assert.ok(markup.includes('⭐ 1200 XP'), 'the XP line is missing');
  assert.ok(markup.includes('early_adopter'), 'a badge is missing');
  assert.ok(markup.includes('gifted'), 'a badge is missing');
  // Both actions offered on an unblocked person.
  assert.ok(markup.includes('>Send Message<'), 'the message action is missing');
  assert.ok(markup.includes('>Block</button>'), 'the block action is missing');
  assert.ok(!markup.includes('disabled'), 'an idle card must not disable its actions');
});

test('a blocked person is not messageable, and the block control states its finality', () => {
  const markup = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      blocked
      canMessage={false}
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
    />,
  );

  assert.ok(!markup.includes('>Send Message<'), 'a blocked person was offered a message action');
  assert.ok(markup.includes('>Blocked</button>'), 'the block control lost its set state');
  assert.ok(
    markup.includes('disabled'),
    'a set block must not look clickable — the wire has no unblock',
  );
});

test('missing standing facts degrade to their absence, not to a broken card', () => {
  const plain = renderToStaticMarkup(
    <UserProfileCard
      profile={{ ...ADA, bio: undefined, country: undefined }}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
    />,
  );

  assert.ok(plain.includes('Ada Lovelace'), 'the person is missing from their own card');
  assert.ok(!plain.includes('Level'), 'a missing progression leaked a level line');
  assert.ok(!plain.includes('XP'), 'a missing progression leaked an XP line');
  assert.ok(!plain.includes('badge-chip'), 'missing badges leaked a badge row');
  assert.ok(!plain.includes('Analyst of engines.'), 'an absent bio was invented');
  assert.ok(!plain.includes('🌍'), 'an absent country was invented');
});
