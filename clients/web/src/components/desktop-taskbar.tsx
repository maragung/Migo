'use client';

/**
 * The desk's taskbar.
 *
 * Thirty-four pixels of deep teal at the screen's edge (which edge is a stored choice — the
 * dock toggle beside the clock flips it and remembers): one button per open window with the
 * state of its dot (green for on-top, pale for minimized), the realtime connection's health,
 * the session's running time, and the clock. The window buttons restore, focus, or minimize their
 * window in one click — the same toggle the reference's taskbar performs.
 *
 * The connection chip took the seat the $MIG balance used to hold (the balance moved up into the
 * contacts window's me bar, above the alerts and the account door): the transport's health is
 * the fact a desk glance wants, in the same live vocabulary the list windows' footer band wears,
 * so the two never disagree.
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { ConnectionStatusDot } from './connection-status-dot.js';
import { Icon } from './icons.js';
import { MigoDiamond } from './migo-brand.js';
import { KIND_LABEL } from './window-types.js';
import type { WinState } from './window-types.js';

/** The clock's tick: fast enough to keep the minutes honest, slow enough to be idle-cheap. */
const CLOCK_TICK_MS = 15_000;

export function Taskbar({
  windows,
  activeId,
  onlineSince,
  onToggle,
  onRequestLogout,
  accountName,
  pos,
  onTogglePos,
}: {
  windows: readonly WinState[];
  activeId: string | null;
  /** When this session started (epoch ms); the timer counts up from it. */
  onlineSince: number;
  onToggle: (id: string) => void;
  onRequestLogout: () => void;
  /** Whose session the logout button would end. */
  accountName: string;
  pos: 'bottom' | 'top';
  onTogglePos: () => void;
}): ReactNode {
  const [now, setNow] = useState<Date | null>(null);

  // The clock starts on mount (never during a static render) and drifts no further than a
  // minute between ticks.
  useEffect(() => {
    const first = setTimeout(() => setNow(new Date()), 0);
    const tick = setInterval(() => setNow(new Date()), CLOCK_TICK_MS);
    return () => {
      clearTimeout(first);
      clearInterval(tick);
    };
  }, []);

  const mins = now !== null ? Math.max(0, Math.floor((now.getTime() - onlineSince) / 60_000)) : 0;

  return (
    <div
      className={`taskbar${pos === 'top' ? ' taskbar-top' : ''}`}
      role="toolbar"
      aria-label="Taskbar"
    >
      <span className="taskbar-brand">
        <MigoDiamond size={16} />
        <span className="taskbar-brand-word">Migo</span>
      </span>

      <div className="taskbar-tasks retro-scroll">
        {windows.map((w) => {
          const active = w.id === activeId && !w.minimized;
          return (
            <button
              key={w.id}
              type="button"
              className={`task-btn${active ? ' task-btn-active' : ''}`}
              onClick={() => onToggle(w.id)}
              title={w.title}
            >
              <span
                className={`task-dot${w.minimized ? ' task-dot-min' : ''}`}
                aria-hidden="true"
              />
              <span className="task-btn-label">{w.title}</span>
              <span className="task-btn-kind">{KIND_LABEL[w.kind]}</span>
            </button>
          );
        })}
      </div>

      {/* The realtime connection's health, in the footer band's own vocabulary. */}
      <span className="task-chip" title="Connection">
        <ConnectionStatusDot />
      </span>
      <span className="task-chip" title="Session time">
        <Icon name="clock" size={12} />
        {mins < 60 ? `${mins}m` : `${Math.floor(mins / 60)}h${mins % 60}m`}
      </span>

      <button
        type="button"
        className="task-tool"
        onClick={onTogglePos}
        title={pos === 'bottom' ? 'Move taskbar to top' : 'Move taskbar to bottom'}
        aria-label={pos === 'bottom' ? 'Move taskbar to top' : 'Move taskbar to bottom'}
      >
        <Icon name={pos === 'bottom' ? 'arrow-up' : 'arrow-down'} size={15} />
      </button>

      <button
        type="button"
        className="task-tool task-logout"
        onClick={onRequestLogout}
        title={`Log out ${accountName}`}
      >
        <Icon name="signout" size={12} />
        Logout
      </button>
      <span className="task-chip task-clock" aria-label="Clock">
        <Icon name="clock" size={12} />
        {now !== null
          ? now.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: false })
          : '--:--'}
      </span>
    </div>
  );
}
