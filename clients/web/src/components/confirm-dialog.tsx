'use client';

/**
 * The logout confirmation: a small window-frame modal.
 *
 * The reference design asks "log out?" in the same language the desktop speaks — a win-frame
 * with a gloss title bar, not a browser dialog — because the action closes every window on the
 * desk at once and deserves to look like it. Escape and a backdrop tap both answer "no".
 */

import { useEffect } from 'react';
import type { ReactNode } from 'react';

export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel,
  cancelLabel,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
}): ReactNode {
  useEffect(() => {
    if (!open) {
      return;
    }
    function onKey(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        onCancel();
      }
    }
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
    };
  }, [open, onCancel]);

  if (!open) {
    return null;
  }

  return (
    <div
      className="confirm-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onCancel();
        }
      }}
    >
      <div className="win-frame confirm-frame">
        <div className="gloss-title confirm-title">{title}</div>
        <div className="confirm-body">
          <p className="confirm-message">{message}</p>
          <div className="confirm-actions">
            <button type="button" className="btn" onClick={onCancel}>
              {cancelLabel ?? 'Cancel'}
            </button>
            <button type="button" className="btn btn-primary" onClick={onConfirm}>
              {confirmLabel ?? 'Confirm'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
