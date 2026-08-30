/**
 * What the gifts panel is allowed to say about the caller's money and standing.
 *
 * The panel's data all arrives from the economy domain, so its rendering tests feed the pure
 * presentational components exactly what the SDK calls return (the mock is the shape of the
 * wire's answers) and pin the rules that would silently regress under a "helpful" refactor:
 *
 *   1. **The XP bar clamps.** A progression whose `xpForNextLevel` is zero (or a negative a
 *      hostile node sent) must render an empty bar, not `NaN`% width — a broken stylesheet is a
 *      wrong screen, an unfilled bar is an honest one.
 *   2. **A ledger line's sign comes from the reason, never the amount.** The wire's amount is a
 *      magnitude; buying a gift debits and receiving one's reputation credits, and a regression
 *      that read the sign off anything else (or guessed a direction for `adjustment`) would show
 *      money moving the wrong way — invisible to any schema check, because the number is still
 *      there, just wrong.
 *   3. **The send flow never hides the price.** The picker states the gift's coin price beside
 *      the recipient choice, so the spend is agreed before the recipient is.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { Id, LedgerEntryWire, RelationshipEntry, SuggestedUser, UserProfile } from '@migo/sdk';

import {
  BalanceCard,
  GiftGrid,
  LedgerList,
  ProgressionCard,
  RecipientPicker,
  ledgerAmountLabel,
  xpFraction,
} from '../src/components/wallet-panel.js';

const AT = Date.parse('2026-08-26T12:00:00Z');

const PROFILES = new Map<Id, UserProfile>([
  ['ada' as Id, { userId: 'ada' as Id, publicId: 'MGO-ADA', username: 'ada', displayName: 'Ada' }],
  [
    'grace' as Id,
    {
      userId: 'grace' as Id,
      publicId: 'MGO-GRACE',
      username: 'grace',
      displayName: 'Grace',
    },
  ],
]);

function entry(reason: string, amount: number, balanceAfter: number): LedgerEntryWire {
  return { txId: `tx_${reason}` as Id, reason, amount, balanceAfter, at: AT };
}

test('the balance card shows the coin balance and the points balance', () => {
  const markup = renderToStaticMarkup(<BalanceCard balance={{ balance: 120, points: 5 }} />);
  // The coin is $MIG: the number in the amount, the ticker beside it, on the coins fact.
  assert.ok(
    markup.includes('balance-amount">120<') || markup.includes('balance-amount">120</span>'),
    'the coin balance is missing',
  );
  assert.ok(markup.includes('$MIG'), 'the coin fact lost its $MIG unit');
  assert.ok(
    markup.includes('balance-amount">5<') || markup.includes('balance-amount">5</span>'),
    'the points balance is missing',
  );
  assert.ok(markup.includes('points'), 'the points fact lost its unit');
});

test('the gift grid renders every catalogue entry with its price, category, and a send control', () => {
  const markup = renderToStaticMarkup(
    <GiftGrid
      gifts={[
        { sku: 'rose', name: 'Rose', price: 10, category: 'flora' },
        { sku: 'cake', name: 'Cake', price: 75, category: 'food' },
      ]}
      onSend={() => {}}
      disabled={false}
    />,
  );
  for (const expect of ['Rose', 'Cake', '10 coins', '75 coins', 'flora', 'food']) {
    assert.ok(markup.includes(expect), `the catalogue card lost its "${expect}" line`);
  }
  // One Send control per gift, and each is clickable while no flow is in flight.
  assert.equal((markup.match(/>Send</g) ?? []).length, 2);
  assert.ok(!markup.includes('disabled'), 'an idle send control must not be disabled');
});

test('the progression card states the level, a filled bar, and the XP numbers behind it', () => {
  const markup = renderToStaticMarkup(
    <ProgressionCard
      progression={{
        accountId: 'me' as Id,
        xp: 1200,
        level: 3,
        xpIntoLevel: 200,
        xpForNextLevel: 400,
      }}
    />,
  );
  assert.ok(markup.includes('Level 3'), 'the level is missing');
  assert.ok(markup.includes('200 / 400 XP'), 'the into/total label is missing');
  assert.ok(markup.includes('width:50%'), 'a half-filled level must fill half the bar');
  // The bar is a progressbar carrying the real bounds, so assistive technology reads the same
  // fraction the sighted bar shows.
  assert.ok(markup.includes('role="progressbar"'));
  assert.ok(markup.includes('aria-valuemax="400"'));
});

test('the XP fraction clamps into 0–1 instead of producing an unrendable width', () => {
  assert.equal(xpFraction(200, 400), 0.5);
  assert.equal(xpFraction(400, 400), 1);
  // Over-full (a delta a race delivered late) and empty both stay inside the bar.
  assert.equal(xpFraction(450, 400), 1);
  assert.equal(xpFraction(-5, 400), 0);
  // A zero or negative total is not a level: an empty bar, never NaN or Infinity.
  assert.equal(xpFraction(10, 0), 0);
  assert.equal(xpFraction(10, -3), 0);
  const hostile = renderToStaticMarkup(
    <ProgressionCard
      progression={{ accountId: 'me' as Id, xp: 0, level: 1, xpIntoLevel: 0, xpForNextLevel: 0 }}
    />,
  );
  assert.ok(hostile.includes('width:0%'), 'a zero-length level must render an empty bar');
  assert.ok(!hostile.includes('NaN'), 'a zero-length level produced an unrendable width');
});

test('a ledger line signs its amount from the reason, and never guesses for unknown words', () => {
  assert.equal(ledgerAmountLabel(entry('gift_purchase', 10, 90)), '−10');
  assert.equal(ledgerAmountLabel(entry('purchase', 5, 85)), '−5');
  assert.equal(ledgerAmountLabel(entry('game_stake', 20, 65)), '−20');
  assert.equal(ledgerAmountLabel(entry('grant', 50, 150)), '+50');
  assert.equal(ledgerAmountLabel(entry('gift_reputation', 3, 153)), '+3');
  assert.equal(ledgerAmountLabel(entry('refund', 10, 163)), '+10');
  assert.equal(ledgerAmountLabel(entry('game_payout', 40, 203)), '+40');
  // An operator adjustment and a reason a newer node wrote render unsigned: the direction of
  // money a client cannot name is not a direction it may draw.
  assert.equal(ledgerAmountLabel(entry('adjustment', 7, 210)), '7');
  assert.equal(ledgerAmountLabel(entry('from_the_future', 9, 219)), '9');
});

test('the ledger list shows one row per transaction with its reason, amount, and balance after', () => {
  const markup = renderToStaticMarkup(
    <LedgerList entries={[entry('gift_purchase', 10, 90), entry('grant', 50, 140)]} />,
  );
  assert.ok(markup.includes('gift_purchase'), 'the reason is missing');
  assert.ok(markup.includes('balance 90'), 'the balance-after is missing');
  assert.ok(markup.includes('−10'), 'the debit sign is missing');
  assert.ok(markup.includes('+50'), 'the credit sign is missing');
  assert.ok(!markup.includes('<script'), 'a reason must never render as a live element');
});

test('an empty ledger says so rather than rendering a hollow list', () => {
  const markup = renderToStaticMarkup(<LedgerList entries={[]} />);
  assert.ok(markup.includes('No transactions yet.'));
});

const FRIENDS: RelationshipEntry[] = [
  { userId: 'ada' as Id, kind: 1 },
  { userId: 'grace' as Id, kind: 1 },
];

const RESULTS: SuggestedUser[] = [
  { accountId: 'alan' as Id, username: 'alan', displayName: 'Alan Turing', mutualFriends: 0 },
];

test('the recipient picker shows the price, the friends, and the username search for anyone else', () => {
  const markup = renderToStaticMarkup(
    <RecipientPicker
      gift={{ sku: 'rose', name: 'Rose', price: 10, category: 'flora' }}
      friends={FRIENDS}
      results={RESULTS}
      profiles={PROFILES}
      onSearch={() => {}}
      onPick={() => {}}
      onCancel={() => {}}
      busy={false}
    />,
  );
  assert.ok(markup.includes('Send Rose'), 'the picker must name the gift being sent');
  assert.ok(markup.includes('10 coins'), 'the picker must state the price before the recipient');
  // Friends by their display names, each with their own send control.
  assert.ok(markup.includes('Ada'));
  assert.ok(markup.includes('Grace'));
  assert.equal((markup.match(/>Send</g) ?? []).length, 3, 'one send control per candidate');
  // Search results are for non-friends, and render beside the friends rather than replacing them.
  assert.ok(markup.includes('Alan Turing'));
  assert.ok(markup.includes('Search results'));
  assert.ok(
    markup.includes('placeholder="Search by username"'),
    'the non-friend search input is missing',
  );
});

test('a picker with no friends points at the search instead of an empty room', () => {
  const markup = renderToStaticMarkup(
    <RecipientPicker
      gift={{ sku: 'rose', name: 'Rose', price: 10, category: 'flora' }}
      friends={[]}
      results={null}
      profiles={new Map()}
      onSearch={() => {}}
      onPick={() => {}}
      onCancel={() => {}}
      busy={false}
    />,
  );
  assert.ok(
    markup.includes('search for anyone by username'),
    'a friendless picker must point at the search path',
  );
});
