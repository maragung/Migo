/**
 * The post-call quality rating: what a user can say about a call they have just left, and how that
 * becomes a wire frame.
 *
 * Section 180 asks for a rating given *after* a call ends, with four verdicts — Excellent, Good,
 * Average and Poor — and an optional note of what was wrong: audio, video, connection, or a call
 * that dropped. Two properties of the requirement drive everything here.
 *
 * The first is that the note is **optional and not exclusive**. A call can have had bad audio and a
 * bad connection at once, so the note is a set rather than a choice, and on the wire that set is a
 * mask rather than a list: a client that knows a fifth problem can set a fifth bit without this
 * build having to learn a new structure, and this build counts the four bits it names and drops the
 * rest rather than guessing what they meant.
 *
 * The second is that a rating is a **user's own statement**, of the same kind as the setup time and
 * loss numbers already carried by `CALL_STATS`: this server never sees the media, so it cannot check
 * any of it, and the frame that carries it is Droppable at cost one. So the rating rides the stats
 * frame rather than earning an opcode, and it is sent at the moment the call ends while the call row
 * still exists — a row the calls store never deletes.
 *
 * A rating is never call content. It is four words and four tick boxes, and that is the whole of
 * what leaves the device.
 */

import type { CallStats } from '@migo/sdk';
import { CallRating } from '@migo/sdk';

/** The verdicts a user can give, best first, which is the order they are offered in. */
export const CALL_RATING_CHOICES: readonly CallRating[] = [
  CallRating.Excellent,
  CallRating.Good,
  CallRating.Average,
  CallRating.Poor,
];

/** One verdict as a word, for the button that offers it. */
export function callRatingLabel(rating: CallRating): string {
  switch (rating) {
    case CallRating.Excellent:
      return 'Excellent';
    case CallRating.Good:
      return 'Good';
    case CallRating.Average:
      return 'Average';
    case CallRating.Poor:
      return 'Poor';
    default:
      // Unknown is what a build that does not know a value decodes to, and is never sent. A prompt
      // cannot offer it, so a caller that asks for its label has made a programming error rather
      // than found a state, and the honest label is the one that says so.
      return 'Unrated';
  }
}

/**
 * A problem a user can attach to a rating, as the schema's own four bits.
 *
 * Named rather than numbered at the call site because the bit positions are a wire fact: the schema
 * fixes audio at 0, video at 1, connection at 2 and dropped at 3, server decoders are written
 * against those, and a client that renumbered them would be reporting a different call's problems
 * without any error to show for it. The mask is built from the schema and never re-typed.
 */
export type CallIssueKind = 'audio' | 'video' | 'connection' | 'dropped';

/** The problems, in the schema's bit order. */
export const CALL_ISSUE_KINDS: readonly CallIssueKind[] = [
  'audio',
  'video',
  'connection',
  'dropped',
];

/** One problem as a word, for the toggle that offers it. */
export function callIssueLabel(issue: CallIssueKind): string {
  switch (issue) {
    case 'audio':
      return 'Audio problem';
    case 'video':
      return 'Video problem';
    case 'connection':
      return 'Connection problem';
    case 'dropped':
      return 'Call dropped';
    default:
      return issue;
  }
}

/**
 * The bit one problem occupies, matching the schema field's documented numbering.
 *
 * The switch is exhaustive over `CallIssueKind`, so a fifth problem cannot be added without this
 * function being made to answer for it — which is what keeps the mask and the schema in step.
 */
function callIssueBit(issue: CallIssueKind): bigint {
  switch (issue) {
    case 'audio':
      return 1n;
    case 'video':
      return 1n << 1n;
    case 'connection':
      return 1n << 2n;
    default:
      return 1n << 3n;
  }
}

/**
 * The mask for a set of problems, or undefined where none were named.
 *
 * Undefined rather than zero for the empty set, and the difference is the point: the field is
 * optional on the wire, so "the user named no problems" travels as an absent field, while a zero
 * would travel as a present field claiming to name nothing. The two decode the same way on this
 * build, but they are not the same statement, and only one of them is true of a user who ticked
 * nothing.
 */
export function callIssueMask(issues: readonly CallIssueKind[]): bigint | undefined {
  if (issues.length === 0) {
    return undefined;
  }
  let mask = 0n;
  for (const issue of issues) {
    mask |= callIssueBit(issue);
  }
  return mask;
}

/** The parts of a stats frame a rating fills in, which is all the call manager needs to send it. */
export type CallRatingReport = Pick<CallStats, 'rating' | 'issues'>;

/**
 * The frame a user's rating becomes, or null where there is nothing to send.
 *
 * No verdict is a real outcome — the prompt is dismissible, and a user who does not want to say
 * anything must not be made to say Good — so a null here is the caller's instruction to send
 * nothing at all, rather than to send an Unknown verdict.
 */
export function callRatingReport(
  rating: CallRating | null,
  issues: readonly CallIssueKind[],
): CallRatingReport | null {
  if (rating === null || rating === CallRating.Unknown) {
    return null;
  }
  const mask = callIssueMask(issues);
  return mask === undefined ? { rating } : { rating, issues: mask };
}
