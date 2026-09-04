'use client';

/**
 * The left panel's tab strip: the lists and streams a messenger lives in.
 *
 * The new-ui-02 model (docs/design mockup `new-ui-02.tsx`) splits the app into two independent
 * panels on a PC: the LEFT panel owns the account's lists — its tab bar carries Main (the
 * friends list), Chats (the conversations), Rooms, Feed — and the chat tabs that this
 * strip used to carry have moved to the right panel's own bar, where a conversation actually
 * opens. Games is not a list but a place: it lives as the right pane's resting tab, so the
 * arcade sits one chip away from every thread instead of competing with the lists for the
 * left panel. The strip keeps the teal `#00838F`; the active chip is the brighter `#00ACC1`
 * over the orange underline, exactly the pairing the mockup draws. It scrolls horizontally
 * when it overflows rather than hiding anything: a tab that is off-screen is still a tab.
 *
 * The Chats chip carries the unread dot because it is the messenger's one list that exists to
 * answer "did somebody write me?" — without it, a message that arrives while another tab is
 * showing has no mark anywhere until its reader goes looking, which is a messenger whose
 * postman never rings.
 */

import type { ReactNode } from 'react';

import { Icon } from './icons.js';
import type { IconName } from './icons.js';

/** The system tabs — the lists and streams a messenger lives in, in the reference's order. */
export type SystemTab = 'friends' | 'chats' | 'rooms' | 'feed';

/** The secondary panels the right pane can show, shared with the banner menu that opens them. */
export type PanelTab =
  'notifications' | 'search' | 'wallet' | 'profile' | 'account' | 'settings' | 'admins' | 'store';

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
  { id: 'feed', label: 'Feed', icon: 'space' },
];

/** The secondary panels' labels, shared with the banner menu that opens them. */
export const PANEL_LABELS: Readonly<Record<PanelTab, string>> = {
  notifications: 'Alerts',
  search: 'Search',
  wallet: 'Wallet',
  profile: 'Profile',
  account: 'Account',
  settings: 'Settings',
  admins: 'Admins',
  store: 'Store',
};

/**
 * The strip itself.
 *
 * @param active The left panel's tab id.
 * @param onSelectSystem Switches the left panel to a system tab.
 * @param chatsUnread Whether any conversation has an unread message — the Chats chip's dot.
 * @param hideChats Omits the Chats chip, for the display mode whose chats live in the right pane's
 *   own tabs — a list and a tab bar showing the same conversations is the same door twice.
 */
export function TabStrip({
  active,
  onSelectSystem,
  chatsUnread = false,
  hideChats = false,
}: {
  active: SystemTab;
  onSelectSystem: (tab: SystemTab) => void;
  chatsUnread?: boolean;
  hideChats?: boolean;
}): ReactNode {
  return (
    <nav className="tab-strip" aria-label="Sections">
      {SYSTEM_TABS.filter((section) => !(hideChats && section.id === 'chats')).map((section) => (
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
          {section.id === 'chats' && chatsUnread ? (
            <span className="tab-chip-dot" aria-label="Unread messages" />
          ) : null}
        </button>
      ))}
    </nav>
  );
}
