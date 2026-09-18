'use client';

/**
 * The phone's home navigation bar — Chat List Mode's own chrome.
 *
 * Four equal tabs across the width, each an icon over its own name, the one in front the only one
 * at full brightness: the arrangement a phone's messenger wears its home in, and the shape Chat
 * List Mode is asked to keep. It stands in for the strip while the phone is at home, because the
 * strip's other rows are windows' tabs and the chat list has no windows to carry; the strip comes
 * back the moment one exists, since a panel covers the phone whole and carries no title bar of its
 * own, which leaves the strip's tabs as the only way back to it.
 *
 * The four are the mode's home views in the mode's own order, and none of them carries an X: a
 * home view is not a tab to close, and the bar says so by drawing none. That is the whole of the
 * difference from the strip it stands in for.
 */

import type { ReactNode } from 'react';

import { Icon } from './icons.js';
import { MOBILE_NAV_META, MOBILE_NAV_ORDER } from './mobile-tab-bar.js';
import type { MobileNavTab } from './mobile-tab-bar.js';

/** The badge's ceiling: more than nine reads as "many", not as arithmetic. */
const BADGE_CAP = 9;

/**
 * The bar itself.
 *
 * `navUnread` is the shell's own attention counts for the home views — the same record the strip
 * badges its tabs from, so a person who switches modes finds the count where they left it. A view
 * with nothing unread draws no badge rather than a zero.
 */
export function MobileNavBar({
  navTab,
  navUnread,
  onSelectNav,
}: {
  navTab: MobileNavTab;
  navUnread: Readonly<Record<MobileNavTab, number>>;
  onSelectNav: (tab: MobileNavTab) => void;
}): ReactNode {
  return (
    <nav className="mnav-bar" aria-label="Home views">
      {MOBILE_NAV_ORDER.map((id) => {
        const active = navTab === id;
        const unread = navUnread[id] ?? 0;
        return (
          <button
            key={id}
            type="button"
            className={`mnav-item${active ? ' mnav-item-on' : ''}`}
            aria-current={active ? 'page' : undefined}
            title={`${MOBILE_NAV_META[id].label} — home view`}
            onClick={() => onSelectNav(id)}
          >
            <span className="mnav-icon">
              <Icon name={MOBILE_NAV_META[id].icon} size={21} />
              {unread > 0 ? (
                <span className="mnav-badge">{unread > BADGE_CAP ? `${BADGE_CAP}+` : unread}</span>
              ) : null}
            </span>
            <span className="mnav-label">{MOBILE_NAV_META[id].label}</span>
          </button>
        );
      })}
    </nav>
  );
}
