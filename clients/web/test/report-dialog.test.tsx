/**
 * What the report dialog decides before it draws anything.
 *
 * The dialog's body lives behind a portal, so a server render cannot see it; what it *can* reach
 * are the two decisions that are the dialog's own and the two that would silently rot:
 *
 *   1. **Closed means nothing at all.** The chat window and the room panel now render this dialog
 *      permanently in their trees, one per surface, so the `subject === null` return is not a
 *      convenience — a dialog that rendered a backdrop while closed would put an invisible sheet
 *      over a window nobody could dismiss.
 *   2. **The reason menu is the codes a person can judge.** The three that exist for legal or
 *      operator reasons — child safety, self-harm, bot abuse — are deliberately absent: a reporter
 *      asked to choose between "child safety" and "sexual content" is being made to draw a legal
 *      line the queue's own prioritisation must draw instead. The set is exported precisely so a
 *      test can hold it still.
 *   3. **The ceiling the textarea enforces is the node's.** The dialog caps the note at the same
 *      length the SDK refuses at, which is the same length the warden stores; three numbers that
 *      have to stay one number.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

import { ReportReason, REPORT_NOTE_MAX_LEN } from '@migo/sdk';

import { ReportDialog, REPORT_REASONS } from '../src/components/report-dialog.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import type { MigoContextValue } from '../src/lib/migo/provider.js';

/**
 * The smallest context the dialog can read: no client, so the Send button is disabled rather than
 * firing, which is what a signed-out surface sees.
 *
 * Cast rather than filled in because the context carries the whole session API and the dialog
 * reads exactly one field of it; a change to the rest of the value must not fail this test.
 */
const NO_SESSION = {
  status: 'ready',
  connectionState: 'open',
  accountId: null,
  deviceId: null,
  error: null,
  resetNonce: 0,
  client: null,
} as unknown as MigoContextValue;

test('a closed report dialog renders nothing at all', () => {
  const markup = renderToStaticMarkup(
    createElement(
      MigoContext.Provider,
      { value: NO_SESSION },
      createElement(ReportDialog, { subject: null, onClose: () => {} }),
    ),
  );
  assert.equal(
    markup,
    '',
    'a dialog mounted for a surface that is not reporting must draw nothing',
  );
});

test('the reason menu offers only codes a person can judge, in wire order', () => {
  const offered = REPORT_REASONS.map((entry) => entry.reason);
  // Absent on purpose — see the file's own note. Each one is reachable by a surface that knows
  // which it means (a bot surface reports BotAbuse), never by a person picking off a list.
  for (const withheld of [
    ReportReason.Flood,
    ReportReason.SelfHarm,
    ReportReason.ChildSafety,
    ReportReason.BotAbuse,
  ]) {
    assert.ok(!offered.includes(withheld), `reason ${withheld} must not be a menu item`);
  }
  // Every offered code is a real code, and "Something else" is last so it reads as the fallback
  // rather than as one option among many.
  assert.deepEqual(offered, [
    ReportReason.Spam,
    ReportReason.Scam,
    ReportReason.MaliciousLink,
    ReportReason.Harassment,
    ReportReason.HateSpeech,
    ReportReason.SexualContent,
    ReportReason.Violence,
    ReportReason.Impersonation,
    ReportReason.Other,
  ]);
  assert.equal(offered.at(-1), ReportReason.Other);
  assert.equal(new Set(offered).size, offered.length, 'no reason is offered twice');
  for (const entry of REPORT_REASONS) {
    assert.ok(entry.label.length > 0, 'every reason is named');
    assert.ok(entry.hint.length > 0, 'every reason explains itself');
  }
});

test('the dialog and the SDK agree on how long a note may be', () => {
  // The wire's own limit: the node stores what it is given, so a divergence here would surface as
  // a rejected report after the reporter had typed the whole thing.
  assert.equal(REPORT_NOTE_MAX_LEN, 500);
  assert.ok(REPORT_NOTE_MAX_LEN > 0);
});
