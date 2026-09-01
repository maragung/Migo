/**
 * What the account-file surfaces are allowed to say and do.
 *
 * The container's own cryptography (Argon2id, XChaCha20-Poly1305, the cross-language vectors) is
 * pinned in `packages/crypto`; what is pinned here is everything the auth screens add around it:
 *
 *   1. **The file's name is findable.** A username with spaces or accents must still produce a
 *      `migo-….migo` a person can recognise and click, and an empty sanitisation must not produce
 *      a dotfile.
 *   2. **The credential is judged before it is hashed.** Argon2id at 64 MiB is a real cost, so
 *      the mismatch and the too-short case must be refused locally, in one line, with no hashing
 *      spent on them.
 *   3. **The offer is honest.** A device with the root offers the download; a device without one
 *      says so in one sentence and offers no button that could not work.
 *   4. **The restore says one thing when it fails.** §182 forbids telling a wrong credential from
 *      a tampered file, so the screen's line is pinned word for word.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { containerFileName, credentialProblem, RESTORE_FAILED } from '../src/lib/account-file.js';
import { RestoreAccountSheet } from '../src/components/restore-account-sheet.js';
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

test('a recovery credential is judged locally, before any Argon2id work is spent on it', () => {
  assert.equal(credentialProblem('correct horse', 'correct horse'), null);
  assert.equal(credentialProblem('12345678', '12345678'), null);
  // `?? ''`: a null (sealable) would fail the match, which is exactly the point — these three
  // lines each demand a refusal, and a credential the judge accepted cannot refuse anything.
  assert.match(credentialProblem('short', 'short') ?? '', /at least 8 characters/);
  assert.match(credentialProblem('one credential', 'another credential') ?? '', /do not match/);
  assert.match(credentialProblem('x'.repeat(1025), 'x'.repeat(1025)) ?? '', /too long/);
});

test('the save offer shows the credential fields and the download, and says what it is for', () => {
  const markup = renderToStaticMarkup(
    <SaveAccountSheet username="alice" accountId="acct_1" root={ROOT} onDone={() => undefined} />,
  );
  assert.ok(markup.includes('class="save-account"'), 'the offer body must render its shell');
  assert.ok(markup.includes('Recovery credential'), 'the credential field must be labelled');
  assert.ok(markup.includes('Confirm recovery credential'), 'the confirm field must be labelled');
  assert.ok(
    markup.includes('not your Migo password'),
    'the credential must be distinguished from the account password',
  );
  assert.ok(
    markup.includes('saved to this browser automatically'),
    'the offer must say the account is already saved locally',
  );
  assert.ok(
    markup.includes('Download account file'),
    'the download control must name what it does',
  );
  assert.ok(markup.includes('Later'), 'declining must be an offered choice');
});

test('a device without the root gets the honest one-liner, not a dead button', () => {
  const markup = renderToStaticMarkup(
    <SaveAccountSheet username="alice" accountId="acct_1" root={null} onDone={() => undefined} />,
  );
  assert.ok(markup.includes('does not hold the account root'), 'the honest line is missing');
  assert.ok(
    !markup.includes('Download account file'),
    'no download control may render without a root',
  );
});

test('the restore sheet asks for the file, the credential, and nothing else', () => {
  const markup = renderToStaticMarkup(
    <RestoreAccountSheet onRestored={() => undefined} onCancel={() => undefined} />,
  );
  assert.ok(markup.includes('type="file"'), 'the file picker must be present');
  assert.ok(
    markup.includes('accept=".migo,application/octet-stream"'),
    'the picker must prefer .migo files',
  );
  assert.ok(markup.includes('Recovery credential'), 'the credential field must be labelled');
  assert.ok(markup.includes('>Restore</button>'), 'the restore control must be present');
  assert.ok(!markup.includes(RESTORE_FAILED), 'no failure line before anything was attempted');
});

test('the restore failure line is one honest sentence, pinned word for word', () => {
  // §182: a wrong credential, a tampered byte, and a foreign file are indistinguishable to the
  // reader, so the screen has exactly one line for all three.
  assert.equal(
    RESTORE_FAILED,
    'That credential does not open this file, or the file is not an account file.',
  );
});
