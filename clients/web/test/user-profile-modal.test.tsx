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

import { PresenceState, RelationshipKind } from '@migo/sdk';
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

test('the enriched card carries presence, status, language, rank, and the level bar', () => {
  const markup = renderToStaticMarkup(
    <UserProfileCard
      profile={{
        ...ADA,
        language: 'en',
        customStatus: 'Counting engines',
        verified: true,
      }}
      progression={{
        accountId: ADA.userId,
        xp: 1200,
        level: 3,
        xpIntoLevel: 200,
        xpForNextLevel: 400,
      }}
      rank={4}
      relationship={RelationshipKind.Friend}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
    />,
  );

  assert.ok(markup.includes('Online'), 'the presence line is missing');
  assert.ok(markup.includes('Counting engines'), 'the custom status is missing');
  assert.ok(markup.includes('🗣 en'), 'the language fact is missing');
  assert.ok(markup.includes('profile-verified'), 'the verified mark is missing');
  assert.ok(markup.includes('🏆 #4 on the XP board'), 'the rank fact is missing');
  assert.ok(markup.includes('200 / 400 XP to level 4'), 'the level bar note is missing');
  assert.ok(markup.includes('profile-progress-fill'), 'the level bar track is missing');
  assert.ok(markup.includes('✓ Friends'), 'the friend line is missing');
  assert.ok(
    markup.includes('aria-label="Copy MGO-ADA42"'),
    'the copy-id control lost its accessible name',
  );
  assert.ok(!markup.includes('Add friend'), 'a friend was offered an add-friend control');
});

test('the social line offers the one act each relationship state admits', () => {
  const incoming = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      relationship={RelationshipKind.PendingIncoming}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
      onFriendRespond={() => {}}
    />,
  );
  assert.ok(incoming.includes('>Accept<'), 'a pending incoming request lost its accept control');
  assert.ok(incoming.includes('>Decline<'), 'a pending incoming request lost its decline control');
  assert.ok(!incoming.includes('Add friend'), 'a pending request was offered an add-friend');

  const outgoing = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      relationship={RelationshipKind.PendingOutgoing}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
    />,
  );
  assert.ok(outgoing.includes('Request sent'), 'a pending outgoing request lost its line');
  assert.ok(!outgoing.includes('Add friend'), 'a sent request was re-offered');

  const stranger = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      relationship={RelationshipKind.Follow}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
      onFriendRequest={() => {}}
    />,
  );
  assert.ok(stranger.includes('>Add friend<'), 'a no-relationship state lost its add control');
  assert.ok(!stranger.includes('✓ Friends'), 'a non-friend was marked a friend');

  // An unknown relationship renders no social line at all — honest, not presumptuous.
  const unknown = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
      onFriendRequest={() => {}}
    />,
  );
  assert.ok(!unknown.includes('Add friend'), 'an unknown relationship invented a social line');
  assert.ok(!unknown.includes('✓ Friends'), 'an unknown relationship invented a friendship');
});

test('a rankless person carries no board line, and the gift act appears only when offered', () => {
  const bare = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
    />,
  );
  assert.ok(!bare.includes('XP board'), 'an unread board leaked a rank line');

  const giftable = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
      onGift={() => {}}
    />,
  );
  assert.ok(giftable.includes('>Gift<'), 'the gift action is missing where it is offered');
  assert.ok(!bare.includes('>Gift<'), 'a gift action appeared where none was offered');
});

test('reporting a person is offered only where a dialog can host it, and never as a toggle', () => {
  const bare = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
    />,
  );
  assert.ok(!bare.includes('Report'), 'a report control appeared where no dialog could open');

  const reportable = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      blocked={false}
      canMessage
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
      onReport={() => {}}
    />,
  );
  assert.ok(
    reportable.includes('aria-label="Report Ada Lovelace"'),
    'the report control lost its accessible name',
  );
  // Unlike the block control there is no busy state and no "Reported" — the reporter is never told
  // the outcome, so a state the card could show would be a state the protocol does not have.
  assert.ok(
    !reportable.includes('disabled'),
    'the report control must stay offered: filing twice is idempotent, not a spent act',
  );
  // Reporting and blocking are different acts on different authorities, so both stay available on
  // the same person — the block is the viewer's own, the report is a moderator's to weigh.
  const both = renderToStaticMarkup(
    <UserProfileCard
      profile={ADA}
      blocked
      canMessage={false}
      busy={false}
      onMessage={() => {}}
      onBlock={() => {}}
      onReport={() => {}}
    />,
  );
  assert.ok(both.includes('aria-label="Report Ada Lovelace"'), 'a blocked person lost reporting');
  assert.ok(both.includes('>Blocked</button>'), 'the block control lost its state beside it');
});
