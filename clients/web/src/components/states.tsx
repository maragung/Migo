'use client';

/**
 * The shared loading, empty, and error states — one look everywhere.
 *
 * Every section can be waiting, finished-empty, or failed, and each of those states is a design
 * decision: a skeleton promises "structured data is coming" (a spinner promises only waiting), an
 * empty state says what would fill it and offers the next move, and an error states what failed
 * and offers the retry. These three components keep those promises identical across every panel,
 * so the app never invents a fourth way to say nothing happened.
 */

import type { ReactNode } from 'react';

import { Icon } from './icons.js';
import type { IconName } from './icons.js';

/** Placeholder rows shaped like the list that is loading, not a bare spinner. */
export function Skeleton({ rows = 3 }: { rows?: number }): ReactNode {
  return (
    <div className="skeleton-stack" aria-hidden="true">
      {Array.from({ length: rows }, (_, index) => (
        <div className="skeleton-row" key={index}>
          <div className="skeleton-avatar" />
          <div className="skeleton-lines">
            <div className="skeleton-line skeleton-w60" />
            <div className="skeleton-line skeleton-w40" />
          </div>
        </div>
      ))}
    </div>
  );
}

/** The finished-empty state: what is empty, and the move that fills it. */
export function EmptyState({
  icon,
  title,
  hint,
  action,
}: {
  icon: IconName;
  title: string;
  hint?: string;
  action?: ReactNode;
}): ReactNode {
  return (
    <div className="state-block">
      <span className="state-icon">
        <Icon name={icon} size={24} />
      </span>
      <p className="state-title">{title}</p>
      {hint ? <p className="state-hint">{hint}</p> : null}
      {action ? <div className="state-action">{action}</div> : null}
    </div>
  );
}

/** The failed state: what failed, in the user's words, with the retry offered. */
export function ErrorState({
  message,
  onRetry,
}: {
  message: string;
  onRetry?: () => void;
}): ReactNode {
  return (
    <div className="state-block state-error" role="alert">
      <span className="state-icon">
        <Icon name="shield" size={24} />
      </span>
      <p className="state-title">{message}</p>
      {onRetry ? (
        <div className="state-action">
          <button type="button" className="btn" onClick={onRetry}>
            Try again
          </button>
        </div>
      ) : null}
    </div>
  );
}
