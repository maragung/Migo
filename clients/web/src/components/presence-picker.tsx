'use client';

/**
 * The account's presence and status, as the two controls the profile banner carries.
 *
 * Presence is a publish, not a store: both controls are fully controlled (the banner owns the
 * current state and performs the {@link PresenceDomain.setPresence} call), so they render the
 * truth they were handed and never optimistically redraw a presence the server refused. They
 * are separate components because they live in separate places now — the state dropdown sits
 * beside the coin chip on the banner's right, the status input under the @username on its left
 * — but they share one module so the option list and the character bound stay one thing.
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { PresenceState } from '@migo/sdk';

/** The longest custom status the input accepts, matching the profile field's bound. */
export const STATUS_MAX_CHARS = 100;

/** The self-reportable states, in display order. */
const PRESENCE_OPTIONS: ReadonlyArray<{ value: PresenceState; label: string }> = [
  { value: PresenceState.Online, label: 'Online' },
  { value: PresenceState.Away, label: 'Away' },
  { value: PresenceState.Busy, label: 'Busy' },
  { value: PresenceState.Invisible, label: 'Invisible' },
];

/**
 * The state a select value names, resolved through the offered options rather than a numeric
 * cast: a value the list never offered (a stale DOM, a newer node) falls back to Online rather
 * than publishing a state number this build cannot name.
 */
function stateOfValue(value: string): PresenceState {
  const found = PRESENCE_OPTIONS.find((option) => String(option.value) === value);
  return found !== undefined ? found.value : PresenceState.Online;
}

/**
 * Publishes one presence change: the state, and the status line that rides with it.
 *
 * The banner performs the network call; this signature is the whole contract.
 */
export type PresenceChange = (state: PresenceState, status: string) => void;

/** The state dropdown: which of the four self-reportable states the account is in. */
export function PresenceSelect({
  state,
  onStateChange,
}: {
  /** The current presence state, as the parent holds it. */
  state: PresenceState;
  /** Called with the next state; the status is the parent's to carry through unchanged. */
  onStateChange: (next: PresenceState) => void;
}): ReactNode {
  return (
    <select
      className="banner-presence-select"
      value={state}
      onChange={(event) => onStateChange(stateOfValue(event.target.value))}
      aria-label="Set presence"
      title="Presence"
    >
      {PRESENCE_OPTIONS.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  );
}

/**
 * The status input: free text capped at 100 characters, committed on blur or Enter so typing
 * never publishes per keystroke.
 */
export function StatusInput({
  state,
  status,
  onChange,
}: {
  /** The current presence state, passed through to the publish when the draft commits. */
  state: PresenceState;
  /** The current custom status, as the parent holds it. */
  status: string;
  /** Called with the state and the committed draft when the input commits. */
  onChange: PresenceChange;
}): ReactNode {
  // The draft is local until it is committed: typing must not publish per keystroke.
  const [draft, setDraft] = useState(status);

  // A parent-side change (an initial load, another surface's publish) re-seeds the draft.
  useEffect(() => {
    setDraft(status);
  }, [status]);

  function commit(): void {
    if (draft !== status) {
      onChange(state, draft);
    }
  }

  return (
    <input
      type="text"
      className="banner-status-input"
      value={draft}
      maxLength={STATUS_MAX_CHARS}
      placeholder="New here! Say hi :)"
      onChange={(event) => setDraft(event.target.value)}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === 'Enter') {
          event.currentTarget.blur();
        }
      }}
      aria-label="Custom status"
    />
  );
}
