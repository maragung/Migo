'use client';

/**
 * Reporting something: the one dialog the four report surfaces open.
 *
 * Brief section 49 asks for a report about a user, a message, a room, or a bot, and the four differ
 * only in what they point at — so they share this dialog and pass a {@link ReportSubjectRef} naming
 * the target. Picking the *reason* is the only decision the reporter makes, which is why the reason
 * list is the whole body and the note is folded behind it: a reason code is what a moderator filters
 * the queue by, and a report that arrives with only a note cannot be triaged at all.
 *
 * # The dialog never shows what was reported
 *
 * The header names the subject in the reporter's own terms — "this message", a display name, "this
 * room" — and the body echoes no message text, no avatar, and no attachment. That is not squeamish:
 * a report dialog that quoted the offending content would be re-rendering it in a surface that has
 * no mute, no filter, and no way to look away, which is the opposite of what a person reporting
 * abuse needs. The moderator follows the pointer inside the app, where the normal controls apply.
 *
 * # The note carries a warning, not a licence
 *
 * The note is the reporter's own words and the server stores it on the report row, where staff read
 * it. The placeholder says so plainly rather than leaving a person to assume it is private — a
 * reporter who writes a home address into a note has told a moderator their home address, and the
 * dialog is the only place that can say so before it happens.
 *
 * # What the reporter is told afterwards
 *
 * Only that the report arrived. The wire answers `REPORT_CREATE` with a bare acknowledgement — see
 * the SDK's moderation domain for why — so there is no case number to show and no status to follow,
 * and this dialog does not invent one. A refusal (a rate limit, a subject the server will not take)
 * is surfaced in the server's own words via {@link friendlyError}, in place, without closing the
 * dialog, so the note the reporter typed is not lost to a retry.
 */

import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';

import { ReportReason, ReportSubject, REPORT_NOTE_MAX_LEN } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { Icon } from './icons.js';
import { Spinner } from './spinner.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

/**
 * What a report points at, plus the words the dialog uses to name it.
 *
 * `label` is the caller's phrasing and never quoted content: "this message", "this room", a display
 * name. It is what the header and the acknowledgement sentence say, so it has to read as the object
 * of "Report", e.g. "Report this message?" and "Thanks — this message has been reported."
 */
export interface ReportSubjectRef {
  kind: ReportSubject;
  id: Id;
  label: string;
}

/**
 * The reasons offered, in the order the dialog lists them.
 *
 * Exported because the *set* is a product decision a test can hold still — which reasons are a
 * menu item and which are not — and a rendering test cannot see it through a portal.
 *
 * A subset of {@link ReportReason}: the codes a person can actually judge for themselves. The rest
 * — {@link ReportReason.ChildSafety}, {@link ReportReason.SelfHarm}, {@link ReportReason.BotAbuse} —
 * are reachable through {@link ReportSubjectRef} callers that know which they mean (a bot surface
 * reports `BotAbuse`) and are deliberately not a menu item, because a reporter choosing between
 * "child safety" and "sexual content" in a list is being asked to make a legal distinction the
 * queue's own prioritisation should make instead.
 */
export interface ReportReasonOption {
  reason: ReportReason;
  label: string;
  hint: string;
}

export const REPORT_REASONS: readonly ReportReasonOption[] = [
  { reason: ReportReason.Spam, label: 'Spam', hint: 'Unwanted bulk messages or invites.' },
  {
    reason: ReportReason.Scam,
    label: 'Scam or fraud',
    hint: 'Trying to get money or details by deception.',
  },
  {
    reason: ReportReason.MaliciousLink,
    label: 'Malicious link',
    hint: 'A link to malware, phishing, or a page that steals sign-ins.',
  },
  {
    reason: ReportReason.Harassment,
    label: 'Harassment',
    hint: 'Threats or targeted abuse of a person.',
  },
  {
    reason: ReportReason.HateSpeech,
    label: 'Hate speech',
    hint: 'Hateful content aimed at a group.',
  },
  {
    reason: ReportReason.SexualContent,
    label: 'Sexual content',
    hint: 'Sexual content where it does not belong.',
  },
  { reason: ReportReason.Violence, label: 'Violence', hint: 'Graphic violence.' },
  {
    reason: ReportReason.Impersonation,
    label: 'Impersonation',
    hint: 'Pretending to be somebody else.',
  },
  { reason: ReportReason.Other, label: 'Something else', hint: 'None of the above.' },
];

