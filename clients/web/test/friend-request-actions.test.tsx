/**
 * What a friend request is allowed to cost the surface it lands on.
 *
 * A pending friend request is the one notification whose answer the wire carries, and on the
 * phone it is also the one friendship fact that used to be unreachable: the Friends view had no
 * requests section, so Accept and Decline existed only on the desktop's panel. These tests pin
 * the two surfaces that now answer a request where the person reads it:
 *
 *   1. **The phone's Friends view.** The requests section renders an incoming request with
 *      Accept and Decline, an outgoing one as the sent fact it is (there is no unsend opcode to
 *      offer), and nothing at all when the graph holds no pending requests — an empty graph owes
 *      no heading.
 *   2. **The bell.** A friend request row carries its answer when the caller (who has read the
 *      graph) supplies it, and carries none otherwise — an already-answered request, and every
 *      other kind, stays inert. The busy state disables the pair together, because they share
 *      one wire call.
 *   3. **The home header's search ink.** The phone's people search is the same reveal the
 *      Friends panel's head makes, wearing the teal band's own button ink — not the panel's,
 *      which the band would swallow.
 *
 * `renderToStaticMarkup` runs no effects and fires no events, so the wiring inside the panels
 * (the respond call, the graph re-read) is not this suite's to drive; what it can pin is that
 * each surface offers exactly the controls its half of the bargain needs.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { RelationshipKind } from '@migo/sdk';
import type { Id, InboxItem, RelationshipEntry } from '@migo/sdk';

import { FriendsSearch } from '../src/components/friends-panel.js';
import { FriendRequestsSection } from '../src/components/mobile-home.js';
import { FriendRequestActions, NotificationRow } from '../src/components/notifications-panel.js';

const NOOP = () => {};

/** One relationship row, as the graph read would carry it. */
function entry(userId: string, kind: RelationshipKind): RelationshipEntry {
  return { userId: userId as Id, kind };
}

/** The phone's requests section over a tiny graph: two incoming, one outgoing, named. */
function phoneRequests(): string {
  const profiles = new Map<Id, { displayName: string; username?: string }>([
    ['acct_ada' as Id, { displayName: 'Ada Lovelace', username: 'ada' }],
    ['acct_budi' as Id, { displayName: 'Budi', username: 'budi' }],
    ['acct_cita' as Id, { displayName: 'Cita', username: 'cita' }],
  ]);
  return renderToStaticMarkup(
    <FriendRequestsSection
      incoming={[
        entry('acct_ada', RelationshipKind.PendingIncoming),
        entry('acct_budi', RelationshipKind.PendingIncoming),
      ]}
      outgoing={[entry('acct_cita', RelationshipKind.PendingOutgoing)]}
      profiles={profiles}
      busy={new Set<Id>(['acct_budi' as Id])}
      onAccept={NOOP}
      onDecline={NOOP}
    />,
  );
}

test('the phone’s Friends view answers an incoming request where it lists it', () => {
  const markup = phoneRequests();

  // The section names itself and counts what it holds.
  assert.ok(markup.includes('Requests (3)'), 'the section must state its count');
  // An incoming request carries the wire’s one answer, named and enabled.
  const ada = markup.slice(markup.indexOf('Ada Lovelace'), markup.indexOf('Budi'));
  assert.ok(ada.includes('wants to be friends'), 'an incoming row must say what it is');
  assert.ok(ada.includes('>Accept</button>'), 'an incoming row must offer Accept');
  assert.ok(ada.includes('>Decline</button>'), 'an incoming row must offer Decline');
  assert.ok(!ada.includes('disabled'), 'a settled row’s buttons must be enabled');
  // A busy row disables its pair together — they share one wire call.
  const budi = markup.slice(markup.indexOf('Budi'), markup.indexOf('Cita'));
  assert.equal(
    (budi.match(/disabled/g) ?? []).length,
    2,
    'a busy row must disable both of its buttons',
  );
  // An outgoing request is the sent fact it is: stated, with no unsend to offer.
  const cita = markup.slice(markup.indexOf('Cita'));
  assert.ok(cita.includes('request sent'), 'an outgoing row must say it was sent');
  assert.ok(!cita.includes('>Accept</button>'), 'an outgoing row owes no answer');
});

test('a graph with no pending requests renders no requests section at all', () => {
  const markup = renderToStaticMarkup(
    <FriendRequestsSection
      incoming={[]}
      outgoing={[]}
      profiles={new Map()}
      onAccept={NOOP}
      onDecline={NOOP}
    />,
  );
  assert.equal(markup, '', 'an empty graph owes neither heading nor rows');
});

test('the bell answers a pending friend request and leaves the rest inert', () => {
  const request: InboxItem = {
    id: 'note_1' as Id,
    // The wire carries the kind as the NotificationKind enum’s number in a string.
    kind: '4',
    at: Date.now(),
    actorId: 'acct_ada' as Id,
  };

  // A pending request — the caller has read the graph — carries its answer.
  const answered = renderToStaticMarkup(
    <NotificationRow
      item={request}
      actorName="Ada Lovelace"
      actions={<FriendRequestActions busy={false} onAccept={NOOP} onDecline={NOOP} />}
    />,
  );
  assert.ok(
    answered.includes('Ada Lovelace — Friend request'),
    'the row must name the kind in words, not the wire’s number',
  );
  assert.ok(answered.includes('>Accept</button>'), 'a pending request must offer Accept');
  assert.ok(answered.includes('>Decline</button>'), 'a pending request must offer Decline');

  // The same row without actions — an answered request, or a kind the wire gives no action —
  // stays inert: no button the wire cannot back.
  const inert = renderToStaticMarkup(<NotificationRow item={request} actorName="Ada Lovelace" />);
  assert.ok(!inert.includes('>Accept</button>'), 'an unbacked row must offer no answer');
  assert.ok(inert.includes('Friend request'), 'the row still says what happened');

  // A busy answer disables both halves together.
  const busy = renderToStaticMarkup(<FriendRequestActions busy onAccept={NOOP} onDecline={NOOP} />);
  assert.equal(
    (busy.match(/disabled/g) ?? []).length,
    2,
    'the busy state must disable the pair together',
  );
});

test('the home header’s people search wears the teal band’s ink, not the panel’s', () => {
  const closed = renderToStaticMarkup(
    <FriendsSearch
      tone="home"
      open={false}
      active={false}
      query=""
      onQueryChange={NOOP}
      onSubmit={NOOP}
      onReveal={NOOP}
      onDismiss={NOOP}
    />,
  );
  assert.ok(
    closed.includes('aria-label="Search people by username"'),
    'the icon must still offer the search by name',
  );
  assert.ok(closed.includes('tbtn tbtn-sm'), 'the home icon must wear the header’s button ink');
  assert.ok(!closed.includes('panel-head-icon'), 'the panel’s ink would vanish on the teal band');

  const open = renderToStaticMarkup(
    <FriendsSearch
      tone="home"
      open
      active
      query="reason"
      onQueryChange={NOOP}
      onSubmit={NOOP}
      onReveal={NOOP}
      onDismiss={NOOP}
    />,
  );
  assert.ok(open.includes('autofocus=""'), 'the revealed field must arrive ready to type in');
  assert.ok(
    open.includes('aria-label="Close search"'),
    'the revealed field must carry its way out',
  );
  assert.ok(open.includes('mhome-head-search'), 'the revealed form must hold the header’s width');
});
