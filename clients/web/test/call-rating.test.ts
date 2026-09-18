/**
 * The post-call rating as a value: what a user's four verdicts and four tick boxes become on the
 * wire.
 *
 * Section 180 makes the verdict a choice of four and the note optional, and both of those are
 * decisions this module could get wrong in ways no screen would show: a note that travelled as a
 * zero would claim the user named no problems when they named none, and a verdict sent for a user
 * who dismissed the prompt would put a word in their mouth. The tests here pin those two, and pin
 * the bit numbering, because the schema fixes it and a client that renumbered would report a
 * different call's problems with nothing to show for it.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { CallRating } from '@migo/sdk';

import {
  CALL_ISSUE_KINDS,
  CALL_RATING_CHOICES,
  callIssueLabel,
  callIssueMask,
  callRatingLabel,
  callRatingReport,
} from '../src/lib/migo/call-rating.js';

test('the four verdicts are offered best first, and Unknown is not offered at all', () => {
  assert.deepEqual(CALL_RATING_CHOICES, [
    CallRating.Excellent,
    CallRating.Good,
    CallRating.Average,
    CallRating.Poor,
  ]);
  assert.deepEqual(CALL_RATING_CHOICES.map(callRatingLabel), [
    'Excellent',
    'Good',
    'Average',
    'Poor',
  ]);
  // Unknown is what a build that does not know a verdict decodes to and is never sent, so a prompt
  // cannot offer it — its label exists only so a caller that asks has an answer that says so.
  assert.equal(callRatingLabel(CallRating.Unknown), 'Unrated');
});

test('the four problems are the schema’s four bits, in its order', () => {
  assert.deepEqual(CALL_ISSUE_KINDS, ['audio', 'video', 'connection', 'dropped']);
  assert.deepEqual(CALL_ISSUE_KINDS.map(callIssueLabel), [
    'Audio problem',
    'Video problem',
    'Connection problem',
    'Call dropped',
  ]);

  // The numbering is a wire fact and not a local choice: the schema documents audio at bit 0, video
  // at 1, connection at 2 and dropped at 3, and every decoder is written against it.
  assert.equal(callIssueMask(['audio']), 1n);
  assert.equal(callIssueMask(['video']), 2n);
  assert.equal(callIssueMask(['connection']), 4n);
  assert.equal(callIssueMask(['dropped']), 8n);

  // A set rather than a choice: a call can have had bad audio and a bad connection at once, and the
  // mask is the union of what was ticked.
  assert.equal(callIssueMask(['audio', 'connection']), 5n);
  assert.equal(callIssueMask(['dropped', 'audio', 'video', 'connection']), 15n);
  // Order does not matter and a repeat is not a second problem.
  assert.equal(callIssueMask(['connection', 'audio']), callIssueMask(['audio', 'connection']));
  assert.equal(callIssueMask(['audio', 'audio']), 1n);
});

test('naming no problem is an absent field rather than a zero', () => {
  // The difference is a statement about the user and not a bytes count: the field is optional, so
  // "the user ticked nothing" travels as no field, while a zero would travel as a field claiming to
  // name nothing. Only the first is true of a user who ticked nothing.
  assert.equal(callIssueMask([]), undefined);
});

test('a verdict with no note is a complete answer, and no verdict sends nothing', () => {
  // The commonest rating is a verdict on its own, and it must not carry an invented note.
  assert.deepEqual(callRatingReport(CallRating.Good, []), { rating: CallRating.Good });

  // The note is optional and travels only when it says something.
  assert.deepEqual(callRatingReport(CallRating.Poor, ['audio']), {
    rating: CallRating.Poor,
    issues: 1n,
  });
  assert.deepEqual(callRatingReport(CallRating.Average, ['video', 'dropped']), {
    rating: CallRating.Average,
    issues: 10n,
  });

  // A user who dismissed the prompt has said nothing, and the frame that would say otherwise is one
  // this refuses to build. Unknown is the same refusal from the other side: it is never sent, so a
  // caller that passes it gets no frame rather than a frame claiming the user chose "Unrated".
  assert.equal(callRatingReport(null, []), null);
  assert.equal(callRatingReport(null, ['audio']), null);
  assert.equal(callRatingReport(CallRating.Unknown, []), null);
  assert.equal(callRatingReport(CallRating.Unknown, ['dropped']), null);
});