export function ReportDialog({
  subject,
  onClose,
}: {
  /** What to report, or `null` when the dialog is closed. */
  subject: ReportSubjectRef | null;
  onClose: () => void;
}): ReactNode {
  const { client } = useMigo();
  const [reason, setReason] = useState<ReportReason>(ReportReason.Spam);
  const [note, setNote] = useState('');
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState(false);

  // A new subject is a new report: the previous pick, note, and outcome must not leak into it.
  // Keyed on the subject's identity rather than on `open`, because reporting two different messages
  // in a row must reset the form both times.
  const key = subject === null ? null : `${subject.kind}:${subject.id}`;
  useEffect(() => {
    setReason(ReportReason.Spam);
    setNote('');
    setError(null);
    setSent(false);
    setSending(false);
  }, [key]);

  useEffect(() => {
    if (subject === null) {
      return;
    }
    function onKey(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        onClose();
      }
    }
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
    };
  }, [subject, onClose]);

  const trimmed = useMemo(() => note.trim(), [note]);

  if (subject === null) {
    return null;
  }

  async function submit(): Promise<void> {
    if (client === null || sending) {
      return;
    }
    setSending(true);
    setError(null);
    try {
      await client.moderation.report(
        { kind: subject!.kind, id: subject!.id },
        reason,
        trimmed.length > 0 ? { note: trimmed } : {},
      );
      setSent(true);
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setSending(false);
    }
  }

  // Portaled to the body, like every other modal here: the desk stacks its own windows, so a
  // dialog rendered inside one could never be the topmost surface.
  return createPortal(
    <div
      className="confirm-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={`Report ${subject.label}`}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onClose();
        }
      }}
    >
      <div className="win-frame confirm-frame report-frame">
        <div className="gloss-title confirm-title">
          {sent ? 'Report sent' : `Report ${subject.label}`}
        </div>
        <div className="confirm-body">
          {sent ? (
            <>
              <p className="confirm-message">
                Thanks — {subject.label} has been reported. Our moderators will review it. You will
                not be told the outcome, and the person is not told who reported them.
              </p>
              <div className="confirm-actions">
                <button type="button" className="btn btn-primary" onClick={onClose}>
                  Close
                </button>
              </div>
            </>
          ) : (
            <>
              <p className="confirm-message">
                What is wrong with {subject.label}? This helps us send it to the right person.
              </p>
              <fieldset className="report-reasons">
                <legend className="report-legend">Reason</legend>
                {REPORT_REASONS.map((entry) => (
                  <label key={entry.reason} className="report-reason">
                    <input
                      type="radio"
                      name="report-reason"
                      value={entry.reason}
                      checked={reason === entry.reason}
                      onChange={() => setReason(entry.reason)}
                    />
                    <span className="report-reason-text">
                      <span className="report-reason-label">{entry.label}</span>
                      <span className="report-reason-hint">{entry.hint}</span>
                    </span>
                  </label>
                ))}
              </fieldset>
              <label className="report-note">
                <span className="report-note-label">Anything else? (optional)</span>
                <textarea
                  className="report-note-input"
                  value={note}
                  maxLength={REPORT_NOTE_MAX_LEN}
                  rows={3}
                  placeholder="Your own words. A moderator will read this, so do not include anything you would not want staff to see."
                  onChange={(event) => setNote(event.target.value)}
                />
                <span className="report-note-count">
                  {note.length}/{REPORT_NOTE_MAX_LEN}
                </span>
              </label>
              {error !== null ? (
                <p className="report-error" role="alert">
                  <Icon name="info" size={16} /> {error}
                </p>
              ) : null}
              <div className="confirm-actions">
                <button type="button" className="btn" onClick={onClose} disabled={sending}>
                  Cancel
                </button>
                <button
                  type="button"
                  className="btn btn-primary"
                  onClick={() => void submit()}
                  disabled={sending || client === null}
                >
                  {sending ? <Spinner /> : 'Send report'}
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
