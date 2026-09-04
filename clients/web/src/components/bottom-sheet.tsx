'use client';

/**
 * The bottom sheet: the mobile context surface.
 *
 * On a phone, a modal dialog is a desktop metaphor — it lands wherever it lands and demands a
 * pointer. A bottom sheet is the thumb's surface: it rises from the bottom edge, keeps its
 * content in one-handed reach, and dismisses on backdrop tap or swipe-down intent (the drag
 * handle is the affordance; Escape and focus order still work for keyboards and screen readers).
 *
 * On tablet and desktop widths the same component renders as a small centred dialog instead —
 * one component, one contract, two placements. The sheet owns a backdrop (inert to scrolls
 * behind it), traps nothing (the browser handles focus order through the DOM order here), and
 * closes itself on Escape.
 */

import { useEffect, useRef } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';

import { Icon } from './icons.js';

export function BottomSheet({
  title,
  onClose,
  children,
  variant = 'plain',
}: {
  /** Announced as the dialog's accessible name. */
  title: string;
  /** Called on backdrop tap, close button, or Escape. */
  onClose: () => void;
  /**
   * The auth variant dresses the sheet as the auth screens' glass — the cyan gradient behind,
   * the frosted card, white ink. The one sheet that follows a registration follows the card it
   * follows: a white panel rising off a darkened gradient reads as a system dialog that lost its
   * way, not as the register screen's own last step.
   */
  variant?: 'plain' | 'auth';
  children: ReactNode;
}): ReactNode {
  const sheetRef = useRef<HTMLDivElement>(null);

  // Escape closes — the one keyboard affordance a sheet must carry.
  useEffect(() => {
    function onKey(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        onClose();
      }
    }
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
    };
  }, [onClose]);

  return (
    <div
      className={`sheet-backdrop${variant === 'auth' ? ' sheet-backdrop-auth' : ''}`}
      onClick={onClose}
    >
      <div
        ref={sheetRef}
        className={`sheet${variant === 'auth' ? ' sheet-auth' : ''}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="sheet-handle" aria-hidden="true" />
        <header className="sheet-head">
          <h2 className="sheet-title">{title}</h2>
          <button
            type="button"
            className="icon-btn"
            aria-label="Close"
            onClick={onClose}
            onKeyDown={(event: ReactKeyboardEvent<HTMLButtonElement>) => {
              if (event.key === 'Escape') {
                event.stopPropagation();
                onClose();
              }
            }}
          >
            <Icon name="close" size={20} />
          </button>
        </header>
        <div className="sheet-body">{children}</div>
      </div>
    </div>
  );
}
