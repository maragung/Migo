/**
 * Errors surface only what section 161 permits: a stable code and symbol, and a hint that may be
 * empty and must never be shown or parsed.
 *
 * The server is deliberately terse about failures. A caller branches on {@link RemoteError.code} and
 * {@link RemoteError.symbol} — never on the human message — and the SDK must not manufacture detail
 * the server withheld. Two invariants matter most and both fail silently if broken: a malformed or
 * opaque error body must collapse to a generic internal error rather than leak raw server text into
 * a structured field, and an authentication failure must look identical whether the account does not
 * exist or the password was wrong, because any observable difference is an account-enumeration
 * oracle. These tests pin both, plus the retry and class-hierarchy contracts a caller relies on.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { RemoteError, SdkError, TimeoutError, TransportError } from '../src/index.js';
import { CODE } from '@migo/protocol';

test('a present hint follows the symbol; an absent hint leaves the symbol standing alone', () => {
  const withHint = new RemoteError(
    CODE.VALIDATION_FAILED,
    'VALIDATION_FAILED',
    'username too long',
  );
  // The symbol leads so a stack trace is legible, and the hint is clearly secondary.
  assert.equal(withHint.message, 'VALIDATION_FAILED: username too long');

  const noHint = new RemoteError(CODE.UNAUTHENTICATED, 'UNAUTHENTICATED', '');
  // No fabricated text: an empty hint yields exactly the symbol, nothing appended.
  assert.equal(noHint.message, 'UNAUTHENTICATED');
});

test('an authentication failure is indistinguishable for a missing account and a wrong password', () => {
  // The server answers both with INVALID_CREDENTIALS and an empty hint; if it did not, the SDK would
  // still expose no field that separates them. Build the two the way the REST layer would and prove
  // every observable is equal.
  const missingAccount = RemoteError.fromEnvelope(401, {
    error: { code: CODE.INVALID_CREDENTIALS, symbol: 'INVALID_CREDENTIALS', message: '' },
  });
  const wrongPassword = RemoteError.fromEnvelope(401, {
    error: { code: CODE.INVALID_CREDENTIALS, symbol: 'INVALID_CREDENTIALS', message: '' },
  });

  assert.equal(missingAccount.code, wrongPassword.code);
  assert.equal(missingAccount.symbol, wrongPassword.symbol);
  assert.equal(missingAccount.message, wrongPassword.message);
  assert.equal(missingAccount.field, wrongPassword.field);
  assert.equal(missingAccount.retryAfterMs, wrongPassword.retryAfterMs);
  // And the code is the credential one, not something that names which half failed.
  assert.equal(missingAccount.code, CODE.INVALID_CREDENTIALS);
});

test('a well-formed error envelope maps to its code, symbol, and offending field', () => {
  const error = RemoteError.fromEnvelope(400, {
    error: {
      code: CODE.VALIDATION_FAILED,
      symbol: 'VALIDATION_FAILED',
      message: 'bad username',
      field: 'username',
    },
  });
  assert.equal(error.code, CODE.VALIDATION_FAILED);
  assert.equal(error.symbol, 'VALIDATION_FAILED');
  assert.equal(error.field, 'username');
});

test('a body that is not the error envelope collapses to a generic internal error', () => {
  // A proxy's HTML page, a truncated response, a bare string — none of it is structured server
  // detail, and none of it may be surfaced as though it were. The status is all that survives.
  for (const body of [null, 'gateway timeout', { unexpected: true }, 42]) {
    const error = RemoteError.fromEnvelope(502, body);
    assert.equal(error.code, CODE.INTERNAL_ERROR);
    assert.equal(error.symbol, 'INTERNAL_ERROR');
    assert.equal(error.message, 'INTERNAL_ERROR: HTTP 502');
  }
});

test('a partial error object falls back per field rather than trusting a wrong type', () => {
  // A code that is a string, a missing symbol: each field independently falls back to the safe
  // default instead of propagating a malformed value a caller might branch on.
  const error = RemoteError.fromEnvelope(500, { error: { code: 'nope', message: 7 } });
  assert.equal(error.code, CODE.INTERNAL_ERROR);
  assert.equal(error.symbol, 'INTERNAL_ERROR');
  assert.equal(error.message, 'INTERNAL_ERROR', 'a non-string hint must not become the message');
});

test('retryable reads back exactly whether the server suggested a delay', () => {
  const limited = RemoteError.fromEnvelope(429, {
    error: { code: CODE.RATE_LIMITED, symbol: 'RATE_LIMITED', retry_after_ms: 2000 },
  });
  assert.equal(limited.retryAfterMs, 2000);
  assert.equal(limited.retryable, true);

  const credentials = new RemoteError(CODE.INVALID_CREDENTIALS, 'INVALID_CREDENTIALS', '');
  assert.equal(credentials.retryAfterMs, undefined);
  assert.equal(credentials.retryable, false);
});

test('fromMessage defaults a missing hint to empty and preserves the retry hint', () => {
  const error = RemoteError.fromMessage({
    code: CODE.RATE_LIMITED,
    symbol: 'RATE_LIMITED',
    retryAfterMs: 500,
  });
  assert.equal(error.symbol, 'RATE_LIMITED');
  assert.equal(error.message, 'RATE_LIMITED', 'an omitted protocol hint must not appear as text');
  assert.equal(error.retryAfterMs, 500);
});

test('every SDK error is catchable as one type, and the transport errors are distinct', () => {
  const remote = new RemoteError(CODE.INTERNAL_ERROR, 'INTERNAL_ERROR', '');
  const transport = new TransportError('socket closed');
  const timeout = new TimeoutError('no reply');

  // A single `catch (e) { if (e instanceof SdkError) ... }` must catch all of them.
  assert.ok(remote instanceof SdkError);
  assert.ok(transport instanceof SdkError);
  assert.ok(timeout instanceof SdkError);
  // But the two connection failures are their own types, so a caller can tell "server said no" from
  // "the link is gone" without string-matching.
  assert.ok(!(transport instanceof RemoteError));
  assert.equal(transport.name, 'TransportError');
  assert.equal(timeout.name, 'TimeoutError');
  assert.equal(remote.name, 'RemoteError');
});

test('a refusal may carry the replacement captcha the server minted with it', () => {
  // A bootstrap refusal that spent a proof attaches the next challenge in the same envelope,
  // so a form swaps its captcha picture without a second round trip. The attachment is
  // optional and every other error shape is unchanged — the field is simply undefined.
  const refused = RemoteError.fromEnvelope(409, {
    error: {
      code: 1306,
      symbol: 'USERNAME_TAKEN',
      message: 'that username is taken',
      captcha: {
        challenge_id: '01ARZ3NDEKTSV4RRFFQ69G5FAV',
        image_png_base64: 'aGVsbG8=',
        mode: 'image',
        ttl_seconds: 120,
      },
    },
  });
  assert.equal(refused.captcha?.challenge_id, '01ARZ3NDEKTSV4RRFFQ69G5FAV');
  assert.equal(refused.captcha?.image_png_base64, 'aGVsbG8=');
  assert.equal(refused.captcha?.mode, 'image');
  assert.equal(refused.captcha?.ttl_seconds, 120);

  // Without the attachment there is nothing to read: undefined, not an empty object.
  const plain = RemoteError.fromEnvelope(401, {
    error: { code: CODE.INVALID_CREDENTIALS, symbol: 'INVALID_CREDENTIALS', message: '' },
  });
  assert.equal(plain.captcha, undefined);

  // A malformed attachment must not turn a readable refusal into an exception — the error
  // crosses with its code intact and the captcha simply missing.
  const malformed = RemoteError.fromEnvelope(400, {
    error: {
      code: CODE.VALIDATION_FAILED,
      symbol: 'VALIDATION_FAILED',
      message: 'bad',
      captcha: { challenge_id: 42 },
    },
  });
  assert.equal(malformed.code, CODE.VALIDATION_FAILED);
  assert.equal(malformed.captcha, undefined);
});
