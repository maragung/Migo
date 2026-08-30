'use client';

/**
 * The context menu: the one right-click (desktop) and long-press (mobile) surface.
 *
 * Desktop opens a small floating menu at the pointer; mobile opens the same items as a bottom
 * sheet in thumb reach — one component, one contract, two placements, exactly like the bottom
 * sheet's own dual placement. Items are declared, not drawn: the caller names the actions, the
 * menu renders them with its own spacing, icons, and keyboard order, so every context menu in
 * the app reads the same.
 *
 * Long-press is detected here (a 450ms hold without a scroll or a second touch) so every row
 * that offers a context menu gets the gesture for free; right-click arrives through the
 * browser's own `contextmenu` event, which this component prevents from also opening the
 * browser's menu.
 */

import { useCallback, useEffect, useRef } from 'react';
import type {
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
  ReactNode,
} from 'react';

import { BottomSheet } from './bottom-sheet.js';
import { Icon } from './icons.js';
import type { IconName } from './icons.js';

/** One action a context menu offers. */
export interface ContextAction {
  id: string;
  label: string;
  icon: IconName;
  /** Whether the action is destructive (drawn in the danger colour). */
  danger?: boolean;
  onRun: () => void;
}

/** How long a touch must hold to be a long-press, without moving past the slop. */
const LONG_PRESS_MS = 450;

/** How far a touch may drift before the hold stops being a press. */
const LONG_PRESS_SLOP_PX = 8;

/**
 * The pointer-down props a row needs to offer a long-press menu.
 *
 * Spread onto the row's own element alongside `onContextMenu`; both gestures route to the same
 * {@link open} callback with the coordinates (a floating menu on desktop, a sheet on touch).
 */
export function useContextMenu(open: (at: { x: number; y: number; touch: boolean }) => void): {
  onPointerDown: (event: ReactPointerEvent<HTMLElement>) => void;
  onPointerMove: (event: ReactPointerEvent<HTMLElement>) => void;
  onPointerUp: (event: ReactPointerEvent<HTMLElement>) => void;
  onPointerCancel: (event: ReactPointerEvent<HTMLElement>) => void;
  onContextMenu: (event: ReactMouseEvent<HTMLElement>) => void;
} {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const start = useRef<{ x: number; y: number } | null>(null);

  const cancel = useCallback((): void => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    start.current = null;
  }, []);

  const onPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLElement>): void => {
      if (event.pointerType !== 'touch') {
        return;
      }
      cancel();
      start.current = { x: event.clientX, y: event.clientY };
      const { x, y } = start.current;
      timer.current = setTimeout(() => {
        timer.current = null;
        start.current = null;
        open({ x, y, touch: true });
      }, LONG_PRESS_MS);
    },
    [cancel, open],
  );

  // A drift past the slop (the user is scrolling) or a second finger cancels the hold.
  const onPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLElement>): void => {
      if (start.current === null) {
        return;
      }
      const dx = event.clientX - start.current.x;
      const dy = event.clientY - start.current.y;
      if (Math.hypot(dx, dy) > LONG_PRESS_SLOP_PX) {
        cancel();
      }
    },
    [cancel],
  );

  const onPointerUp = useCallback(cancel, [cancel]);
  const onPointerCancel = useCallback(cancel, [cancel]);

  const onContextMenu = useCallback(
    (event: ReactMouseEvent<HTMLElement>): void => {
      event.preventDefault();
      // Right-click is a pointer gesture by definition; the sheet path belongs to long-press.
      open({ x: event.clientX, y: event.clientY, touch: false });
    },
    [open],
  );

  return {
    onPointerDown,
    onPointerMove,
    onPointerUp,
    onPointerCancel,
    onContextMenu,
  };
}

/**
 * The rendered menu: floating at the pointer on desktop, a bottom sheet on touch.
 *
 * @param at Where the gesture happened (the floating menu's anchor; the sheet ignores it).
 * @param title The sheet's accessible name on touch.
 * @param actions The declared items, in order.
 * @param onClose Called after an item runs, on backdrop tap, or on Escape.
 */
export function ContextMenu({
  at,
  title,
  actions,
  onClose,
}: {
  at: { x: number; y: number; touch: boolean };
  title: string;
  actions: ReadonlyArray<ContextAction>;
  onClose: () => void;
}): ReactNode {
  const items = (
    <div className="context-menu-items">
      {actions.map((action) => (
        <button
          key={action.id}
          type="button"
          className={`context-menu-item ${action.danger === true ? 'context-menu-danger' : ''}`}
          onClick={() => {
            action.onRun();
            onClose();
          }}
        >
          <span className="context-menu-icon" aria-hidden="true">
            <Icon name={action.icon} size={20} />
          </span>
          {action.label}
        </button>
      ))}
    </div>
  );

  // A touch gesture gets the thumb-reachable sheet; a pointer gets the floating menu.
  if (at.touch) {
    return (
      <BottomSheet title={title} onClose={onClose}>
        {items}
      </BottomSheet>
    );
  }
  // The clamps keep the menu on screen; without a window (a static render) the raw point stands.
  const left = typeof window === 'undefined' ? at.x : Math.min(at.x, window.innerWidth - 220);
  const top =
    typeof window === 'undefined'
      ? at.y
      : Math.min(at.y, window.innerHeight - 40 - actions.length * 44);
  return (
    <>
      <div className="context-menu-backdrop" onClick={onClose} />
      <div className="context-menu" role="menu" aria-label={title} style={{ left, top }}>
        {items}
      </div>
    </>
  );
}

/** Closes the menu on Escape — mounted with the menu, unmounted with it. */
export function useEscape(onClose: () => void): void {
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
}
