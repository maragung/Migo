/**
 * What an error is allowed to say to the user.
 *
 * Two brief rules meet in this one small function. Section 161: an internal failure must surface only a
 * public message — never a stack trace, a file path, or an underlying cause. And the auth rule the
 * login screen depends on: a failed sign-in must look identical whether the account does not exist or
 * the password was wrong, because any difference is an account-enumeration oracle. `friendlyError` is
 * the only thing standing between a raw thrown value and the string a user reads, so its regressions
 * are silent and serious: a change that returned `String(error)` for the fall-through case would leak
 * the contents of every unexpected exception into the UI, and every functional test would still pass.
 *
 * These tests feed it errors deliberately loaded with things that must not escape — a message full of
 * absolute paths, a populated `cause`, an internal host in a transport failure — and assert the user
 * sees a fixed, generic line instead. The enumeration rule is checked by mapping two differently-coded
 * errors that the server has collapsed to one public answer and asserting the client cannot tell them
 * apart either.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { RemoteError, SdkError, TimeoutError, TransportError } from '@migo/sdk';

import { friendlyError } from '../src/lib/migo/errors.js';

const GENERIC = 'Something went wrong. Please try again.';

test('an unexpected non-SDK error never leaks its message, stack, path, or cause', () => {
  const leaky = new Error('failed reading /home/dev/migo/secret.key while decrypting');
  leaky.stack = 'Error: boom\n    at /home/dev/migo/src/app.js:12:7';
  (leaky as Error & { cause?: unknown }).cause = new Error('inner detail /etc/passwd');

  const shown = friendlyError(leaky);
  assert.equal(shown, GENERIC);
  for (const secret of ['secret.key', '/home/dev', '/etc/passwd', '.js:', 'boom']) {
    assert.ok(!shown.includes(secret), `leaked "${secret}"`);
  }
});

test('a thrown value that is not an Error at all still maps to the generic line', () => {
  assert.equal(friendlyError('raw string boom'), GENERIC);
  assert.equal(friendlyError({ message: 'looks like an error but is not' }), GENERIC);
  assert.equal(friendlyError(undefined), GENERIC);
  assert.equal(friendlyError(null), GENERIC);
});

test('a transport failure is rephrased, hiding any internal host or address it carried', () => {
  const shown = friendlyError(new TransportError('connect ECONNREFUSED 10.4.2.7:8080'));
  assert.equal(shown, 'Could not reach the Migo server. Check your connection and try again.');
  assert.ok(!shown.includes('10.4.2.7'));
  assert.ok(!shown.includes('8080'));
});

test('a timeout is rephrased, hiding the internal deadline detail it carried', () => {
  const shown = friendlyError(new TimeoutError('no reply to opcode 0x2141 after 30000ms'));
  assert.equal(shown, 'The server took too long to respond. Check your connection and try again.');
  assert.ok(!shown.includes('0x2141'));
});

test('a server refusal shows the curated public message the server chose', () => {
  const err = new RemoteError(429, 'RATE_LIMITED', 'Too many attempts. Try again later.', 5_000);
  const shown = friendlyError(err);
  // The web client trusts the server's public message — but the symbol prefix the SDK's JS
  // message carries is stripped: "RATE_LIMITED: Too many…" reaches a person as "Too many…".
  assert.equal(shown, 'Too many attempts. Try again later.');
  assert.ok(!shown.includes('RATE_LIMITED'), 'the machine symbol never reaches the person');
  // Even carrying a retry hint, nothing internal (a stack) is appended.
  assert.ok(!shown.includes('at '));
});

test('a full room turns a person away in plain words, never as a symbol or a fault', () => {
  // ROOM_FULL is a condition worth naming in our own kinder words: the door is shut for now, not a
  // fault the person caused. The symbol never reaches them, and the friendly line stands even when
  // the server withheld its own text (the empty message here), so the symbol-fold never runs.
  const shown = friendlyError(new RemoteError(1505, 'ROOM_FULL', ''));
  assert.equal(shown, 'This room is full right now. Try again later, or find another room.');
  assert.ok(!shown.includes('ROOM_FULL'), 'the machine symbol never reaches the person');
});

test('an account lockout reaches the person as a wait, never as a symbol', () => {
  const err = new RemoteError(
    1406,
    'AUTH_LOCKED',
    'Account temporarily locked. Retry in 90 s',
    90_000,
  );
  const shown = friendlyError(err);
  assert.equal(shown, 'Account temporarily locked. Retry in 90 s');
  assert.ok(!shown.includes('AUTH_LOCKED'), 'the machine symbol never reaches the person');
});

test('a wrong password and a missing account are indistinguishable to the user', () => {
  // The server collapses both to one public answer; the client must not re-introduce a difference,
  // e.g. by branching on the numeric code. Two different codes, one identical public message:
  const wrongPassword = new RemoteError(1001, 'UNAUTHENTICATED', 'Invalid credentials.');
  const noSuchAccount = new RemoteError(1002, 'UNAUTHENTICATED', 'Invalid credentials.');
  assert.equal(friendlyError(wrongPassword), friendlyError(noSuchAccount));
});

test('a withheld error message never leaks the machine symbol, and a restricted lookup is indistinguishable from a missing one', () => {
  // The server withholds the human message by default (section 161); the SDK then folds the empty
  // message into the bare symbol, so `error.message` is e.g. "PRIVACY_RESTRICTED". Surfacing that
  // would both leak internal vocabulary and — the section 180 rule — let a restricted profile be
  // told apart from a missing one. Both must collapse to one generic, symbol-free line.
  const restricted = friendlyError(new RemoteError(1003, 'PRIVACY_RESTRICTED', ''));
  const missing = friendlyError(new RemoteError(1004, 'NOT_FOUND', ''));
  assert.equal(restricted, missing);
  for (const shown of [restricted, missing]) {
    assert.ok(!/PRIVACY_RESTRICTED|NOT_FOUND/.test(shown), `leaked a symbol: ${shown}`);
    assert.ok(!/block/i.test(shown));
    assert.ok(!/restrict/i.test(shown));
    assert.ok(shown.length > 0);
  }
});

test('a bare SDK error surfaces its own developer message, and every case is non-empty', () => {
  assert.equal(
    friendlyError(new SdkError('the client is not connected')),
    'the client is not connected',
  );
  for (const value of [new Error('x'), 'y', 42, new TransportError('z')]) {
    assert.ok(friendlyError(value).length > 0);
  }
});
