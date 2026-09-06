'use client';

/**
 * A direct conversation's verification surface: the pair safety numbers, per device the peer
 * publishes, and the words that make the numbers mean something (§47, §164).
 *
 * A safety number is only worth the comparison behind it, so the explanation is the desktop and
 * Android clients' own sentence — compare in a call or in person, and a mismatch means stop — rather
 * than a softer paraphrase. The numbers are monospaced so two strings that differ in one digit
 * differ *visibly*, which is the whole reason anyone reads them aloud.
 *
 * Everything here is a controlled view over the state {@link useSafety} reads: the panel never
 * fetches on its own, because the read that observes the peer's identities belongs to the
 * conversation window (it runs on open, not on panel open — a key change must be visible whether or
 * not the person ever looks at this panel, which is the entire warning). The acknowledgment button
 * exists only while a change is unacknowledged: clearing the warning is the person's act, never the
 * read's, and a button that offered to "acknowledge" an unchanged number would be teaching that the
 * word means nothing.
 */

import type { ReactNode } from 'react';

import type { PeerSafetyNumber } from '@/lib/migo/safety.js';

import { Spinner } from './spinner.js';

/** The sentence that turns a number into a check: what to compare, and what a mismatch means. */
export const SAFETY_EXPLANATION =
  'Compare this with the other person, in a call or in person. If it differs, stop and do not trust the conversation.';

/** What a changed number says on the row it belongs to. */
export const SAFETY_CHANGED_NOTE =
  "This device's identity key changed since this conversation last saw it.";

/** The warning the conversation itself carries while a change is unacknowledged. */
export const SAFETY_WARNING =
  "Your contact's identity key changed. Verify the safety number before trusting this conversation.";

/** The state a conversation's verification surface renders, as {@link useSafety} reads it. */
export interface SafetyState {
  /** The report, or `null` while the read is in flight — the honest state, never a guess. */
  numbers: PeerSafetyNumber[] | null;
  /** Why the read failed, when it did. */
  failure: string | null;
  /** Whether any peer device's identity changed since this conversation acknowledged it. */
  changed: boolean;
  /** Records every current fingerprint as the acknowledged one — the person's act. */
  acknowledge: () => void;
  /** Re-runs the read. */
  retry: () => void;
}

/**
 * The warning a changed key leaves on the conversation: visible on the thread itself, not only in a
 * panel a person may never open, with the one way through it — review the number.
 *
 * It does not block the conversation (messages still send and still decrypt, since a changed key is
 * also what an honest reinstall looks like); it refuses to let the change pass unremarked, which is
 * the entire requirement.
 */
export function SafetyWarningBannerView({ onReview }: { onReview: () => void }): ReactNode {
  return (
    <div className="safety-banner" role="alert">
      <p>{SAFETY_WARNING}</p>
      <button type="button" className="btn btn-ghost" onClick={onReview}>
        Review
      </button>
    </div>
  );
}

/**
 * One peer device's number. The device label appears only when there is more than one to tell
 * apart: the single-device case is the common one, and "Device ab12cd34" over a lone number is
 * chrome explaining itself.
 */
export function SafetyNumberRow({
  deviceId,
  number,
  changed,
  label,
}: {
  deviceId: string;
  number: string;
  changed: boolean;
  /** Whether the device label belongs on the row. */
  label: boolean;
}): ReactNode {
  return (
    <div className={changed ? 'safety-number changed' : 'safety-number'}>
      {label ? <div className="safety-device">Device {deviceId.slice(0, 8)}</div> : null}
      <div className="safety-digits">{number}</div>
      {changed ? <div className="safety-changed-note">{SAFETY_CHANGED_NOTE}</div> : null}
    </div>
  );
}

/**
 * The numbers and the words, over the state the conversation window reads.
 *
 * A failed read says so and offers the retry it owes, because a number shown before the read lands
 * would be a number invented on the spot, and a read that failed silently would be a verification
 * surface that quietly verifies nothing.
 */
export function SafetyPanelView({ safety }: { safety: SafetyState }): ReactNode {
  if (safety.failure !== null && safety.numbers === null) {
    return (
      <div className="safety-failure">
        <p className="form-error">{safety.failure}</p>
        <button type="button" className="btn btn-ghost" onClick={safety.retry}>
          Try again
        </button>
      </div>
    );
  }
  if (safety.numbers === null) {
    return (
      <div className="safety-loading">
        <Spinner />
      </div>
    );
  }
  const label = safety.numbers.length > 1;
  return (
    <div className="safety-panel">
      {safety.numbers.map((entry) => (
        <SafetyNumberRow
          key={entry.deviceId}
          deviceId={entry.deviceId}
          number={entry.number}
          changed={entry.changed}
          label={label}
        />
      ))}
      <p className="hint">{SAFETY_EXPLANATION}</p>
      {safety.changed ? (
        <button type="button" className="btn" onClick={safety.acknowledge}>
          I&apos;ve checked the new number
        </button>
      ) : null}
    </div>
  );
}

/**
 * The direct conversation's details drawer: the same place a room's and a group's ⓘ opens, carrying
 * the one detail a 1:1 has — the safety numbers the two people can compare.
 */
export function DirectInfoPanel({ safety }: { safety: SafetyState }): ReactNode {
  return (
    <div className="room-info" aria-label="Safety numbers">
      <h2 className="panel-heading">Safety numbers</h2>
      <SafetyPanelView safety={safety} />
    </div>
  );
}
