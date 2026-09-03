'use client';

/**
 * The right pane's resting content: what it shows when no conversation or panel is open.
 *
 * It used to be the Feed — but the Feed is a left-panel tab, so a click on the strip's Feed
 * drew the identical activity list in both panes at once. The pane now rests on nothing in
 * particular: a mark, a word, and the two honest directions — a conversation from the lists,
 * a panel from the banner's menu. An empty pane that states its emptiness beats one that
 * quietly duplicates the panel beside it.
 */

import type { ReactNode } from 'react';

import { EmptyState } from './states.js';

/** The pane's resting state. */
export function PaneEmpty(): ReactNode {
  return (
    <div className="panel">
      <EmptyState
        icon="chats"
        title="Nothing open"
        hint="Pick a conversation from the lists, or open a panel from the banner's menu."
      />
    </div>
  );
}
