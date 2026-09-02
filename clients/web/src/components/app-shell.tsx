'use client';

/**
 * The messenger shell: two independent panels on a PC, one column on a phone.
 *
 * The new-ui-02 model (docs/design mockup `new-ui-02.tsx`) splits the app: a LEFT panel (~32% on
 * a PC, the whole screen on a phone) that owns the account's lists — its own teal tab strip
 * (Main, Rooms, Games, Feed) over the orange profile banner — and a RIGHT panel that shows what
 * the left panel's clicks open, as tabs: one closable chip per open conversation, the games
 * arcade, and the secondary panels the banner menu reaches, over a persistent Feed chip that is
 * the pane's resting content — what it shows when nothing is open. Clicking around the left
 * panel never disturbs the right, and that independence is the model's whole offer.
 *
 * Below the PC breakpoint the two panes take turns: the left panel is the app, and the right
 * pane covers it while it has something to show. The `showRight` flag is the layout's say in
 * that — on a PC the CSS shows both panes whatever it reads.
 *
 * The shell stays a *presentational* component: which tabs exist, which is active, and what
 * happens on select/close are the layout's answers (see app/chat/layout.tsx), because the tab
 * lists are session state — they follow the open conversations, not the navigation.
 */

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import type { Theme } from '@/lib/theme.js';

import { ProfileBanner } from './profile-banner.js';
import { RightTabBar } from './right-tab-bar.js';
import type { RightTabChip, RightPaneTab } from './right-tab-bar.js';
import { TabStrip } from './tab-strip.js';
import type { PanelTab, SystemTab } from './tab-strip.js';

export type { PanelTab, SystemTab } from './tab-strip.js';
export type { RightTabChip, RightPaneTab, RightTabKind } from './right-tab-bar.js';

/** Every section the shell can land on, for surfaces that ask the shell to navigate. */
export type AppTab = SystemTab | PanelTab;

/**
 * The shell around every authenticated screen.
 *
 * @param leftTab The left panel's active system tab.
 * @param leftContent The left panel's content, already chosen by the owner.
 * @param rightTabs The right pane's open closable tabs, in open order.
 * @param activeRight The right pane's active chip: `'feed'` or a tab id.
 * @param activeChat The conversation the right pane is showing, when the active tab is a chat.
 * @param rightContent The right pane's content for its active non-chat tab (Feed included).
 * @param showRight Whether the right pane covers the screen (the mobile story; a PC shows both).
 * @param onSelectSystem Switches the left panel's tab.
 * @param onSelectRight Activates a right-pane chip, the Feed chip included.
 * @param onCloseRight Closes a right-pane chip.
 * @param onBackToLists Hands the screen back to the left lists (the single-column story only).
 * @param onOpenPanel Opens a secondary panel as a right-pane tab — the banner menu's action.
 * @param chatsUnread Whether any conversation has an unread message — the Chats chip's dot.
 * @param children The active conversation's thread, already chosen by the owner.
 */
export function AppShell({
  leftTab,
  leftContent,
  rightTabs,
  activeRight,
  activeChat,
  rightContent,
  showRight,
  onSelectSystem,
  onSelectRight,
  onCloseRight,
  onBackToLists,
  onOpenPanel,
  children,
  theme,
  onToggleTheme,
  chatsUnread,
}: {
  leftTab: SystemTab;
  leftContent: ReactNode;
  rightTabs: readonly RightTabChip[];
  activeRight: RightPaneTab;
  activeChat: Id | null;
  rightContent: ReactNode;
  showRight: boolean;
  onSelectSystem: (tab: SystemTab) => void;
  onSelectRight: (id: RightPaneTab) => void;
  onCloseRight: (id: string) => void;
  onBackToLists: () => void;
  onOpenPanel: (panel: PanelTab) => void;
  children: ReactNode;
  /** Pins the theme control's appearance; defaults to the persisted theme. */
  theme?: Theme;
  /** Called when the theme control is clicked; defaults to flipping the persisted theme. */
  onToggleTheme?: () => void;
  /** Whether any conversation has an unread message — the Chats chip's dot. */
  chatsUnread?: boolean;
}): ReactNode {
  return (
    <div className={`app${showRight ? ' show-right' : ''}`}>
      <div className="app-left">
        <TabStrip active={leftTab} onSelectSystem={onSelectSystem} chatsUnread={chatsUnread} />
        <ProfileBanner onOpenPanel={onOpenPanel} theme={theme} onToggleTheme={onToggleTheme} />
        <div className="app-body">{leftContent}</div>
      </div>
      <div className="app-right">
        <RightTabBar
          tabs={rightTabs}
          active={activeRight}
          onSelect={onSelectRight}
          onClose={onCloseRight}
          onBackToLists={onBackToLists}
        />
        <div className="app-body">
          {activeChat !== null ? <main className="thread-area">{children}</main> : rightContent}
        </div>
      </div>
    </div>
  );
}
