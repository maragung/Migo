'use client';

/**
 * The phone's tab strip — the taskbar's replacement below the PC breakpoint.
 *
 * No window chrome survives a phone's width, so the strip is where windows live: the home
 * navigation tabs come first (Main leading whenever Chat List Mode is on, then Friends, Rooms,
 * and Feed — only Feed closable; Main, Friends, and Rooms are permanent and ship without an X),
 * a hairline divides them from one tab per open window, and every window tab carries its own
 * close. The strip scrolls horizontally when it must, with chevron arrows that fade in only on
 * the side that still hides tabs, and the active tab keeps itself scrolled into view.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { Icon } from './icons.js';
import { Sheet, SheetAction } from './intent-sheet.js';
import { KIND_ICON } from './window-types.js';
import type { WinState } from './window-types.js';

/**
 * The home views the strip navigates between. `main` is the Chat List Mode home screen — the
 * conversation list the mode puts where the tabbed layout keeps its three views — and it leads
 * {@link MOBILE_NAV_ORDER} so the strip offers it as the Main tab whenever the mode is on. The
 * tabbed layout keeps no conversation list, so its strip offers the other three alone (see
 * {@link TABBED_NAV_ORDER}); the view header's back control is the mode's other way back to the
 * list, one step from whichever tabbed view is on screen.
 */
export type MobileNavTab = 'main' | 'friends' | 'rooms' | 'feed';

/**
 * The home tabs in strip order: Main first, then Friends, then Rooms, then Feed. Chat List
 * Mode's strip offers all four; the tabbed layout offers the order without its Main (see
 * {@link TABBED_NAV_ORDER}).
 */
export const MOBILE_NAV_ORDER: readonly MobileNavTab[] = ['main', 'friends', 'rooms', 'feed'];

/** The tabbed layout's home tabs: the strip's order minus Main, which has no screen there. */
export const TABBED_NAV_ORDER: readonly MobileNavTab[] = MOBILE_NAV_ORDER.filter(
  (tab) => tab !== 'main',
);

/** The home tabs' names and icons. */
export const MOBILE_NAV_META: Readonly<
  Record<MobileNavTab, { label: string; icon: 'friends' | 'rooms' | 'space' | 'chats' }>
> = {
  main: { label: 'Main', icon: 'chats' },
  friends: { label: 'Friends', icon: 'friends' },
  rooms: { label: 'Rooms', icon: 'rooms' },
  feed: { label: 'Feed', icon: 'space' },
};

/** Only Feed closes from its X; Main, Friends, and Rooms are the home itself. */
const NAV_CLOSEABLE: Readonly<Record<MobileNavTab, boolean>> = {
  feed: true,
  friends: false,
  main: false,
  rooms: false,
};

/** The unread badge's ceiling: more than nine reads as "many", not as arithmetic. */
const BADGE_CAP = 9;

function Badge({ count }: { count: number }): ReactNode {
  if (count <= 0) {
    return null;
  }
  return <span className="mtab-badge">{count > BADGE_CAP ? `${BADGE_CAP}+` : count}</span>;
}

