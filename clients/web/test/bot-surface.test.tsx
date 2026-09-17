/**
 * Where a bot becomes visible, and the one thing that has to follow from it.
 *
 * Brief section 49's last open sentence is that a bot could be reported from the SDK and never
 * seen: a bot holds an ordinary account row and an ordinary profile, so every listing in this
 * client drew one as a person. The wire now names the bot behind such an account — `bot_id` on a
 * profile card and on a search result — and these are the rules that make the name worth carrying:
 *
 *   1. **A row says so, and only when there is something to say.** The mark is drawn where the
 *      wire named a bot and nowhere else; a mark that were always present is a mark nobody reads.
 *   2. **A report about a bot names the bot.** `bot.bot_id` is a different id from the account id
 *      and is the one a moderator's bot actions act on, so a report filed under the account id
 *      would reach the queue as a report about a bot naming something that is not one. One
 *      builder decides, so no row has to know.
 *   3. **The reason starts where the reporter's knowledge ends.** A bot report opens on bot abuse
 *      rather than on the menu's first code, because the surface already knows what the reporter
 *      would otherwise have to work out — and the reporter can still change it.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

import { RelationshipKind, ReportReason, ReportSubject } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { FriendsSection } from '../src/components/friends-panel.js';
import { personSubject } from '../src/components/report-dialog.js';
import { UserProfileCard } from '../src/components/user-profile-modal.js';

const ADA = {
  userId: 'user_ada' as Id,
  publicId: 'MGO-ADA42',
  username: 'ada',
  displayName: 'Ada Lovelace',
  avatarUrl: undefined,
};

const WEATHER_BOT = 'bot_weather' as Id;

const WEATHER = {
  ...ADA,
  userId: 'user_weather' as Id,
  publicId: 'MGO-WX42',
  username: 'weather',
  displayName: 'Weather',
  botId: WEATHER_BOT,
};

test('a profile card marks a bot and leaves a person unmarked', () => {
  const bot = renderToStaticMarkup(
    createElement(UserProfileCard, {
      profile: WEATHER,
      blocked: false,
      canMessage: true,
      busy: false,
    }),
  );
  assert.match(bot, /bot-badge/, 'a bot’s name carries the mark');
  assert.match(bot, /Bot</, 'and says what it is');

  const person = renderToStaticMarkup(
    createElement(UserProfileCard, {
      profile: ADA,
      blocked: false,
      canMessage: true,
      busy: false,
    }),
  );
  assert.doesNotMatch(person, /bot-badge/, 'and a person’s carries nothing at all');
});

test('a friends list marks the bot in it', () => {
  const markup = renderToStaticMarkup(
    createElement(FriendsSection, {
      entries: [
        { userId: ADA.userId, kind: RelationshipKind.Friend },
        { userId: WEATHER.userId, kind: RelationshipKind.Friend },
      ],
      profiles: new Map<Id, { displayName: string; username?: string; botId?: Id }>([
        [ADA.userId, { displayName: ADA.displayName, username: ADA.username }],
        [
          WEATHER.userId,
          {
            displayName: WEATHER.displayName,
            username: WEATHER.username,
            botId: WEATHER_BOT,
          },
        ],
      ]),
      onSelect: () => {},
      onMessage: () => {},
      onRemove: () => {},
    }),
  );
  assert.equal(
    markup.split('class="bot-badge').length - 1,
    1,
    'exactly one row of the two is a bot',
  );
});

test('a report about a bot names the bot and not the account', () => {
  const subject = personSubject({
    userId: WEATHER.userId,
    displayName: WEATHER.displayName,
    botId: WEATHER_BOT,
  });
  assert.equal(subject.kind, ReportSubject.Bot);
  assert.equal(subject.id, WEATHER_BOT, 'the bot id, which is what the node’s bot actions take');
  assert.notEqual(subject.id, WEATHER.userId);
  assert.equal(subject.label, WEATHER.displayName);
  assert.equal(
    subject.reason,
    ReportReason.BotAbuse,
    'the picker opens on the code the surface already knows',
  );
});

test('a report about a person names the account and seeds no reason', () => {
  const subject = personSubject({ userId: ADA.userId, displayName: ADA.displayName });
  assert.equal(subject.kind, ReportSubject.User);
  assert.equal(subject.id, ADA.userId);
  assert.equal(subject.reason, undefined, 'the dialog’s own default decides, not this builder');
});
