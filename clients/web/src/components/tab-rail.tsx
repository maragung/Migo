'use client';

import type { ReactNode } from 'react';

/**
 * The app's top-level sections, switched by the navigation rail.
 *
 * The chat shell stays one tab rather than becoming a route: the bundle is a static export and the
 * open conversation already lives in the URL fragment, so section switching is pure client state and
 * a tab change never unloads the session or the socket.
 */
export type AppTab =
  'chats' | 'friends' | 'notifications' | 'discover' | 'gifts' | 'profile' | 'settings';

const TABS: ReadonlyArray<{ id: AppTab; label: string; icon: string }> = [
  { id: 'chats', label: 'Chats', icon: '💬' },
  { id: 'friends', label: 'Friends', icon: '👥' },
  { id: 'notifications', label: 'Alerts', icon: '🔔' },
  { id: 'discover', label: 'Discover', icon: '🧭' },
  { id: 'gifts', label: 'Gifts', icon: '🎁' },
  { id: 'profile', label: 'Profile', icon: '👤' },
  { id: 'settings', label: 'Settings', icon: '⚙️' },
];

/**
 * The section switcher: a vertical rail beside the shell on desktop, a bottom bar on mobile.
 *
 * These are app sections, not a tab widget, so the markup is a plain `nav` of buttons with
 * `aria-current="page"` on the active one — the landmark and current-page attribute carry the
 * semantics a screen reader needs, without claiming tabpanel relationships the panels do not have.
 */
export function TabRail({
  active,
  onSelect,
}: {
  active: AppTab;
  onSelect: (tab: AppTab) => void;
}): ReactNode {
  return (
    <nav className="tab-rail" aria-label="Sections">
      {TABS.map((tab) => (
        <button
          key={tab.id}
          type="button"
          className={`tab-btn ${active === tab.id ? 'active' : ''}`}
          aria-current={active === tab.id ? 'page' : undefined}
          onClick={() => onSelect(tab.id)}
        >
          <span className="tab-icon" aria-hidden="true">
            {tab.icon}
          </span>
          <span className="tab-label">{tab.label}</span>
        </button>
      ))}
    </nav>
  );
}
