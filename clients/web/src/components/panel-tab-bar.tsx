'use client';

/**
 * The right panel's menu bar: the pane's own navigation, independent of the left panel.
 *
 * When no conversation is active, the new-ui-02 right pane is the "menu panel" — its own teal
 * header naming what it shows ("Panel: Feed") with the mockup's small tab buttons: Feed, Games,
 * Profile, Settings, TopUp. The reference's list is five buttons; the real app adds the two
 * surfaces it owns that the mockup never drew — Alerts and Search — because a surface without a
 * control is stranded. The left panel's tabs are a separate state: clicking Games here does not
 * touch what the left panel shows, and that independence is the whole point of the model.
 *
 * The back control is the mobile story: below the PC breakpoint the right pane takes over the
 * whole screen (see the app shell), so the menu pane carries its own "‹ Menu Panel" way back.
 * It is hidden on a PC, where both panes are always on screen and the control has nowhere to go.
 */

import type { ReactNode } from 'react';

import { Icon } from './icons.js';
import type { IconName } from './icons.js';
import type { PanelTab } from './tab-strip.js';

/** What the right pane can show in menu mode: the streams plus the secondary panels. */
export type RightTab = 'feed' | 'games' | PanelTab;

interface RightSection {
  id: RightTab;
  label: string;
  icon: IconName;
}

const RIGHT_TABS: ReadonlyArray<RightSection> = [
  { id: 'feed', label: 'Feed', icon: 'space' },
  { id: 'games', label: 'Games', icon: 'game' },
  { id: 'notifications', label: 'Alerts', icon: 'bell' },
  { id: 'search', label: 'Search', icon: 'search' },
  { id: 'wallet', label: 'TopUp', icon: 'wallet' },
  { id: 'profile', label: 'Profile', icon: 'user' },
  { id: 'settings', label: 'Settings', icon: 'settings' },
];

/** The pane's name for what it is showing, as the header's title spells it. */
export function rightTabLabel(tab: RightTab): string {
  return RIGHT_TABS.find((section) => section.id === tab)?.label ?? 'Panel';
}

/**
 * The bar itself.
 *
 * @param active The right pane's menu tab id.
 * @param onSelect Switches the right pane's content.
 * @param onBackToMenu Hands the whole screen back to the left panel (mobile only control).
 */
export function PanelTabBar({
  active,
  onSelect,
  onBackToMenu,
}: {
  active: RightTab;
  onSelect: (tab: RightTab) => void;
  onBackToMenu: () => void;
}): ReactNode {
  return (
    <nav className="panel-tab-bar" aria-label="Panel">
      <button type="button" className="chat-back pane-back" onClick={onBackToMenu}>
        <Icon name="back" size={14} />
        <span>Menu Panel</span>
      </button>
      <span className="panel-tab-title">
        <Icon name="sparkle" size={16} />
        <span>{`Panel: ${rightTabLabel(active)}`}</span>
      </span>
      <div className="panel-tabs">
        {RIGHT_TABS.map((section) => (
          <button
            key={section.id}
            type="button"
            className={`panel-tab${active === section.id ? ' active' : ''}`}
            aria-current={active === section.id ? 'page' : undefined}
            onClick={() => onSelect(section.id)}
          >
            {section.label}
          </button>
        ))}
      </div>
    </nav>
  );
}
