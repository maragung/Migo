'use client';

/**
 * The presence and status control for the sidebar footer.
 *
 * Presence is a publish, not a store: the picker is fully controlled (the parent owns the current
 * state and performs the {@link PresenceDomain.setPresence} call), so the control renders the
 * truth it was handed and never optimistically redraws a presence the server refused.
 *
 * The four states offered are the ones the protocol's enum names for self-reporting; the status
 * line beside them is free text capped at 100 characters, the same bound the wire's profile
 * field carries.
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { PresenceState } from '@migo/sdk';

/** The longest custom status the picker accepts, matching the profile field's bound. */
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
 * The parent (the sidebar) performs the network call; this signature is the whole contract.
 */
export type PresenceChange = (state: PresenceState, status: string) => void;

/** The picker: a state dropdown, a status input, and the current status stated below. */
export function PresencePicker({
  state,
  status,
  onChange,
}: {
  /** The current presence state, as the parent holds it. */
  state: PresenceState;
  /** The current custom status, as the parent holds it. */
  status: string;
  /** Called with the next state and status when either control commits. */
  onChange: PresenceChange;
}): ReactNode {
  // The draft is local until it is committed: typing must not publish per keystroke.
  const [draft, setDraft] = useState(status);

  // A parent-side change (an initial load, another surface's publish) re-seeds the draft.
  useEffect(() => {
    setDraft(status);
  }, [status]);

  return (
    <div className="presence-picker">
      <label className="visually-hidden" htmlFor="presence-state">
        Presence
      </label>
      <select
        id="presence-state"
        className="input presence-select"
        value={state}
        onChange={(event) => onChange(stateOfValue(event.target.value), draft)}
        aria-label="Set presence"
      >
        {PRESENCE_OPTIONS.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
      <label className="visually-hidden" htmlFor="presence-status">
        Custom status
      </label>
      <input
        id="presence-status"
        type="text"
        className="input presence-status"
        value={draft}
        maxLength={STATUS_MAX_CHARS}
        placeholder="Set a status…"
        onChange={(event) => setDraft(event.target.value)}
        onBlur={() => {
          if (draft !== status) {
            onChange(state, draft);
          }
        }}
        aria-label="Custom status"
      />
      <div className="presence-current" aria-live="polite">
        {status.trim().length > 0 ? status : null}
      </div>
    </div>
  );
}
