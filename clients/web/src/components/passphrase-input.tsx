'use client';

/**
 * A passphrase field that is masked by default, with a show/hide toggle beside it.
 *
 * The mask is the browser's own (`type="password"`, one dot per character); the toggle swaps to
 * `type="text"` for as long as the user asks to see. Nothing about the value, the toggle, or the
 * timing is ever stored or logged — showing a passphrase is a reading aid the user grants
 * themselves, one render at a time.
 *
 * The field keeps the shapes its callers already pass (`autoComplete`, `minLength`, `required`,
 * an accessible label), so swapping an existing `<input type="passphrase">` for it changes the
 * masking and adds the eye — and nothing else. (`type="passphrase"` was never a real input type:
 * browsers render it as plain text, which is how passphrases ended up on screen unmasked.)
 */

import { useState } from 'react';
import type { ChangeEvent, ReactNode } from 'react';

import { Icon } from './icons.js';

export function PassphraseInput({
  value,
  onChange,
  autoComplete,
  ariaLabel,
  minLength,
  required = false,
  className,
}: {
  value: string;
  onChange: (event: ChangeEvent<HTMLInputElement>) => void;
  autoComplete?: string;
  /** The field's accessible name, for the screens whose label wraps several fields. */
  ariaLabel?: string;
  minLength?: number;
  required?: boolean;
  /** Extra classes for the input itself, e.g. the shared `input` style. */
  className?: string;
}): ReactNode {
  const [shown, setShown] = useState(false);
  return (
    <span className="passphrase-field">
      <input
        type={shown ? 'text' : 'password'}
        className={className}
        value={value}
        onChange={onChange}
        autoComplete={autoComplete}
        minLength={minLength}
        required={required}
        aria-label={ariaLabel}
      />
      <button
        type="button"
        className="passphrase-toggle"
        onClick={() => setShown((open) => !open)}
        aria-label={shown ? 'Hide passphrase' : 'Show passphrase'}
        aria-pressed={shown}
        title={shown ? 'Hide passphrase' : 'Show passphrase'}
      >
        <Icon name={shown ? 'eye-off' : 'eye'} size={16} />
      </button>
    </span>
  );
}
