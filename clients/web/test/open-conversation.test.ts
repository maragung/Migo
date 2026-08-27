/**
 * Where the open-conversation id is allowed to live in a URL.
 *
 * Section 178's privacy rule reaches the address bar: which conversation a user has open is metadata
 * about who talks to whom, and it must not travel to the static host that serves the bundle. The
 * client encodes it in the URL *fragment* (`#c=<id>`) precisely because a fragment is never put on the
 * HTTP request line and never reaches a server log — unlike a path segment or a query string. A
 * refactor that moved the id into the path or the query would leak that metadata on every navigation
 * while behaving identically in the UI, so the tests below pin the location and the round-trip.
 *
 * The second hazard is the id itself. Conversation ids are opaque server tokens, not known at build
 * time, and if one contained `&`, `=`, or `#` an unescaped href would either smuggle a second
 * fragment parameter or truncate. The href builder percent-encodes, and the parser round-trips through
 * `URLSearchParams`; the tests feed it an adversarial id to prove one field cannot become two. The
 * navigation helpers are also asserted to be inert without a `window`, since the bundle is prerendered
 * with no URL at all.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import type { Id } from '@migo/sdk';

import {
  closeConversation,
  conversationHref,
  openConversation,
  parseConversationFragment,
} from '../src/lib/migo/use-open-conversation.js';
import { installFakeWindow } from './support/dom-stubs.js';

test('the conversation href carries the id in the fragment, never in a path or query', () => {
  const href = conversationHref('conv_0001' as Id);
  assert.ok(href.startsWith('#'), `expected a fragment, got ${href}`);
  // A fragment reaches no server log; a path or query would. There must be neither.
  assert.ok(!href.includes('/'), 'a path segment would leak to the host access log');
  assert.ok(!href.includes('?'), 'a query string would leak to the host access log');
  assert.equal(href, '#c=conv_0001');
});

test('an id survives the href-then-parse round-trip', () => {
  const id = 'conv_9f3a-b2' as Id;
  assert.equal(parseConversationFragment(conversationHref(id)), id);
});

test('the parser reads the id whether or not the fragment keeps its leading hash', () => {
  assert.equal(parseConversationFragment('#c=abc'), 'abc');
  assert.equal(parseConversationFragment('c=abc'), 'abc');
});

test('an empty, hash-only, or unrelated fragment opens no conversation', () => {
  assert.equal(parseConversationFragment(''), null);
  assert.equal(parseConversationFragment('#'), null);
  assert.equal(parseConversationFragment('#other=x'), null);
  // A present-but-empty value is treated as no conversation, not as the id "".
  assert.equal(parseConversationFragment('#c='), null);
});

test('an id containing URL metacharacters cannot smuggle a second fragment parameter', () => {
  // If the builder did not encode, this id would parse back as c="a" plus a rogue "admin=1".
  const hostile = 'a&admin=1&c=b#x' as Id;
  const href = conversationHref(hostile);
  assert.ok(!href.includes('&'), 'a raw & would split the fragment into two parameters');
  // The whole hostile string comes back as one opaque id, and nothing else leaks in beside it.
  const parsed = parseConversationFragment(href);
  assert.equal(parsed, hostile);
  const params = new URLSearchParams(href.slice(1));
  assert.equal([...params.keys()].length, 1);
  assert.equal(params.get('admin'), null);
});

test('opening a conversation writes only the fragment, leaving path and query untouched', () => {
  const win = installFakeWindow('/chat/', '?ref=email');
  try {
    openConversation('conv_42' as Id);
    assert.equal(win.location.hash, '#c=conv_42');
    assert.equal(parseConversationFragment(win.location.hash), 'conv_42');
    // The privacy point: the id never migrated into the parts a server sees.
    assert.equal(win.location.pathname, '/chat/');
    assert.equal(win.location.search, '?ref=email');
  } finally {
    win.restore();
  }
});

test('closing a conversation clears the fragment without leaving a bare hash behind', () => {
  const win = installFakeWindow('/chat/', '');
  try {
    openConversation('conv_42' as Id);
    closeConversation();
    assert.equal(parseConversationFragment(win.location.hash), null);
    // A naive `location.hash = ""` would leave "#" in the bar and a dead history entry; replaceState
    // to the bare path must not.
    assert.notEqual(win.location.hash, '#');
    assert.equal(win.location.hash, '');
  } finally {
    win.restore();
  }
});

test('the navigation helpers are inert during prerender, when there is no window', () => {
  // The bundle is rendered once at build time with no URL; these must not throw then.
  assert.equal(typeof window, 'undefined');
  assert.doesNotThrow(() => openConversation('conv_1' as Id));
  assert.doesNotThrow(() => closeConversation());
});
