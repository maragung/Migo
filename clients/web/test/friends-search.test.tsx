/**
 * What the Friends tab's search is allowed to ask of the header.
 *
 * The search field used to sit in the panel head permanently, spending the header's narrow
 * right-hand row on a field that is idle most of the time. It now waits behind an icon: the
 * icon's tap reveals the field in the icon's place — focused, so the tap is the whole trip into
 * a query — and the field leaves again when the search is finished, through its own close
 * control or a blur on an empty query. These tests pin the two states and their affordances:
 *
 *   1. **The icon default.** The header offers the search as an icon button, not a field, and
 *      the panel itself ships in that state.
 *   2. **The reveal.** The open control is the field, auto-focused, with the close beside it —
 *      and the icon gone from the row while the field holds its place.
 *   3. **The chosen icon.** With results on screen and the field collapsed, the icon says so
 *      the same way the view icons do.
 *
 * `renderToStaticMarkup` runs no effects and fires no events, so the collapse paths themselves
 * (the close's click, the empty field's blur) are wiring inside {@link FriendsPanel} rather
 * than things this suite can drive; what it can pin is that each state offers exactly the
 * controls its half of the bargain needs.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { FriendsPanel, FriendsSearch } from '../src/components/friends-panel.js';
import { ConversationsProvider } from '../src/lib/migo/conversations-provider.js';
import { MutedProvider } from '../src/lib/migo/muted-provider.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import type { MigoContextValue } from '../src/lib/migo/provider.js';

const ME = 'acct_self' as Id;

/** The ready-session context double: connected, but with nothing fetched yet. */
const CONTEXT: MigoContextValue = {
  status: 'ready',
  connectionState: 'ready',
  accountId: ME,
  deviceId: null,
  error: null,
  resetNonce: 0,
  persistKeyStore: () => {},
  client: null,
  register: () => Promise.resolve(),
  loginWithFile: () => Promise.resolve(),
  logout: () => Promise.resolve(),
};

/** The provider stack the panel reads, over the context double. */
function sessionShell(node: ReactNode): string {
  return renderToStaticMarkup(
    <MigoContext.Provider value={CONTEXT}>
      <ConversationsProvider>
        <MutedProvider>{node}</MutedProvider>
      </ConversationsProvider>
    </MigoContext.Provider>,
  );
}

const NOOP = () => {};

test('the search waits behind its icon — the panel offers a button, never an idle field', () => {
  // The control on its own, closed.
  const closed = renderToStaticMarkup(
    <FriendsSearch
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
    'the icon must offer the search by name',
  );
  assert.ok(!closed.includes('type="search"'), 'a closed control must not render the field');
  assert.ok(!closed.includes('Close search'), 'a closed control owes no way out');

  // The panel itself ships closed: the icon in the header, the field nowhere.
  const panel = sessionShell(<FriendsPanel onOpenConversation={NOOP} />);
  assert.ok(
    panel.includes('aria-label="Search people by username"'),
    'the header must carry the search icon',
  );
  assert.ok(!panel.includes('type="search"'), 'the header must not carry an idle search field');
  // The new-conversation control stays visible beside the icon, as it always was.
  assert.ok(
    panel.includes('aria-label="New conversation"'),
    'the new-conversation control must stay beside the search',
  );
});

test('the revealed field arrives focused, in the icon’s place, with its way out', () => {
  const open = renderToStaticMarkup(
    <FriendsSearch
      open
      active
      query="reason"
      onQueryChange={NOOP}
      onSubmit={NOOP}
      onReveal={NOOP}
      onDismiss={NOOP}
    />,
  );

  assert.ok(open.includes('type="search"'), 'the revealed control must be the field');
  // The server renderer spells the prop in markup lowercase — `autofocus`, the HTML attribute.
  assert.ok(open.includes('autofocus=""'), 'the field must arrive ready to type in');
  assert.ok(open.includes('value="reason"'), 'the field must hold the query it was given');
  assert.ok(
    open.includes('aria-label="Close search"'),
    'the field must carry its explicit way out',
  );
  // The icon is gone while the field holds its place — one search control in the row, not two.
  assert.ok(!open.includes('aria-pressed='), 'the icon must leave with the field’s arrival');
});

test('a collapsed field over results leaves the icon saying the view it is on', () => {
  const collapsed = renderToStaticMarkup(
    <FriendsSearch
      open={false}
      active
      query="reason"
      onQueryChange={NOOP}
      onSubmit={NOOP}
      onReveal={NOOP}
      onDismiss={NOOP}
    />,
  );

  // The view icons mark their list with `chosen` and `aria-pressed`; the search icon says the
  // same thing about the results view its field left on screen.
  assert.ok(collapsed.includes('aria-pressed="true"'), 'the icon must own its pressed state');
  assert.ok(collapsed.includes('chosen'), 'the icon must wear the chosen ink');
});
