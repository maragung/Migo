'use client';

/**
 * The right panel's chat bar: the conversation tabs, on the pane that shows them.
 *
 * In the new-ui-02 model the chat tabs live on the RIGHT panel, not on the global strip: a
 * conversation opens where it renders. The bar is the mockup's slate-800 strip — a cyan
 * "‹ Menu Panel" control that hands the right pane back to its menu tabs, chevrons that scroll
 * the tab row without moving the page, and one closable chip per open conversation (#room or
 * @peer titles), exactly the drawing in the reference.
 *
 * The chevron scroll is the only behaviour here: `scrollBy` on the row's own overflow axis, so
 * an off-screen tab is still a tab, exactly the posture the left strip keeps.
 */

import { useRef } from 'react';
import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { Icon } from './icons.js';

/** One open conversation as the bar knows it: which thread, what to call the chip. */
export interface ChatTabChip {
  conversationId: Id;
  title: string;
}

/** How far one chevron click scrolls the row: about three chips, in either direction. */
const SCROLL_STEP_PX = 240;

/**
 * The bar itself.
 *
 * @param tabs The open conversations, in open order.
 * @param active The conversation whose thread the right pane is showing, if any.
 * @param onSelect Activates a conversation's chip.
 * @param onClose Closes a conversation's chip.
 * @param onBackToMenu Hands the right pane back to its menu tabs.
 */
export function ChatTabBar({
  tabs,
  active,
  onSelect,
  onClose,
  onBackToMenu,
}: {
  tabs: readonly ChatTabChip[];
  active: Id | null;
  onSelect: (conversationId: Id) => void;
  onClose: (conversationId: Id) => void;
  onBackToMenu: () => void;
}): ReactNode {
  const rowRef = useRef<HTMLDivElement | null>(null);

  const scrollRow = (direction: 'left' | 'right'): void => {
    rowRef.current?.scrollBy({
      left: direction === 'left' ? -SCROLL_STEP_PX : SCROLL_STEP_PX,
      behavior: 'smooth',
    });
  };

  return (
    <nav className="chat-tab-bar" aria-label="Open conversations">
      <button type="button" className="chat-back" onClick={onBackToMenu}>
        <Icon name="back" size={16} />
        <span>Menu Panel</span>
      </button>
      <button
        type="button"
        className="chat-scroll"
        aria-label="Scroll conversation tabs left"
        onClick={() => scrollRow('left')}
      >
        <Icon name="back" size={16} />
      </button>
      <div className="chat-tabs" ref={rowRef}>
        {tabs.map((tab) => (
          <button
            key={tab.conversationId}
            type="button"
            className={`chat-tab${active === tab.conversationId ? ' active' : ''}`}
            aria-current={active === tab.conversationId ? 'page' : undefined}
            onClick={() => onSelect(tab.conversationId)}
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
                onClose(tab.conversationId);
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
        aria-label="Scroll conversation tabs right"
        onClick={() => scrollRow('right')}
      >
        <Icon name="chevron-right" size={16} />
      </button>
    </nav>
  );
}