export function MobileTabBar({
  windows,
  activeId,
  unreadWin,
  navTab,
  hiddenNavs,
  navUnread,
  chatList,
  onSelectNav,
  onCloseNav,
  onReopenNav,
  onSelectWindow,
  onCloseWindow,
}: {
  windows: readonly WinState[];
  activeId: string | null;
  /** Unread message counts per window id, the shell's own attention signal. */
  unreadWin: Readonly<Record<string, number>>;
  navTab: MobileNavTab;
  /** Home tabs the user closed from their X (reopenable from the "+" sheet). */
  hiddenNavs: readonly MobileNavTab[];
  /** Unread counts for the home tabs themselves. */
  navUnread: Readonly<Record<MobileNavTab, number>>;
  /** Whether Chat List Mode is on — the mode whose strip leads with the Main tab. */
  chatList: boolean;
  onSelectNav: (tab: MobileNavTab) => void;
  onCloseNav: (tab: MobileNavTab) => void;
  onReopenNav: (tab: MobileNavTab) => void;
  onSelectWindow: (id: string) => void;
  onCloseWindow: (id: string) => void;
}): ReactNode {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [canLeft, setCanLeft] = useState(false);
  const [canRight, setCanRight] = useState(false);
  const [reopenOpen, setReopenOpen] = useState(false);

  const updateArrows = useCallback((): void => {
    const el = scrollRef.current;
    if (!el) {
      return;
    }
    setCanLeft(el.scrollLeft > 2);
    setCanRight(el.scrollLeft + el.clientWidth < el.scrollWidth - 2);
  }, []);

  useEffect(() => {
    updateArrows();
    const el = scrollRef.current;
    if (!el) {
      return;
    }
    const observer = new ResizeObserver(updateArrows);
    observer.observe(el);
    if (el.firstElementChild !== null) {
      observer.observe(el.firstElementChild);
    }
    window.addEventListener('resize', updateArrows);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', updateArrows);
    };
  }, [updateArrows, windows, activeId]);

  // Keep the active tab visible: the strip scrolls, the person should not have to know that.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) {
      return;
    }
    const active = el.querySelector<HTMLElement>('[data-tab-active="true"]');
    if (!active) {
      return;
    }
    const strip = el.getBoundingClientRect();
    const tab = active.getBoundingClientRect();
    const left = tab.left - strip.left + el.scrollLeft;
    const right = left + tab.width;
    const pad = 34; // the arrow overlays' width
    if (left - pad < el.scrollLeft) {
      el.scrollTo({ left: Math.max(0, left - pad), behavior: 'smooth' });
    } else if (right + pad > el.scrollLeft + el.clientWidth) {
      el.scrollTo({ left: Math.max(0, right - el.clientWidth + pad), behavior: 'smooth' });
    }
  }, [activeId, navTab, windows.length]);

  function nudge(direction: 1 | -1): void {
    const el = scrollRef.current;
    if (!el) {
      return;
    }
    el.scrollBy({ left: direction * Math.max(140, el.clientWidth * 0.65), behavior: 'smooth' });
  }

  const atHome = activeId === null;
  // The strip's home tabs follow the navigation mode: Chat List Mode's strip leads with Main —
  // the conversation list is one of the mode's screens, and its tab is the way back to it —
  // while the tabbed layout, which keeps no conversation list, offers the three it always has.
  const order = chatList ? MOBILE_NAV_ORDER : TABBED_NAV_ORDER;
  const visibleNavs = order.filter((tab) => !hiddenNavs.includes(tab));

  return (
    <div
      className="taskbar taskbar-top mtab-bar"
      role="tablist"
      aria-label="Navigation and open windows"
    >
      <div ref={scrollRef} className="mtab-scroll" onScroll={updateArrows}>
        {visibleNavs.map((id) => {
          const active = atHome && navTab === id;
          const unread = navUnread[id] ?? 0;
          return (
            <div
              key={id}
              data-tab-active={active}
              role="tab"
              aria-selected={active}
              tabIndex={0}
              className={`task-btn mtab-tab${active ? ' task-btn-active' : ''}`}
              title={`${MOBILE_NAV_META[id].label} — home view`}
              onClick={() => onSelectNav(id)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  onSelectNav(id);
                }
              }}
            >
              <button
                type="button"
                className="mtab-open"
                onClick={(event) => {
                  event.stopPropagation();
                  onSelectNav(id);
                }}
                aria-label={`Open ${MOBILE_NAV_META[id].label}`}
              >
                <Icon name={MOBILE_NAV_META[id].icon} size={15} />
                {MOBILE_NAV_META[id].label}
                <Badge count={unread} />
              </button>
              {NAV_CLOSEABLE[id] ? (
                <button
                  type="button"
                  aria-label={`Close ${MOBILE_NAV_META[id].label} tab`}
                  title={`Close ${MOBILE_NAV_META[id].label}`}
                  className="mtab-x"
                  onClick={(event) => {
                    event.stopPropagation();
                    onCloseNav(id);
                  }}
                >
                  <Icon name="close" size={12} />
                </button>
              ) : null}
            </div>
          );
        })}

        {hiddenNavs.length > 0 ? (
          <button
            type="button"
            className="task-btn mtab-plus"
            onClick={() => setReopenOpen(true)}
            aria-label="Reopen closed tabs"
            title="Reopen closed tabs"
          >
            <Icon name="plus" size={16} />
          </button>
        ) : null}

        <span className="mtab-divider" aria-hidden="true" />

        {windows.map((w) => {
          const active = w.id === activeId && !w.minimized;
          const unread = unreadWin[w.id] ?? 0;
          return (
            <div
              key={w.id}
              data-tab-active={active}
              role="tab"
              aria-selected={active}
              tabIndex={0}
              className={`task-btn mtab-tab${active ? ' task-btn-active' : ''}`}
              title={w.title}
              onClick={() => onSelectWindow(w.id)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  onSelectWindow(w.id);
                }
              }}
            >
              <button
                type="button"
                className="mtab-open"
                onClick={(event) => {
                  event.stopPropagation();
                  onSelectWindow(w.id);
                }}
                aria-label={`Open ${w.title}`}
              >
                <span
                  className={`task-dot${w.minimized ? ' task-dot-min' : ''}`}
                  aria-hidden="true"
                />
                <Icon name={KIND_ICON[w.kind]} size={13} className="mtab-kind-icon" />
                <span className="mtab-title">{w.title}</span>
                <Badge count={unread} />
              </button>
              <button
                type="button"
                aria-label={`Close ${w.title}`}
                title={`Close ${w.title}`}
                className="mtab-x"
                onClick={(event) => {
                  event.stopPropagation();
                  onCloseWindow(w.id);
                }}
              >
                <Icon name="close" size={12} />
              </button>
            </div>
          );
        })}
      </div>

      {canLeft ? (
        <button
          type="button"
          className="mtab-arrow mtab-arrow-l"
          onClick={() => nudge(-1)}
          aria-label="Scroll tabs to the left"
        >
          <Icon name="chevron-left" size={22} />
        </button>
      ) : null}
      {canRight ? (
        <button
          type="button"
          className="mtab-arrow mtab-arrow-r"
          onClick={() => nudge(1)}
          aria-label="Scroll tabs to the right"
        >
          <Icon name="chevron-right" size={22} />
        </button>
      ) : null}

      <Sheet open={reopenOpen} onClose={() => setReopenOpen(false)} title="Reopen tab">
        {order
          .filter((tab) => hiddenNavs.includes(tab))
          .map((id) => (
            <SheetAction
              key={id}
              icon={MOBILE_NAV_META[id].icon}
              label={MOBILE_NAV_META[id].label}
              onClick={() => {
                setReopenOpen(false);
                onReopenNav(id);
              }}
            />
          ))}
        <div className="sheet-tail" />
      </Sheet>
    </div>
  );
}
