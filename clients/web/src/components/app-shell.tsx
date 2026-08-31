'use client';

/**
 * The messenger shell: Migo's primary navigation, re-composed per size.
 *
 * The v0.9.0 restyle (docs/design/new-client-ui.tsx) trades the icon rail and the five-slot
 * bottom bar for the reference's model, worn the same way at every width: a teal TAB STRIP
 * across the top — the system tabs Friends | Chats | Rooms | Games | Feed plus one closable
 * chip per open conversation or secondary panel — over an orange PROFILE BANNER that owns the
 * account: avatar, presence, the coin balance, and the menu (profile, wallet, settings, sign
 * out) that used to live on the rail's foot. Mobile and PC differ only in width: the strip
 * scrolls horizontally, exactly as the reference's mobile view does, so there is no separate
 * mobile chrome to keep in sync.
 *
 * The shell stays a *presentational* component: which tabs exist, which is active, and what
 * happens on select/close are the layout's answers (see app/chat/layout.tsx), because the tab
 * list is session state — it follows the open conversations, not the navigation.
 */

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import type { Theme } from '@/lib/theme.js';

import { ProfileBanner } from './profile-banner.js';
import { TabStrip } from './tab-strip.js';
import type { ChatTabChip, PanelTab } from './tab-strip.js';

export type { ChatTabChip, PanelTab } from './tab-strip.js';
export type { SystemTab } from './tab-strip.js';

/** Every section the shell can land on, for surfaces that ask the shell to navigate. */
export type AppTab =
  | 'friends'
  | 'chats'
  | 'rooms'
  | 'games'
  | 'feed'
  | 'notifications'
  | 'search'
  | 'wallet'
  | 'profile'
  | 'settings';

/**
 * The shell around every authenticated screen.
 *
 * @param active The tab id currently shown: a system tab, `chat:<id>`, or `panel:<id>`.
 * @param chatTabs One chip per open conversation, in open order.
 * @param panelTabs One chip per open secondary panel (wallet, settings, …).
 * @param onSelectSystem Switches to a system tab.
 * @param onSelectChat Activates an open conversation's chip.
 * @param onCloseChat Closes a conversation's chip.
 * @param onClosePanel Closes a secondary panel's chip.
 * @param onOpenPanel Opens a secondary panel as a tab — the banner menu's action.
 * @param children The active tab's content, already chosen by the owner.
 */
export function AppShell({
  active,
  chatTabs,
  panelTabs,
  onSelectSystem,
  onSelectChat,
  onSelectPanel,
  onCloseChat,
  onClosePanel,
  onOpenPanel,
  children,
  theme,
  onToggleTheme,
}: {
  active: string;
  chatTabs: readonly ChatTabChip[];
  panelTabs: readonly PanelTab[];
  onSelectSystem: (tab: 'friends' | 'chats' | 'rooms' | 'games' | 'feed') => void;
  onSelectChat: (conversationId: Id) => void;
  onSelectPanel: (panel: PanelTab) => void;
  onCloseChat: (conversationId: Id) => void;
  onClosePanel: (panel: PanelTab) => void;
  onOpenPanel: (panel: PanelTab) => void;
  children: ReactNode;
  /** Pins the theme control's appearance; defaults to the persisted theme. */
  theme?: Theme;
  /** Called when the theme control is clicked; defaults to flipping the persisted theme. */
  onToggleTheme?: () => void;
}): ReactNode {
  return (
    <div className="app">
      <TabStrip
        active={active}
        chatTabs={chatTabs}
        panelTabs={panelTabs}
        onSelectSystem={onSelectSystem}
        onSelectChat={onSelectChat}
        onSelectPanel={onSelectPanel}
        onCloseChat={onCloseChat}
        onClosePanel={onClosePanel}
      />
      <ProfileBanner onOpenPanel={onOpenPanel} theme={theme} onToggleTheme={onToggleTheme} />
      <div className="app-body">{children}</div>
    </div>
  );
}
