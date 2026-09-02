/**
 * What the Owner/CEO's admin management page is allowed to say and offer.
 *
 * The panel's data all arrives from the admin REST surface, so its rendering tests feed the
 * exported presentational components exactly what the SDK calls return and pin the rules that
 * would silently regress under a "helpful" refactor:
 *
 *   1. **A row offers removal, labelled with the account it acts on.** One Revoke per admin,
 *      named in its `aria-label`, so a screen reader hears which appointment a button ends.
 *   2. **The grant form gates its submit.** An empty or blank username leaves the Appoint
 *      button disabled — the gate lives in the markup, not only in the handler.
 *   3. **A busy form stays closed.** While a grant is in flight the button is disabled even
 *      with a name typed, so a double submit cannot race the reload.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { AdminView, Id } from '@migo/sdk';

import { AdminRowView, GrantAdminFormView } from '../src/components/admins-panel.js';

const NOW = Date.parse('2026-09-02T12:00:00Z');

function admin(fields: { id: string; username: string; grantedAgoMs?: number }): AdminView {
  return {
    accountId: fields.id as Id,
    username: fields.username,
    grantedBy: 'id_granter' as Id,
    grantedAtMs: NOW - (fields.grantedAgoMs ?? 0),
  };
}

const WARDEN = admin({ id: 'id_warden', username: 'warden', grantedAgoMs: 3_600_000 });

test('an admin row names the account and offers exactly one labelled revoke', () => {
  const markup = renderToStaticMarkup(
    <AdminRowView admin={WARDEN} busy={false} onRevoke={() => {}} />,
  );

  assert.ok(markup.includes('warden'), 'the username is missing');
  assert.ok(markup.includes('Global admin'), 'the standing mark is missing');
  assert.ok(markup.includes('appointed'), 'the appointment line is missing');
  const revokes = markup.match(/aria-label="Revoke global admin /g) ?? [];
  assert.equal(revokes.length, 1, 'the row must carry exactly one revoke control');
  assert.ok(
    markup.includes('aria-label="Revoke global admin warden"'),
    'the revoke control must name the account it acts on',
  );
});

test('a busy row disables its revoke control rather than hiding it', () => {
  const markup = renderToStaticMarkup(<AdminRowView admin={WARDEN} busy onRevoke={() => {}} />);
  assert.ok(markup.includes('disabled'), 'the revoke control must be disabled while busy');
});

test('the grant form stays gated until the draft is a name', () => {
  for (const blank of ['', '   ']) {
    const markup = renderToStaticMarkup(
      <GrantAdminFormView username={blank} busy={false} onUsername={() => {}} onGrant={() => {}} />,
    );
    assert.ok(
      markup.includes('disabled'),
      `a draft of ${JSON.stringify(blank)} must leave the appoint button disabled`,
    );
  }

  const markup = renderToStaticMarkup(
    <GrantAdminFormView username="warden" busy={false} onUsername={() => {}} onGrant={() => {}} />,
  );
  assert.ok(
    !markup.includes('disabled'),
    'a real draft and an idle form must leave the appoint button enabled',
  );
  assert.ok(markup.includes('username to appoint'), 'the input must say what it wants');
});

test('a grant in flight keeps the appoint button closed', () => {
  const markup = renderToStaticMarkup(
    <GrantAdminFormView username="warden" busy onUsername={() => {}} onGrant={() => {}} />,
  );
  assert.ok(
    markup.includes('disabled'),
    'a busy form must not offer a second submit of the same appointment',
  );
});
