/**
 * What the account-file surfaces are allowed to say and do.
 *
 * The container's own cryptography (Argon2id, XChaCha20-Poly1305, the cross-language vectors) is
 * pinned in `packages/crypto`; what is pinned here is everything the auth screens add around it:
 *
 *   1. **The file's name is findable.** A username with spaces or accents must still produce a
 *      `migo-….migo` a person can recognise and click, and an empty sanitisation must not produce a
 *      dotfile.
 *   2. **The credential is judged before it is hashed.** Argon2id at 64 MiB is a real cost, so
 *      the mismatch and the too-short case must be refused locally, in one line, with no hashing
 *      spent on them.
 *   3. **The offer is honest.** A device with the root offers the download, sealed with the
 *      registration passphrase the register screen handed in — the offer asks for nothing, because
 *      the passphrase was already typed — and a device without a root says so in one sentence and
 *      offers no button that could not work.
 *   4. **The open says one thing when it fails.** §182 forbids telling a wrong passphrase from a
 *      tampered file, so the sign-in screen's line is pinned word for word.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { containerFileName, credentialProblem, RESTORE_FAILED } from '../src/lib/account-file.js';
import { SaveAccountSheet } from '../src/components/save-account-sheet.js';

const ROOT = new Uint8Array(32).fill(0x5a);

test('a username becomes a findable account file name', () => {
  assert.equal(containerFileName('alice'), 'migo-alice.migo');
  assert.equal(containerFileName('Ada Lovelace'), 'migo-ada-lovelace.migo');
  assert.equal(containerFileName('Íñigo M!'), 'migo-igo-m.migo');
  assert.equal(containerFileName(''), 'migo-account.migo');
  assert.equal(containerFileName('---'), 'migo-account.migo');
  // No path separators or uppercase can survive the sanitisation.
  assert.ok(!containerFileName('../etc/passwd').includes('/'));
});

test('a typed credential pair is judged locally, before any Argon2id work is spent on it', () => {
  assert.equal(credentialProblem('correct horse', 'correct horse'), null);
  assert.equal(credentialProblem('12345678', '12345678'), null);
  // `?? ''`: a null (sealable) would fail the match, which is exactly the point — these three
  // lines each demand a refusal, and a credential the judge accepted cannot refuse anything.
  assert.match(credentialProblem('short', 'short') ?? '', /at least 8 characters/);
  assert.match(credentialProblem('one credential', 'another credential') ?? '', /do not match/);
  assert.match(credentialProblem('x'.repeat(1025), 'x'.repeat(1025)) ?? '', /too long/);
});

test('the save offer seals with the registration passphrase and asks for nothing else', () => {
  const markup = renderToStaticMarkup(
    <SaveAccountSheet
      username="alice"
      accountId="acct_1"
      root={ROOT}
      passphrase="correct-horse-battery-staple"
      onDone={() => undefined}
    />,
  );
  assert.ok(markup.includes('class="save-account"'), 'the offer body must render its shell');
  // The offer asks for no credential: the passphrase it seals with is the one the register screen
  // just collected, and a second secret to keep straight is a second secret to lose.
  assert.ok(
    !markup.includes('Recovery credential'),
    'the offer must not ask for a credential of its own',
  );
  assert.ok(
    !markup.includes('type="passphrase"'),
    'the offer must not collect any secret — the passphrase was already typed',
  );
  assert.ok(
    markup.includes('sealed with your passphrase'),
    'the offer must say which passphrase seals the file',
  );
  assert.ok(
    markup.includes('saved to this browser automatically'),
    'the offer must say the account is already saved locally',
  );
  assert.ok(markup.includes('Download key file'), 'the download control must name what it does');
  assert.ok(markup.includes('Continue'), 'continuing into the app must be an offered choice');
});

test('a device without the root gets the honest one-liner, not a dead button', () => {
  const markup = renderToStaticMarkup(
    <SaveAccountSheet
      username="alice"
      accountId="acct_1"
      root={null}
      passphrase="correct-horse-battery-staple"
      onDone={() => undefined}
    />,
  );
  assert.ok(markup.includes('does not hold the account root'), 'the honest line is missing');
  assert.ok(!markup.includes('Download key file'), 'no download control may render without a root');
});

test('the open-failure line is one honest sentence, pinned word for word', () => {
  // §182: a wrong passphrase, a tampered byte, and a foreign file are indistinguishable to the
  // reader, so the sign-in screen has exactly one line for all three.
  assert.equal(
    RESTORE_FAILED,
    'That passphrase does not open this file, or the file is not an account file.',
  );
});
