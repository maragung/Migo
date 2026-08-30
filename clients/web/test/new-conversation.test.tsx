/**
 * What the new-conversation dialog's people search is allowed to ask and show.
 *
 * The search is the dialog's working path now that account ids are gone from the form: a
 * debounced username prefix query against `social.search`, plus a friends quick-pick above it.
 * Two rules carry correctness weight:
 *
 *   1. **An empty query asks the server nothing.** The wire's search is a prefix lookup — an
 *      empty (or whitespace-only) query is not a question, and sending one would be a request
 *      whose answer is predefined.
 *   2. **A real query is trimmed and bounded.** Spaces are not part of a username prefix, and
 *      the picker asks for one small page, not the directory.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { Id, SuggestedUser } from '@migo/sdk';

import { PersonPickRow, searchPeople } from '../src/components/new-conversation-dialog.js';

function person(accountId: string, username: string, displayName: string): SuggestedUser {
  return { accountId: accountId as Id, username, displayName, mutualFriends: 0 };
}

/** A client double recording what the search path asked of the social domain. */
function socialDouble(reply: SuggestedUser[]): {
  client: { social: { search: (query: string, limit?: number) => Promise<SuggestedUser[]> } };
  queries: Array<{ query: string; limit?: number }>;
} {
  const queries: Array<{ query: string; limit?: number }> = [];
  return {
    queries,
    client: {
      social: {
        search: (query, limit) => {
          queries.push({ query, limit });
          return Promise.resolve(reply);
        },
      },
    },
  };
}

test('a real query is trimmed, bounded, and answered by the server\u2019s results', async () => {
  const found = [person('user_ada', 'ada', 'Ada Lovelace')];
  const { client, queries } = socialDouble(found);

  const results = await searchPeople(client, '  ada ');

  assert.deepEqual(results, found, 'the results did not pass through unchanged');
  assert.deepEqual(queries, [{ query: 'ada', limit: 10 }], 'the query must be trimmed and bounded');
});

test('an empty or whitespace query resolves to nothing and asks the server nothing', async () => {
  for (const blank of ['', '   ']) {
    const { client, queries } = socialDouble([]);
    const results = await searchPeople(client, blank);
    assert.deepEqual(results, [], 'a blank query must resolve to no one');
    assert.equal(queries.length, 0, 'a blank query must not reach the server');
  }
});

test('a candidate row shows the person and a select control that says when they are chosen', () => {
  const unpicked = renderToStaticMarkup(
    <PersonPickRow
      accountId={'user_ada' as Id}
      displayName="Ada Lovelace"
      username="ada"
      onPick={() => {}}
    />,
  );
  assert.ok(unpicked.includes('Ada Lovelace'), 'the candidate\u2019s name is missing');
  assert.ok(unpicked.includes('@ada'), 'the candidate\u2019s username is missing');
  assert.ok(unpicked.includes('>Select</button>'), 'the select control is missing');
  assert.ok(!unpicked.includes('disabled'), 'an unpicked candidate must be selectable');

  const picked = renderToStaticMarkup(
    <PersonPickRow
      accountId={'user_ada' as Id}
      displayName="Ada Lovelace"
      username="ada"
      picked
      onPick={() => {}}
    />,
  );
  assert.ok(picked.includes('>Selected</button>'), 'a picked candidate must say so');
  assert.ok(picked.includes('disabled'), 'a picked candidate must not be re-selectable');
});

test('a candidate row carries a mutual-friends note when the server sent one', () => {
  const markup = renderToStaticMarkup(
    <PersonPickRow
      accountId={'user_ada' as Id}
      displayName="Ada Lovelace"
      username="ada"
      note="3 mutual friends"
      onPick={() => {}}
    />,
  );
  assert.ok(markup.includes('3 mutual friends'), 'the mutual-friends note is missing');
});
