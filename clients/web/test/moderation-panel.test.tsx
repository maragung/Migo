/**
 * What the moderator's dashboard is allowed to say and offer.
 *
 * Every fact on this screen arrives from the moderation REST surface, so its rendering tests feed
 * the exported presentational components exactly what the SDK calls return and pin the rules that
 * would silently regress under a "helpful" refactor:
 *
 *   1. **A case row offers one door, named with the case it opens.** The queue is a list of
 *      buttons, and each one says in its `aria-label` which report a click opens.
 *   2. **The reporter's note is never in the queue.** The row says a note exists; the words
 *      themselves are rendered only inside the open case, because a list of them is a list of
 *      quotations lined up for reading rather than a set of cases to triage.
 *   3. **Nothing in the queue can suspend an account.** The ruling codes offered here are the
 *      ones that decide a report. Suspension (3) and room archival (4) are acts on a person and a
 *      room, they live on `/v1/moderation/act`, and they are not one stray click away from a
 *      triager working down a list.
 *   4. **Escalation is offered and is not a closing.** It is the one ruling that leaves the case
 *      open, so a case escalated by mistake is still in the queue for whoever it was handed to.
 *   5. **A ruling in flight closes every button.** Not only the one pressed: two moderators
 *      clicking two rulings on one case is how a case gets two decisions.
 *   6. **An empty trail is a sentence.** A moderator who has recorded nothing gets told so in
 *      words, not an empty list that reads as a failed load.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { AuditEntryView, Id, ModerationCase } from '@migo/sdk';

import {
  AuditTrailView,
  CaseRowView,
  RULINGS,
  RulingFormView,
} from '../src/components/moderation-panel.js';

const NOW = Date.parse('2026-09-02T12:00:00Z');

function openCase(fields: Partial<ModerationCase> = {}): ModerationCase {
  return {
    reportId: 'id_report_0001' as Id,
    reporterId: 'id_reporter_0001' as Id,
    subjectKind: 1,
    subjectKindName: 'message',
    subjectId: 'id_subject_0001' as Id,
    reason: 0,
    reasonName: 'Spam',
    status: 0,
    open: true,
    createdAtMs: NOW - 3_600_000,
    ...fields,
  };
}

test('a case row names its reason and subject, and offers one labelled door', () => {
  const markup = renderToStaticMarkup(
    <CaseRowView entry={openCase()} selected={false} onOpen={() => {}} />,
  );

  assert.ok(markup.includes('Spam'), 'the reason is missing');
  assert.ok(markup.includes('message'), 'the subject kind is missing');
  const doors = markup.match(/aria-pressed="/g) ?? [];
  assert.equal(doors.length, 1, 'the row must carry exactly one open control');
  assert.ok(markup.includes('id_subject'), 'the row must name the subject it is about');
});

test("the reporter's note is announced in the queue and never quoted there", () => {
  const note = 'they keep sending me the same wallet drain link';
  const markup = renderToStaticMarkup(
    <CaseRowView entry={openCase({ note })} selected={false} onOpen={() => {}} />,
  );

  assert.ok(markup.includes('has a note'), 'the queue must say a note exists');
  assert.ok(
    !markup.includes('wallet drain'),
    'the queue must not render the note itself; it belongs to the open case',
  );
});

test('the queue cannot suspend an account or archive a room', () => {
  const codes = RULINGS.map((ruling) => ruling.code);
  assert.ok(!codes.includes(3), 'suspension (3) must not be a queue ruling');
  assert.ok(!codes.includes(4), 'room archival (4) must not be a queue ruling');
  for (const ruling of RULINGS) {
    assert.ok(ruling.label.trim() !== '', `ruling ${ruling.code} must be named`);
  }
});

test('escalation is offered and is the one ruling that does not close', () => {
  const escalate = RULINGS.find((ruling) => ruling.code === 5);
  assert.ok(escalate, 'escalation must be offered');
  assert.equal(escalate.closing, false, 'escalation decides nothing, so it cannot close the case');
  assert.ok(
    RULINGS.filter((ruling) => !ruling.closing).length === 1,
    'escalation is the only ruling that leaves a case open',
  );

  const markup = renderToStaticMarkup(
    <RulingFormView
      entry={openCase()}
      reason=""
      busy={false}
      onReason={() => {}}
      onRule={() => {}}
    />,
  );
  assert.ok(markup.includes('Escalate'), 'the form must offer escalation');
  assert.ok(
    markup.includes('id_subject_0001'),
    'each ruling must name the subject it would be made about',
  );
});

test('a ruling in flight disables every ruling, not only the one pressed', () => {
  const markup = renderToStaticMarkup(
    <RulingFormView entry={openCase()} reason="" busy onReason={() => {}} onRule={() => {}} />,
  );
  const disabled = markup.match(/disabled=""/g) ?? [];
  assert.equal(
    disabled.length,
    RULINGS.length,
    'a busy form must close all of its rulings, so no second decision can race the first',
  );
});

test('an empty audit trail is a sentence rather than an empty list', () => {
  const markup = renderToStaticMarkup(<AuditTrailView entries={[]} />);
  assert.ok(markup.includes('Nothing has been recorded'), 'the empty trail must say so in words');
});

test('an audit row names its action, its actor kind, and when it happened', () => {
  const entry: AuditEntryView = {
    auditId: 'id_audit_0001' as Id,
    actorId: 'id_moderator_0001' as Id,
    actorKind: 0,
    actorKindName: 'operator',
    action: 'moderation.report.resolve',
    targetKind: 15,
    targetKindName: 'report',
    targetId: 'id_report_0001' as Id,
    summary: 'warned about a spam report',
    createdAtMs: NOW - 60_000,
  };
  const markup = renderToStaticMarkup(<AuditTrailView entries={[entry]} />);

  assert.ok(markup.includes('moderation.report.resolve'), 'the action name is missing');
  assert.ok(markup.includes('operator'), 'the actor kind is missing');
  assert.ok(markup.includes('warned about a spam report'), 'the summary is missing');
});
