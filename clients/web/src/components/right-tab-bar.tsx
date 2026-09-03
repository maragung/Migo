'use client';

/**
 * The right pane's tab bar: everything the pane can show, as tabs.
 *
 * The pane has one mode now, not two: a persistent Home chip — the pane's resting content, the
 * thing it shows when nothing is open — followed by one closable chip per open thing
 * (a conversation, a secondary panel the banner menu or a deep link opened). The system tabs'
 * content (Friends, Rooms, Games, Feed) never appears here: those live in the left panel, so
 * the pane cannot draw the same list twice. There is no "menu panel" to switch back to:
 * closing a chip falls through to the next one, and closing the last one leaves Home, which is
 * exactly the fallback the pane owes an empty state. The chevrons scroll the row without moving
 * the page; a compact back chevron — the single-column story only, where the pane covers the
 * whole screen — hands the screen back to the left lists without closing anything.
 */

import { useRef } from 'react';
import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { Icon } from './icons.js';

/** The closable things the right pane can hold besides its resting Home. */
export type RightTabKind =
  'chat' | 'notifications' | 'search' | 'wallet' | 'profile' | 'account' | 'settings' | 'admins';

/**
 * The pane's active target: `'feed'` (the resting tab, which the bar renders itself) or the id
 * of one of the open tabs. A plain `string` on purpose — the ids are the layout's to mint
 * (`chat:<conversation>` for a thread, the kind itself for one-per-kind tabs).
 */
export type RightPaneTab = string;

/** One open thing as the bar knows it: which tab, what to call the chip. */
export interface RightTabChip {
  /** The tab's identity: `chat:<conversation>` for a thread, the kind itself for one-per-kind tabs. */
  id: string;
  kind: RightTabKind;
  /** The conversation the tab shows, when the kind is a chat. */
  conversationId?: Id;
  /** What the chip says. */
  title: string;
}

/** How far one chevron click scrolls the row: about three chips, in either direction. */
const SCROLL_STEP_PX = 240;

/**
 * The bar itself.
 *
 * @param tabs The open closable tabs, in open order; the Home chip the bar renders itself.
 * @param active The tab the pane is showing — `'feed'` or a tab id.
 * @param onSelect Activates a chip (the Home chip included).
 * @param onClose Closes a chip; the owner decides what showing it next means.
 * @param onBackToLists Hands the screen back to the left lists (the single-column story only).
 */
export function RightTabBar({
  tabs,
  active,
  onSelect,
  onClose,
  onBackToLists,
}: {
  tabs: readonly RightTabChip[];
  active: RightPaneTab;
  onSelect: (id: RightPaneTab) => void;
  onClose: (id: string) => void;
  onBackToLists: () => void;
}): ReactNode {
  const rowRef = useRef<HTMLDivElement | null>(null);

  const scrollRow = (direction: 'left' | 'right'): void => {
    rowRef.current?.scrollBy({
      left: direction === 'left' ? -SCROLL_STEP_PX : SCROLL_STEP_PX,
      behavior: 'smooth',
    });
  };

  return (
    <nav className="chat-tab-bar" aria-label="Open panels">
      {/* The single-column way back: compact, icon-only, and gone on a PC — closing tabs is the
          PC's way home, and the left lists are always on screen beside the pane. */}
      <button
        type="button"
        className="chat-back"
        onClick={onBackToLists}
        aria-label="Back to the lists"
        title="Back to the lists"
      >
        <Icon name="back" size={16} />
      </button>
      <button
        type="button"
        className="chat-scroll"
        aria-label="Scroll tabs left"
        onClick={() => scrollRow('left')}
      >
        <Icon name="back" size={16} />
      </button>
      <div className="chat-tabs" ref={rowRef}>
        {/* The resting chip: always first, never closable — an empty pane still owes a home. */}
        <button
          type="button"
          className={`chat-tab${active === 'feed' ? ' active' : ''}`}
          aria-current={active === 'feed' ? 'page' : undefined}
          onClick={() => onSelect('feed')}
        >
          <span className="tab-chip-label">Home</span>
        </button>
        {tabs.map((tab) => (
          <button
            key={tab.id}
            type="button"
            className={`chat-tab${active === tab.id ? ' active' : ''}`}
            aria-current={active === tab.id ? 'page' : undefined}
            onClick={() => onSelect(tab.id)}
            title={tab.title}
          >
            <span className="tab-chip-label">{tab.title}</span>
            <span
              className="tab-close"
              role="button"
              tabIndex={-1}
              aria-label={`Close ${tab.title}`}
              title={`Close ${tab.title}`}
              onClick={(event) => {
                event.stopPropagation();
                onClose(tab.id);
              }}
            >
              <Icon name="close" size={16} />
            </span>
          </button>
        ))}
      </div>
      <button
        type="button"
        className="chat-scroll"
        aria-label="Scroll tabs right"
        onClick={() => scrollRow('right')}
      >
        <Icon name="chevron-right" size={16} />
      </button>
    </nav>
  );
}

/**
 * The one-window mode's slim bar: one title, one close, no chips.
 *
 * The display setting that drops the right-pane tabs still needs a way to say what the pane is
 * showing and to leave it — the bar carries the single open panel's name as a plain label (it is
 * not a control; there is nothing to switch it with), the mobile back chevron, and a close that
 * returns the pane to its resting Home.
 */
export function PaneBar({
  title,
  onClose,
  onBackToLists,
}: {
  /** What the pane is showing, as the banner menu named it. */
  title: string;
  /** Returns the pane to its resting Home. */
  onClose: () => void;
  /** Hands the screen back to the left lists (the single-column story only). */
  onBackToLists: () => void;
}): ReactNode {
  return (
    <nav className="chat-tab-bar" aria-label={title}>
      <button
        type="button"
        className="chat-back"
        onClick={onBackToLists}
        aria-label="Back to the lists"
        title="Back to the lists"
      >
        <Icon name="back" size={16} />
      </button>
      <div className="chat-tabs">
        <span className="chat-tab pane-tab active" aria-current="page">
          <span className="tab-chip-label">{title}</span>
        </span>
      </div>
      <button
        type="button"
        className="chat-scroll"
        onClick={onClose}
        aria-label={`Close ${title}`}
        title={`Close ${title}`}
      >
        <Icon name="close" size={16} />
      </button>
    </nav>
  );
}
