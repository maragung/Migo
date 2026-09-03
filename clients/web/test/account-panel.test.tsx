/**
 * What the "My Account" panel is allowed to say about the account's identity, email, passphrase,
 * and `.migo` key file — the four account-level surfaces Settings handed over.
 *
 * The panel's presentational halves are exported as controlled components over plain data, so the
 * rules that would silently regress under a "helpful" refactor are pinned by feeding those views
 * exactly what the container owns, with no live client:
 *
 *   1. **Identity is read-only, and says so.** The username can never change (§182), so the panel
 *      states that plainly rather than offering an edit that would only ever error; the public id
 *      is shown beside it as the shareable handle.
 *   2. **Email is written, never read back.** There is no "what is my email" call, so the field
 *      never claims to show the current address, and a plainly-broken value is kept off the wire
 *      by {@link isLikelyEmail} before a request is spent on it.
 *   3. **A passphrase change is not the end of the story.** After it succeeds the form stops
 *      offering the submit and states the one thing left to act on — the saved `.migo` file still
 *      opens with the *old* passphrase — offering a fresh file only where this device holds the
 *      root to reseal.
 *   4. **The key file is offered only where it can be produced.** A device without the account
 *      root has no key file to download, so the panel says so and offers no button that could not
 *      work — the honest no-root state, pinned here so it cannot quietly become a dead control.
 *
 * The full panel is rendered once under a minimal context double (the same shape app-shell.test.tsx
 * and calls.test.tsx feed), with `client: null` so it takes the no-root branch; `renderToStaticMarkup`
 * runs no effects, so the profile lookup never fires and identity falls back to its resolving state.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import {
  AccountIdentityView,
  AccountPanel,
  EmailFormView,
  KeyFileFormView,
  MIN_PASSPHRASE_LENGTH,
  PassphraseFormView,
  isLikelyEmail,
} from '../src/components/account-panel.js';
import { MigoContext } from '../src/lib/migo/provider.js';

/** How many `disabled` attributes the markup carries — a submit gate is a disabled button. */
function disabledCount(markup: string): number {
  return (markup.match(/disabled/g) ?? []).length;
}

// --- isLikelyEmail: the local typo gate, not RFC validation -----------------------------------

test('isLikelyEmail accepts an ordinary address and trims surrounding space', () => {
  assert.equal(isLikelyEmail('ada@migo.app'), true);
  assert.equal(isLikelyEmail('  ada@migo.app  '), true, 'surrounding whitespace must be trimmed');
  assert.equal(isLikelyEmail('a@b.co'), true);
});

test('isLikelyEmail rejects the obvious typos before a request is spent', () => {
  assert.equal(isLikelyEmail(''), false, 'empty is not an address');
  assert.equal(isLikelyEmail('ada'), false, 'no @');
  assert.equal(isLikelyEmail('ada@migo'), false, 'no dotted domain');
  assert.equal(isLikelyEmail('ada@@migo.app'), false, 'a doubled @ is not an address');
  assert.equal(isLikelyEmail('ada mia@migo.app'), false, 'an embedded space is not an address');
  assert.equal(
    isLikelyEmail(`${'x'.repeat(250)}@migo.app`),
    false,
    'an over-long value is refused',
  );
});

// --- AccountIdentityView: read-only identity, with the immutability note ----------------------

test('the identity view shows @username, the public id, and states the username is permanent', () => {
  const markup = renderToStaticMarkup(
    <AccountIdentityView username="ada" publicId="MGO-ABCD1234" />,
  );

  assert.ok(markup.includes('@ada'), 'the username is shown with its @');
  assert.ok(
    markup.includes('MGO-ABCD1234'),
    'the public id is the shareable handle, shown beside it',
  );
  assert.ok(
    markup.includes('Your username can never be changed.'),
    'the immutability note is the whole point of a read-only identity',
  );
});

test('the identity view has an honest resting state before the profile resolves', () => {
  const markup = renderToStaticMarkup(<AccountIdentityView username={null} publicId={null} />);

  assert.ok(markup.includes('Your account'), 'an unresolved identity still names itself');
  assert.ok(!markup.includes('@'), 'no @handle is claimed before the username is known');
  assert.ok(
    !markup.includes('person-note'),
    'no public-id slot is drawn before there is a public id',
  );
});

