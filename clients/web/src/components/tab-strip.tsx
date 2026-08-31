'use client';

/**
 * The tab strip: the reference's top chrome, worn the same at every width.
 *
 * Five system tabs — Friends, Chats, Rooms, Games, Feed — then one closable chip per open
 * conversation (the reference's 💬/👤 tabs) and per open secondary panel. The strip is the teal
 * `#00838F`; the active chip is the brighter `#00ACC1` over an orange underline, exactly the
 * pairing the mockup draws. It scrolls horizontally when it overflows rather than hiding
 * anything: a tab that is off-screen is still a tab, and the reference's mobile view scrolls
 * the same strip.
 */

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { Icon } from './icons.js';
import type { IconName } from './icons.js';

/** The five system tabs — the lists and streams a messenger lives in, in the reference's order. */
export type SystemTab = 'friends' | 'chats' | 'rooms' | 'games' | 'feed';

/** The secondary panels that open as closable tabs rather than sitting on the strip. */
export type PanelTab = 'notifications' | 'search' | 'wallet' | 'profile' | 'settings';

/** One open conversation as the strip knows it: which thread, what to call the chip. */
export interface ChatTabChip {
  conversationId: Id;
  title: string;
}

interface SystemSection {
  id: SystemTab;
  label: string;
  icon: IconName;
}

/** A chip's class list: the base plus only the modifiers that apply, with no stray spaces. */
function chipClass(...parts: Array<string | false>): string {
  return parts.filter(Boolean).join(' ');
}

const SYSTEM_TABS: ReadonlyArray<SystemSection> = [
  { id: 'friends', label: 'Friends', icon: 'friends' },
  { id: 'chats', label: 'Chats', icon: 'chats' },
  { id: 'rooms', label: 'Rooms', icon: 'rooms' },
  { id: 'games', label: 'Games', icon: 'game' },
  { id: 'feed', label: 'Feed', icon: 'space' },
];

/** The secondary panels' chip labels, shared with the banner menu that opens them. */
export const PANEL_LABELS: Readonly<Record<PanelTab, string>> = {
  notifications: 'Alerts',
  search: 'Search',
  wallet: 'Wallet',
  profile: 'Profile',
  settings: 'Settings',
};

const PANEL_ICONS: Readonly<Record<PanelTab, IconName>> = {
  notifications: 'bell',
  search: 'search',
  wallet: 'wallet',
  profile: 'user',
  settings: 'settings',
};

/**
 * The strip itself.
 *
 * @param active The active tab id: a system tab id, `chat:<id>`, or `panel:<id>`.
 * @param chatTabs The open conversations, in open order, left of which nothing closes.
 * @param panelTabs The open secondary panels, in open order.
 */
export function TabStrip({
  active,
  chatTabs,
  panelTabs,
  onSelectSystem,
  onSelectChat,
  onSelectPanel,
  onCloseChat,
  onClosePanel,
}: {
  active: string;
  chatTabs: readonly ChatTabChip[];
  panelTabs: readonly PanelTab[];
  onSelectSystem: (tab: SystemTab) => void;
  onSelectChat: (conversationId: Id) => void;
  onSelectPanel: (panel: PanelTab) => void;
  onCloseChat: (conversationId: Id) => void;
  onClosePanel: (panel: PanelTab) => void;
}): ReactNode {
  return (
    <nav className="tab-strip" aria-label="Sections">
      {SYSTEM_TABS.map((section) => (
        <button
          key={section.id}
          type="button"
          className={chipClass('tab-chip', active === section.id && 'active')}
          aria-current={active === section.id ? 'page' : undefined}
          onClick={() => onSelectSystem(section.id)}
        >
          <span className="tab-chip-icon" aria-hidden="true">
            <Icon name={section.icon} size={16} />
          </span>
          <span className="tab-chip-label">{section.label}</span>
        </button>
      ))}

      {chatTabs.map((tab) => (
        <button
          key={`chat:${tab.conversationId}`}
          type="button"
          className={chipClass(
            'tab-chip',
            'tab-chat',
            active === `chat:${tab.conversationId}` && 'active',
          )}
          aria-current={active === `chat:${tab.conversationId}` ? 'page' : undefined}
          onClick={() => onSelectChat(tab.conversationId)}
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
              onCloseChat(tab.conversationId);
            }}
          >
            <Icon name="close" size={16} />
          </span>
        </button>
      ))}

      {panelTabs.map((panel) => (
        <button
          key={`panel:${panel}`}
          type="button"
          className={chipClass('tab-chip', 'tab-panel', active === `panel:${panel}` && 'active')}
          aria-current={active === `panel:${panel}` ? 'page' : undefined}
          onClick={() => onSelectPanel(panel)}
        >
          <span className="tab-chip-icon" aria-hidden="true">
            <Icon name={PANEL_ICONS[panel]} size={16} />
          </span>
          <span className="tab-chip-label">{PANEL_LABELS[panel]}</span>
          <span
            className="tab-close"
            role="button"
            tabIndex={-1}
            aria-label={`Close ${PANEL_LABELS[panel]}`}
            title={`Close ${PANEL_LABELS[panel]}`}
            onClick={(event) => {
              event.stopPropagation();
              onClosePanel(panel);
            }}
          >
            <Icon name="close" size={16} />
          </span>
        </button>
      ))}
    </nav>
  );
}
