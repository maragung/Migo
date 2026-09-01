/**
 * What the Settings tab is allowed to say about the account's devices, sessions, and password.
 *
 * The panel's data all arrives from the account-management REST surface, so its rendering tests
 * feed the exported presentational components exactly what the SDK calls return and pin the
 * rules that would silently regress under a "helpful" refactor:
 *
 *   1. **The current session is identified, not revocable.** The server refuses to let a session
 *      revoke itself, so a revoke button on the viewing session's own row is a button that
 *      always errors — the row must carry the "This device" mark and no control instead.
 *   2. **Other sessions stay revocable, one control each.** A row per device, a Revoke per row,
 *      labelled with the device it acts on.
 *   3. **The device list offers removal only where it means something.** The current device and
 *      an already-revoked one get no control; every other device gets exactly one Remove,
 *      labelled with the device it acts on, and a revoked device stays listed with its mark —
 *      "which phone was that" is a question about the past as much as the present.
 *   4. **The password form gates its submit.** An empty current password, a short new password,
 *      or a confirm that disagrees leaves the save button disabled — the gate lives in the
 *      markup, not only in the handler.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { AccountSession, DeviceSummary, Id } from '@migo/sdk';

import { DeviceList, PasswordFormView, SessionList } from '../src/components/settings-panel.js';

const NOW = Date.parse('2026-08-26T12:00:00Z');

function session(fields: { id: string; device: string; seenAgoMs?: number }): AccountSession {
  return {
    id: fields.id as Id,
    device: fields.device,
    created_at: NOW - 86_400_000,
    last_seen_at: NOW - (fields.seenAgoMs ?? 0),
    ip_class: 0,
  };
}

function device(fields: {
  id: string;
  name: string;
  current?: boolean;
  revoked?: boolean;
  credential?: boolean;
  seenAgoMs?: number;
}): DeviceSummary {
  return {
    deviceId: fields.id as Id,
    displayName: fields.name,
    platform: 'web',
    status: fields.revoked ? 'revoked' : 'active',
    createdAtMs: NOW - 86_400_000,
    lastSeenAtMs: NOW - (fields.seenAgoMs ?? 0),
    hasCredential: fields.credential ?? false,
    isCurrent: fields.current ?? false,
  };
}

const LAPTOP = session({ id: 'sess_this', device: 'Migo Web (Firefox)' });
const PHONE = session({ id: 'sess_other', device: 'Migo iOS', seenAgoMs: 3_600_000 });

test('the session list marks the viewing session and offers it no revoke control', () => {
  const markup = renderToStaticMarkup(
    <SessionList
      sessions={[LAPTOP, PHONE]}
      currentSessionId={'sess_this' as Id}
      busyId={null}
      onRevoke={() => {}}
    />,
  );

  assert.ok(markup.includes('This device'), 'the current session lost its identifying mark');
  assert.ok(markup.includes('Migo Web (Firefox)'), 'the current device name is missing');
  assert.ok(markup.includes('last active'), 'the last-active line is missing');
  // Exactly one revoke control: the other session's. The viewing session must not be offered
  // a button the server is documented to refuse.
  assert.equal((markup.match(/>Revoke</g) ?? []).length, 1);
  assert.ok(
    !markup.includes('aria-label="Revoke session on Migo Web (Firefox)"'),
    'the current session was offered a self-revoke',
  );
  assert.ok(
    markup.includes('aria-label="Revoke session on Migo iOS"'),
    'the other session\u2019s revoke lost its device-specific label',
  );
});

test('a busy revoke disables only its own row', () => {
  const markup = renderToStaticMarkup(
    <SessionList
      sessions={[LAPTOP, PHONE]}
      currentSessionId={'sess_this' as Id}
      busyId={'sess_other' as Id}
      onRevoke={() => {}}
    />,
  );

  assert.ok(markup.includes('disabled'), 'an in-flight revoke must disable its control');
  // The busy row is the only disabled one — the current session has no control to disable.
  assert.equal((markup.match(/disabled/g) ?? []).length, 1);
});

test('an account with one session sees it marked and nothing revocable', () => {
  const markup = renderToStaticMarkup(
    <SessionList
      sessions={[LAPTOP]}
      currentSessionId={'sess_this' as Id}
      busyId={null}
      onRevoke={() => {}}
    />,
  );

  assert.ok(!markup.includes('>Revoke<'), 'a lone current session was offered a revoke');
});

test('the password form renders all three fields and gates the save on a complete, matching draft', () => {
  const ready = renderToStaticMarkup(
    <PasswordFormView
      current="old-secret"
      next="new-secret-1"
      confirm="new-secret-1"
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );

  assert.ok(ready.includes('aria-label="Current password"'), 'the current field is missing');
  assert.ok(ready.includes('aria-label="New password"'), 'the new field is missing');
  assert.ok(ready.includes('aria-label="Confirm new password"'), 'the confirm field is missing');
  assert.ok(ready.includes('>Change password</button>'), 'the submit control is missing');
  // A complete, matching draft leaves the save enabled.
  assert.ok(!ready.includes('disabled'), 'a complete draft must not disable the save');

  // A confirm that disagrees keeps the save disabled — the gate must live in the markup.
  const mismatch = renderToStaticMarkup(
    <PasswordFormView
      current="old-secret"
      next="new-secret-1"
      confirm="different"
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(mismatch.includes('disabled'), 'a mismatched confirm must disable the save');

  // A short new password is not a password the server would accept; the form says so by gating.
  const short = renderToStaticMarkup(
    <PasswordFormView
      current="old-secret"
      next="short"
      confirm="short"
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(short.includes('disabled'), 'a short new password must disable the save');
});

test('the password form states its failure and its success beside the fields they belong to', () => {
  const failed = renderToStaticMarkup(
    <PasswordFormView
      current="wrong"
      next="new-secret-1"
      confirm="new-secret-1"
      busy={false}
      error="The server rejected the request."
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(failed.includes('form-error'), 'a refused change lost its error line');

  const done = renderToStaticMarkup(
    <PasswordFormView
      current=""
      next=""
      confirm=""
      busy={false}
      error={null}
      saved
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(done.includes('Password changed.'), 'a completed change lost its success line');
});

test('the device list marks the current device and offers it no removal', () => {
  const markup = renderToStaticMarkup(
    <DeviceList
      devices={[
        device({ id: 'dev_this', name: 'Ada laptop', current: true, credential: true }),
        device({ id: 'dev_phone', name: 'Ada phone', seenAgoMs: 3_600_000 }),
      ]}
      busyId={null}
      onRemove={() => {}}
    />,
  );

  assert.ok(markup.includes('This device'), 'the current device lost its identifying mark');
  assert.ok(
    markup.includes('holds a sign-in credential'),
    'the credential mark is missing — the security question a device list exists to answer',
  );
  // Exactly one removal control: the other device's.
  assert.equal((markup.match(/>Remove</g) ?? []).length, 1);
  assert.ok(
    !markup.includes('aria-label="Remove device Ada laptop"'),
    'the current device was offered a self-removal',
  );
  assert.ok(
    markup.includes('aria-label="Remove device Ada phone"'),
    'the other device’s removal lost its device-specific label',
  );
});

test('a revoked device stays listed with its mark and no removal control', () => {
  const markup = renderToStaticMarkup(
    <DeviceList
      devices={[
        device({ id: 'dev_this', name: 'Ada laptop', current: true }),
        device({ id: 'dev_old', name: 'Old phone', revoked: true }),
      ]}
      busyId={null}
      onRemove={() => {}}
    />,
  );

  assert.ok(markup.includes('Old phone'), 'a revoked device must stay in the list');
  assert.ok(markup.includes('Revoked'), 'the revoked mark is missing');
  assert.ok(!markup.includes('>Remove<'), 'a revoked device was offered a removal');
});

test('a busy removal disables only its own row', () => {
  const markup = renderToStaticMarkup(
    <DeviceList
      devices={[
        device({ id: 'dev_this', name: 'Ada laptop', current: true }),
        device({ id: 'dev_phone', name: 'Ada phone' }),
      ]}
      busyId={'dev_phone' as Id}
      onRemove={() => {}}
    />,
  );

  assert.ok(markup.includes('disabled'), 'an in-flight removal must disable its control');
  assert.equal((markup.match(/disabled/g) ?? []).length, 1);
});
