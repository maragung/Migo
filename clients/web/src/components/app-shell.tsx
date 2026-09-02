'use client';

/**
 * The messenger shell: two independent panels on a PC, one column on a phone.
 *
 * The new-ui-02 model (docs/design mockup `new-ui-02.tsx`) replaces the v0.9.0 single strip
 * over one body with a split: a LEFT panel (~32% on a PC, the whole screen on a phone) that
 * owns the account's lists — its own teal tab strip (Main, Rooms, Games, Feed) over
 * the orange profile banner — and a RIGHT panel that runs on its own state: its menu tabs
 * (Feed, Games, TopUp, Profile, Settings — Alerts and Search open from the banner menu) when no
 * conversation is active, or
 * the chat tab bar with its closable conversation chips and "‹ Menu Panel" control when one
 * is. Clicking around the left panel never disturbs the right, and that independence is the
 * model's whole offer.
 *
 * Below the PC breakpoint the two panes take turns: the left panel is the app, and opening a
 * conversation or a menu panel slides the right pane over it. The `showRight` flag is the
 * layout's say in that — on a PC the CSS shows both panes whatever it reads.
 *
 * The shell stays a *presentational* component: which tabs exist, which is active, and what
 * happens on select/close are the layout's answers (see app/chat/layout.tsx), because the tab
 * lists are session state — they follow the open conversations, not the navigation.
 */

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import type { Theme } from '@/lib/theme.js';

import { ChatTabBar } from './chat-tab-bar.js';
import type { ChatTabChip } from './chat-tab-bar.js';
import { PanelTabBar } from './panel-tab-bar.js';
import type { RightTab } from './panel-tab-bar.js';
import { ProfileBanner } from './profile-banner.js';
import { TabStrip } from './tab-strip.js';
import type { PanelTab, SystemTab } from './tab-strip.js';

export type { PanelTab, SystemTab } from './tab-strip.js';
export type { ChatTabChip } from './chat-tab-bar.js';
export type { RightTab } from './panel-tab-bar.js';

/** Every section the shell can land on, for surfaces that ask the shell to navigate. */
export type AppTab = SystemTab | 'notifications' | 'search' | 'wallet' | 'profile' | 'settings';

/**
 * The shell around every authenticated screen.
 *
 * @param leftTab The left panel's active system tab.
 * @param leftContent The left panel's content, already chosen by the owner.
 * @param rightTab The right pane's active menu tab, shown when no conversation is active.
 * @param rightContent The right pane's menu content, already chosen by the owner.
 * @param activeChat The conversation the right pane is showing, or null for menu mode.
 * @param chatTabs One chip per open conversation, in open order.
 * @param showRight Whether the right pane covers the screen (the mobile story; a PC shows both).
 * @param onSelectSystem Switches the left panel's tab.
 * @param onSelectRightTab Switches the right pane's menu tab.
 * @param onSelectChat Activates an open conversation's chip.
 * @param onCloseChat Closes a conversation's chip.
 * @param onBackToMenu Hands the right pane back to its menu tabs (and the screen to the left panel).
 * @param onOpenPanel Opens a secondary panel in the right pane — the banner menu's action.
 * @param children The active conversation's thread, already chosen by the owner.
 */
export function AppShell({
  leftTab,
  leftContent,
  rightTab,
  rightContent,
  activeChat,
  chatTabs,
  showRight,
  onSelectSystem,
  onSelectRightTab,
  onSelectChat,
  onCloseChat,
  onBackToMenu,
  onOpenPanel,
  children,
  theme,
  onToggleTheme,
}: {
  leftTab: SystemTab;
  leftContent: ReactNode;
  rightTab: RightTab;
  rightContent: ReactNode;
  activeChat: Id | null;
  chatTabs: readonly ChatTabChip[];
  showRight: boolean;
  onSelectSystem: (tab: SystemTab) => void;
  onSelectRightTab: (tab: RightTab) => void;
  onSelectChat: (conversationId: Id) => void;
  onCloseChat: (conversationId: Id) => void;
  onBackToMenu: () => void;
  onOpenPanel: (panel: PanelTab) => void;
  children: ReactNode;
  /** Pins the theme control's appearance; defaults to the persisted theme. */
  theme?: Theme;
  /** Called when the theme control is clicked; defaults to flipping the persisted theme. */
  onToggleTheme?: () => void;
}): ReactNode {
  return (
    <div className={`app${showRight ? ' show-right' : ''}`}>
      <div className="app-left">
        <TabStrip active={leftTab} onSelectSystem={onSelectSystem} />
        <ProfileBanner onOpenPanel={onOpenPanel} theme={theme} onToggleTheme={onToggleTheme} />
        <div className="app-body">{leftContent}</div>
      </div>
      <div className="app-right">
        {activeChat !== null ? (
          <>
            <ChatTabBar
              tabs={chatTabs}
              active={activeChat}
              onSelect={onSelectChat}
              onClose={onCloseChat}
              onBackToMenu={onBackToMenu}
            />
            <div className="app-body">
              <main className="thread-area">{children}</main>
            </div>
          </>
        ) : (
          <>
            <PanelTabBar
              active={rightTab}
              onSelect={onSelectRightTab}
              onBackToMenu={onBackToMenu}
            />
            <div className="app-body">{rightContent}</div>
          </>
        )}
      </div>
    </div>
  );
}
