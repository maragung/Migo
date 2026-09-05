'use client';

/**
 * One window of the desktop shell.
 *
 * The shell is a desktop-OS metaphor: conversations and panels are not tabs in a pane but
 * windows on a desk — draggable by their title bar, resizable from their edges, minimizable to
 * the taskbar, maximizable to the desk's full extent, and stacked in a z-order that the last
 * click wins. This component is that window's chrome: the teal gloss title bar with its
 * min/max/close controls (desktop only), the pointer-capture drag, and the e/s/se resize
 * handles.
 *
 * On a phone the same component is the full-bleed surface below the tab strip: no title bar
 * (the strip's tab already names it and closes it), no drag, no resize — `mobileFullscreen`
 * says which world it is in, and the position comes from the `.mtab-window` class rather than
 * the window's own x/y.
 */

import { useCallback, useRef, useState } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent, ReactNode } from 'react';

import { Icon } from './icons.js';

/** The resize floor: a window smaller than this stops being a window. */
const MIN_W = 340;
const MIN_H = 240;

export type RetroWindowProps = {
  /** The title the bar carries (the taskbar's tab carries the same). */
  title: string;
  /** The desk position; the shell's cascade chose it when the window was minted. */
  x: number;
  y: number;
  /** The z-order slot; the shell's focus counter owns it. */
  z: number;
  /** Whether this window is the focused one (the active title bar is full-strength). */
  active: boolean;
  /** Pixels, or a CSS size for the non-resizable kinds. */
  width: number | string;
  height: number | string;
  /** A minimized window renders nothing; the taskbar's tab is what remains of it. */
  minimized?: boolean;
  /** Phone layout: full-bleed, no chrome, positioned by the stylesheet. */
  mobileFullscreen?: boolean;
  /** Which edge the desk's taskbar occupies, so a maximized window clears it. */
  taskbarPos?: 'top' | 'bottom';
  onFocus: () => void;
  onMinimize: () => void;
  onClose: () => void;
  onMove: (x: number, y: number) => void;
  children: ReactNode;
};

export function RetroWindow(p: RetroWindowProps): ReactNode {
  const dragRef = useRef<{ dx: number; dy: number } | null>(null);
  const resizeRef = useRef<{
    mode: 'e' | 's' | 'se';
    sw: number;
    sh: number;
    sx: number;
    sy: number;
  } | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const numeric = typeof p.width === 'number' && typeof p.height === 'number';
  const [size, setSize] = useState<{ w: number; h: number } | null>(
    numeric ? { w: p.width as number, h: p.height as number } : null,
  );
  const [maximized, setMaximized] = useState(false);

  const onPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLElement>): void => {
      // The controls in the bar are not drag handles; they opt out with the marker.
      if ((event.target as HTMLElement).closest('[data-nodrag]') !== null) {
        return;
      }
      p.onFocus();
      if (p.mobileFullscreen === true || maximized) {
        return;
      }
      const rect = rootRef.current?.getBoundingClientRect();
      if (!rect) {
        return;
      }
      dragRef.current = { dx: event.clientX - rect.left, dy: event.clientY - rect.top };
      event.currentTarget.setPointerCapture(event.pointerId);
    },
    [p, maximized],
  );

  const onPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLElement>): void => {
      const drag = dragRef.current;
      if (!drag) {
        return;
      }
      // Clamped to the desk: a window can hang off an edge but never leave it entirely.
      const nx = Math.min(Math.max(event.clientX - drag.dx, -40), window.innerWidth - 80);
      const ny = Math.min(Math.max(event.clientY - drag.dy, 0), window.innerHeight - 80);
      p.onMove(nx, ny);
    },
    [p],
  );

  const onPointerUp = useCallback((): void => {
    dragRef.current = null;
  }, []);

  const onResizeDown = useCallback(
    (mode: 'e' | 's' | 'se') =>
      (event: ReactPointerEvent<HTMLDivElement>): void => {
        event.preventDefault();
        event.stopPropagation();
        p.onFocus();
        if (!size) {
          return;
        }
        resizeRef.current = { mode, sw: size.w, sh: size.h, sx: event.clientX, sy: event.clientY };
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
      },
    [p, size],
  );

  const onResizeMove = useCallback((event: ReactPointerEvent<HTMLDivElement>): void => {
    const r = resizeRef.current;
    if (!r) {
      return;
    }
    const maxW = window.innerWidth - 30;
    const maxH = window.innerHeight - 44;
    setSize((prev) => {
      if (!prev) {
        return prev;
      }
      let w = prev.w;
      let h = prev.h;
      if (r.mode === 'e' || r.mode === 'se') {
        w = Math.min(Math.max(r.sw + (event.clientX - r.sx), MIN_W), maxW);
      }
      if (r.mode === 's' || r.mode === 'se') {
        h = Math.min(Math.max(r.sh + (event.clientY - r.sy), MIN_H), maxH);
      }
      return { w, h };
    });
  }, []);

  const onResizeUp = useCallback((): void => {
    resizeRef.current = null;
  }, []);

  if (p.minimized === true) {
    return null;
  }

  const isMobile = p.mobileFullscreen === true;
  const canResize = numeric && !isMobile && !maximized;
  const tbTop = (p.taskbarPos ?? 'bottom') === 'top';
  const style: CSSProperties = isMobile
    ? { zIndex: 300 + p.z }
    : maximized
      ? {
          left: 0,
          top: tbTop ? 34 : 0,
          width: '100vw',
          height: 'calc(100vh - 34px)',
          zIndex: p.z,
        }
      : { left: p.x, top: p.y, width: size?.w, height: size?.h, zIndex: p.z };

  return (
    <div
      ref={rootRef}
      className={`win-frame win-draggable${isMobile ? ' mtab-window' : ''}${p.active ? '' : ' win-inactive'}`}
      style={style}
      onMouseDown={p.onFocus}
    >
      {/* The title bar — the desk's, never the phone's: the strip's tab already names the
          window there, and closing it is the tab's X. */}
      {isMobile ? null : (
        <div
          className="gloss-title win-titlebar"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
        >
          <span className="win-title">{p.title}</span>
          <span className="win-controls" data-nodrag>
            <button
              type="button"
              aria-label="Minimize window"
              title="Minimize"
              className="win-ctl"
              onClick={p.onMinimize}
            >
              <Icon name="minimize" size={14} />
            </button>
            <button
              type="button"
              aria-label={maximized ? 'Restore window size' : 'Maximize window'}
              title={maximized ? 'Restore' : 'Maximize'}
              className="win-ctl"
              onClick={() => setMaximized((m) => !m)}
            >
              <Icon name={maximized ? 'restore' : 'maximize'} size={maximized ? 13 : 14} />
            </button>
            <button
              type="button"
              aria-label="Close window"
              title="Close"
              className="win-ctl win-ctl-close"
              onClick={p.onClose}
            >
              <Icon name="close" size={14} />
            </button>
          </span>
        </div>
      )}
      <div className="win-content">{p.children}</div>
      {canResize ? (
        <>
          <div
            className="rz-handle rz-e"
            onPointerDown={onResizeDown('e')}
            onPointerMove={onResizeMove}
            onPointerUp={onResizeUp}
          />
          <div
            className="rz-handle rz-s"
            onPointerDown={onResizeDown('s')}
            onPointerMove={onResizeMove}
            onPointerUp={onResizeUp}
          />
          <div
            className="rz-handle rz-se"
            onPointerDown={onResizeDown('se')}
            onPointerMove={onResizeMove}
            onPointerUp={onResizeUp}
          />
        </>
      ) : null}
    </div>
  );
}
