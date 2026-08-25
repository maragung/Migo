import type { ReactNode } from 'react';

/** A simple loading spinner. */
export function Spinner(): ReactNode {
  return <span className="spinner" role="status" aria-label="Loading" />;
}

/** A spinner centered in the full available height, with an optional caption below it. */
export function FullSpinner({ label }: { label?: string }): ReactNode {
  return (
    <div className="full-spinner">
      <Spinner />
      {label ? <p className="muted">{label}</p> : null}
    </div>
  );
}
