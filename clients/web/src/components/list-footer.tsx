'use client';

/**
 * The list window's footer band: the realtime connection's health and what the open list
 * responds to.
 *
 * The reference design closes its windows — the phone's home and the PC's contacts panel — with
 * the same pale band: one glance-worthy fact on the left, a small grey hint on the right that
 * changes with the list ("double-click to chat", "Migo activity"). It is a status bar in the
 * oldest sense: the two things you want at a glance and never want to go looking for.
 *
 * The left half used to carry the $MIG balance; the balance has moved up into the me bar, beside
 * the alerts and the account door where the design now puts it, and the connection mark has
 * taken the half it left. That is not a demotion of the connection but the same seat: a figure
 * the person glances at, in the band every list closes with — and it keeps the mark out of the
 * chat window, where it floated over the thread and answered to nobody.
 *
 * The mark is the ConnectionStatusDot's own vocabulary (green steady, yellow pulsing, red
 * dropped), so the band and the taskbar chip never disagree about the one transport they both
 * describe.
 */

import type { ReactNode } from 'react';

import { ConnectionStatusDot } from './connection-status-dot.js';

/** The lists a footer band can sit under. */
export type ListTab = 'friends' | 'chats' | 'rooms' | 'feed';

/**
 * What each list responds to, in the design's own register: a lowercase aside, not an instruction.
 *
 * The gesture named is the one the row actually implements — a hint for an affordance that is not
 * there would be worse than no hint, so each of these is the row's real primary action.
 */
const TAB_HINTS: Readonly<Record<ListTab, string>> = {
  friends: 'click a friend to open a chat',
  chats: 'click a conversation to open it',
  rooms: 'click a room to join and open it',
  feed: 'Migo activity',
};

/**
 * The band itself.
 *
 * @param tab The active list, which chooses the hint.
 * @param hint Overrides the tab's stock hint — the phone's home names the tap, not the click.
 */
export function ListFooter({ tab, hint }: { tab: ListTab; hint?: string }): ReactNode {
  return (
    <div className="list-footer">
      <span className="list-footer-conn">
        <ConnectionStatusDot />
      </span>
      <span className="list-footer-hint">{hint ?? TAB_HINTS[tab]}</span>
    </div>
  );
}