// --- EmailFormView: the write-only field, gated on a plausible address -------------------------

test('the email field will not submit an implausible address', () => {
  const markup = renderToStaticMarkup(
    <EmailFormView
      value="ada"
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );

  assert.ok(markup.includes('Email address'), 'the field is labelled');
  assert.ok(
    markup.includes('Your current email is never shown here'),
    'the field is honest that it never reads the address back',
  );
  assert.equal(disabledCount(markup), 1, 'an implausible address leaves Save disabled');
});

test('the email field enables Save for a plausible address', () => {
  const markup = renderToStaticMarkup(
    <EmailFormView
      value="ada@migo.app"
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );

  assert.equal(disabledCount(markup), 0, 'a plausible address must not leave Save disabled');
});

test('the email field surfaces the server error and, on success, the saved hint', () => {
  const errored = renderToStaticMarkup(
    <EmailFormView
      value="ada@migo.app"
      busy={false}
      error="That address is already in use."
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(errored.includes('form-error'), 'a server error lands in the form-error slot');
  assert.ok(errored.includes('That address is already in use.'));

  const saved = renderToStaticMarkup(
    <EmailFormView
      value="ada@migo.app"
      busy={false}
      error={null}
      saved
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(saved.includes('Email saved.'), 'a saved address is acknowledged');
});

// --- PassphraseFormView: the three-field change, and its post-success key-file offer ------------

test('the passphrase form carries current, new, and confirm with the length rule stated', () => {
  const markup = renderToStaticMarkup(
    <PassphraseFormView
      current=""
      next=""
      confirm=""
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
      hasRoot
      refreshSealing={false}
      refreshError={null}
      refreshSaved={false}
      onDownloadUpdated={() => {}}
    />,
  );

  assert.ok(markup.includes('Current passphrase'));
  assert.ok(markup.includes('New passphrase'));
  assert.ok(markup.includes('Confirm new passphrase'));
  assert.ok(
    markup.includes(`At least ${MIN_PASSPHRASE_LENGTH} characters.`),
    'the new-passphrase length rule is stated where it is enforced',
  );
  assert.equal(disabledCount(markup), 1, 'an empty draft leaves Change passphrase disabled');
});

test('the passphrase form enables the change only for a complete, matching, long-enough draft', () => {
  const good = 'correct horse battery';
  const markup = renderToStaticMarkup(
    <PassphraseFormView
      current="old-secret"
      next={good}
      confirm={good}
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
      hasRoot
      refreshSealing={false}
      refreshError={null}
      refreshSaved={false}
      onDownloadUpdated={() => {}}
    />,
  );
  assert.equal(disabledCount(markup), 0, 'a complete matching draft must enable the change');

  const tooShort = renderToStaticMarkup(
    <PassphraseFormView
      current="old-secret"
      next="short"
      confirm="short"
      busy={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
      hasRoot
      refreshSealing={false}
      refreshError={null}
      refreshSaved={false}
      onDownloadUpdated={() => {}}
    />,
  );
  assert.equal(disabledCount(tooShort), 1, 'a too-short new passphrase leaves the change disabled');
});

test('after a change, the form warns the old key file is stale and offers a fresh one', () => {
  const markup = renderToStaticMarkup(
    <PassphraseFormView
      current="old-secret"
      next="correct horse battery"
      confirm="correct horse battery"
      busy={false}
      error={null}
      saved
      onChange={() => {}}
      onSubmit={() => {}}
      hasRoot
      refreshSealing={false}
      refreshError={null}
      refreshSaved={false}
      onDownloadUpdated={() => {}}
    />,
  );

  assert.ok(markup.includes('Passphrase changed.'));
  assert.ok(
    markup.includes('still opens with your old passphrase'),
    'the honest warning that the saved file lags the account is the point of the after-state',
  );
  assert.ok(markup.includes('Download updated key file'), 'a device with the root can reseal');
  assert.ok(
    !markup.includes('>Change passphrase<'),
    'the submit is gone once the change succeeded — the form does not re-offer it',
  );
});

test('after a change on a device without the root, the form says there is nothing to reseal', () => {
  const markup = renderToStaticMarkup(
    <PassphraseFormView
      current="old-secret"
      next="correct horse battery"
      confirm="correct horse battery"
      busy={false}
      error={null}
      saved
      onChange={() => {}}
      onSubmit={() => {}}
      hasRoot={false}
      refreshSealing={false}
      refreshError={null}
      refreshSaved={false}
      onDownloadUpdated={() => {}}
    />,
  );

  assert.ok(
    markup.includes('This device does not hold the account root'),
    'a device without the root must say so rather than offer a download it cannot produce',
  );
  assert.ok(
    !markup.includes('Download updated key file'),
    'no reseal button where there is no root',
  );
});

// --- KeyFileFormView: the download, gated by the recovery-credential judgement ------------------

test('the key-file form explains what the file is and gates the download on a valid credential', () => {
  const markup = renderToStaticMarkup(
    <KeyFileFormView
      credential="abc"
      confirm="abc"
      sealing={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );

  assert.ok(
    markup.includes('the only way to sign in on a new device'),
    'the file is explained as the one route onto a new device',
  );
  assert.ok(
    markup.includes('Re-downloading replaces your previous file'),
    'a re-download is framed honestly as a replacement',
  );
  assert.ok(
    markup.includes('needs at least 8 characters'),
    'a too-short credential shows the judgement before any Argon2id work',
  );
  assert.equal(disabledCount(markup), 1, 'a too-short credential leaves Download disabled');
});

test('the key-file form flags a credential mismatch and enables the download when it clears', () => {
  const mismatch = renderToStaticMarkup(
    <KeyFileFormView
      credential="battery-staple"
      confirm="battery-stapler"
      sealing={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(mismatch.includes('do not match'), 'a mismatch is named');
  assert.equal(disabledCount(mismatch), 1, 'a mismatch leaves Download disabled');

  const ok = renderToStaticMarkup(
    <KeyFileFormView
      credential="battery-staple"
      confirm="battery-staple"
      sealing={false}
      error={null}
      saved={false}
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.equal(disabledCount(ok), 0, 'a valid, matching credential must enable Download');

  const saved = renderToStaticMarkup(
    <KeyFileFormView
      credential="battery-staple"
      confirm="battery-staple"
      sealing={false}
      error={null}
      saved
      onChange={() => {}}
      onSubmit={() => {}}
    />,
  );
  assert.ok(saved.includes('Key file downloaded'), 'a completed download is acknowledged');
});

// --- The whole panel: its four sections, and the honest no-root key-file state -------------------

/** The panel under a ready-session context double with no live client — the no-root branch. */
function renderPanel(panel: ReactNode): string {
  return renderToStaticMarkup(
    <MigoContext.Provider
      value={{
        status: 'ready',
        connectionState: 'ready',
        accountId: 'acct_self' as Id,
        deviceId: null,
        error: null,
        resetNonce: 0,
        persistKeyStore: () => {},
        client: null,
        register: () => Promise.resolve(),
        loginWithFile: () => Promise.resolve(),
        logout: () => Promise.resolve(),
      }}
    >
      {panel}
    </MigoContext.Provider>,
  );
}

test('the panel renders its four account sections', () => {
  const markup = renderPanel(<AccountPanel />);

  assert.ok(markup.includes('>Account</h1>'), 'the panel is titled Account');
  assert.ok(markup.includes('>Account</h2>'), 'the identity section');
  assert.ok(markup.includes('>Email</h2>'), 'the email section');
  assert.ok(markup.includes('>Passphrase</h2>'), 'the passphrase section');
  assert.ok(markup.includes('>Account key file (.migo)</h2>'), 'the key-file section');

  assert.ok(markup.includes('Your username can never be changed.'), 'the identity note is present');
  assert.ok(markup.includes('Email address'), 'the email field is present');
  assert.ok(
    markup.includes('>Change passphrase</button>'),
    'the passphrase change opens its own screen',
  );
});

test('the panel takes the honest no-root state when the device holds no account root', () => {
  const markup = renderPanel(<AccountPanel />);

  assert.ok(
    markup.includes('This device does not hold the account root'),
    'a device without the root says so rather than offering a key file it cannot produce',
  );
  // The key-file section still draws a Download control, but disabled — a button that cannot work
  // is shown as unavailable, not hidden, so the reason is legible.
  assert.ok(
    markup.includes('Download key file'),
    'the control is present so the absence is legible',
  );
  assert.ok(markup.includes('disabled'), 'the no-root Download control is disabled');
});
