'use client';

/**
 * The authenticated shell: a left panel of lists, a right panel that runs on its own.
 *
 * The new-ui-02 IA (docs/design mockup `new-ui-02.tsx`) is a split: the LEFT panel holds the
 * account's lists behind its own tab state (`leftTab`), and the RIGHT panel holds either its
 * menu tabs (`rightTab` — Feed, Games, TopUp, Profile, Settings; Alerts and Search arrive
 * through the banner menu) or the open
 * conversations (`chatTabs` + `activeChat`). The two panels' states are independent on purpose:
 * reading Games on the left never disturbs the thread open on the right, which is the model's
 * whole offer. Below the PC breakpoint the panes take turns — `rightForced` remembers that a
 * menu panel (not a chat) has taken over the phone's screen.
 *
 * The URL fragment stays the single source of truth for the open conversation (see
 * use-open-conversation.ts): every door into a thread — a conversation row, a room join, a
 * friend's row, a deep link — is `openConversation(id)`, and the fragment's effect below both
 * adds the chip and activates it. "‹ Menu Panel" hides the thread without closing it — the
 * fragment keeps naming the conversation, so the chip can bring it straight back — while
 * closing a chip removes the tab and, for the conversation the fragment names, clears the
 * fragment (so Back and the close button can never disagree about what is open).
 *
 * The rooms provider sits inside the conversations provider because the two lists describe the
 * same objects from two sides: a join notes the room in both, and the sidebar's room rows and
 * the thread header read the room record back out.
 *
 * The call manager wraps everything under the session gate — a call can start from a thread
 * header and must keep ringing across pane switches — and the overlay it feeds renders after
 * the app div so a live call sits over the whole shell, not inside one pane of it.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id } from '@migo/sdk';

import { AppShell } from '@/components/app-shell.js';
import type { PanelTab, SystemTab } from '@/components/app-shell.js';
import type { RightTab } from '@/components/app-shell.js';
import { FriendsPanel } from '@/components/friends-panel.js';
import { GamesPanel } from '@/components/games-panel.js';
import { NotificationsPanel } from '@/components/notifications-panel.js';
import { ProfilePanel } from '@/components/profile-panel.js';
import { RequireReady } from '@/components/require-ready.js';
import { RoomsPanel } from '@/components/rooms-panel.js';
import { SearchPanel } from '@/components/search-panel.js';
import { SettingsPanel } from '@/components/settings-panel.js';
import { SpacePanel } from '@/components/space-panel.js';
import { WalletPanel } from '@/components/wallet-panel.js';
import { CallOverlay } from '@/components/call-overlay.js';
import { ConversationsProvider } from '@/lib/migo/conversations-provider.js';
import { RoomsProvider } from '@/lib/migo/rooms-provider.js';
import { CallManagerProvider } from '@/lib/migo/call-manager.js';
import {
  closeConversation,
  openConversation,
  useOpenConversation,
} from '@/lib/migo/use-open-conversation.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import type { RoomsContextValue } from '@/lib/migo/rooms-provider.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import type { ResolvedProfile } from '@/lib/migo/use-profiles.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';
import { SectionNavProvider } from '@/lib/migo/section-nav.js';

export default function ChatLayout({ children }: { children: ReactNode }): ReactNode {
  return (
    <RequireReady>
      <ConversationsProvider>
        <RoomsProvider>
          <CallManagerProvider>
            <TabbedShell>{children}</TabbedShell>
          </CallManagerProvider>
        </RoomsProvider>
      </ConversationsProvider>
    </RequireReady>
  );
}

/**
 * The pane machine, inside the providers it reads.
 *
 * `activeChat` is the conversation the right pane shows, or null for its menu tabs; it follows
 * the fragment but can be ducked out of ("‹ Menu Panel") without closing the conversation. The
 * open conversations are held as a plain ordered id list; their chip titles are derived per
 * render from the conversation list (a room's name, a direct chat's peer) exactly the way the
 * conversation rows derive theirs, so a tab always says what the row would.
 */
function TabbedShell({ children }: { children: ReactNode }): ReactNode {
  const { accountId } = useMigo();
  const openId = useOpenConversation();
  const { items } = useConversations();
  const rooms = useRooms();

  const [chatTabs, setChatTabs] = useState<Id[]>([]);
  const [activeChat, setActiveChat] = useState<Id | null>(null);
  // A session opens on its people: the mockup's first tab is Main (the friends list).
  const [leftTab, setLeftTab] = useState<SystemTab>('friends');
  const [rightTab, setRightTab] = useState<RightTab>('feed');
  // A menu panel covering the phone: the right pane's claim on a single-column screen.
  const [rightForced, setRightForced] = useState(false);

  // The fragment effect: every door into a thread lands here, whatever opened it. Clearing the
  // fragment (Back, the last chip closing) lands here too, and ducks the right pane to its menu.
  useEffect(() => {
    if (openId === null) {
      setActiveChat(null);
      return;
    }
    setChatTabs((prev) => (prev.includes(openId) ? prev : [...prev, openId]));
    setActiveChat(openId);
  }, [openId]);

  const selectChat = useCallback(
    (conversationId: Id): void => {
      if (conversationId === openId) {
        // The thread is already the open one; only the right pane's mode lags behind the chip.
        setActiveChat(conversationId);
        return;
      }
      openConversation(conversationId);
    },
    [openId],
  );

  const closeChat = useCallback(
    (conversationId: Id): void => {
      setChatTabs((prev) => prev.filter((id) => id !== conversationId));
      if (conversationId === openId) {
        // Closing the open thread: fall through to the most recent remaining one, else the menu.
        const remaining = chatTabs.filter((id) => id !== conversationId);
        const next = remaining.length > 0 ? remaining[remaining.length - 1] : undefined;
        if (next !== undefined) {
          openConversation(next);
        } else {
          setActiveChat(null);
          closeConversation();
        }
      }
    },
    [chatTabs, openId],
  );

  // "‹ Menu Panel": the right pane keeps its chips but shows its menu tabs; on a phone the left
  // panel takes the screen back.
  const backToMenu = useCallback((): void => {
    setActiveChat(null);
    setRightForced(false);
  }, []);

  const openPanel = useCallback((panel: PanelTab): void => {
    setRightTab(panel);
    setRightForced(true);
    setActiveChat(null);
  }, []);

  const selectRightTab = useCallback((tab: RightTab): void => {
    setRightTab(tab);
  }, []);

  // The cross-section navigation every deep surface shares: the system tabs drive the left
  // panel, the secondary panels arrive in the right one.
  const navigate = useCallback(
    (tab: SystemTab | PanelTab): void => {
      if (tab === 'friends' || tab === 'rooms' || tab === 'games' || tab === 'feed') {
        setLeftTab(tab);
        return;
      }
      openPanel(tab);
    },
    [openPanel],
  );

  const openInTab = useCallback((conversationId: Id): void => {
    openConversation(conversationId);
  }, []);

  // The chips' titles: a room is its #name, a direct chat is the peer's display name — the
  // same derivation the conversation rows use, read from the same sources.
  const peerIds = chatTabs.flatMap((id) => {
    const summary = items.find((item) => item.conversationId === id);
    if (summary?.kind !== ConversationKind.Direct) {
      return [];
    }
    const other = summary.members?.find((member) => member !== accountId) ?? null;
    return other !== null ? [other] : [];
  });
  const profiles = useProfiles(peerIds);

  const chatChips = chatTabs.map((id) => ({
    conversationId: id,
    title: chipTitleOf(id, items, rooms, accountId, profiles),
  }));

  const leftContent = (() => {
    switch (leftTab) {
      case 'friends':
        return <FriendsPanel onOpenConversation={openInTab} />;
      case 'rooms':
        return <RoomsPanel onOpenConversation={openInTab} />;
      case 'games':
        return <GamesPanel />;
      case 'feed':
        return <SpacePanel onOpenConversation={openInTab} />;
    }
  })();

  const rightContent = (() => {
    switch (rightTab) {
      case 'feed':
        return <SpacePanel onOpenConversation={openInTab} />;
      case 'games':
        return <GamesPanel />;
      case 'notifications':
        return <NotificationsPanel />;
      case 'search':
        return <SearchPanel onOpenConversation={openInTab} />;
      case 'wallet':
        return <WalletPanel />;
      case 'profile':
        return <ProfilePanel onOpenSettings={() => openPanel('settings')} />;
      case 'settings':
        return <SettingsPanel />;
    }
  })();

  return (
    <SectionNavProvider navigate={navigate}>
      <AppShell
        leftTab={leftTab}
        leftContent={leftContent}
        rightTab={rightTab}
        rightContent={rightContent}
        activeChat={activeChat}
        chatTabs={chatChips}
        showRight={activeChat !== null || rightForced}
        onSelectSystem={setLeftTab}
        onSelectRightTab={selectRightTab}
        onSelectChat={selectChat}
        onCloseChat={closeChat}
        onBackToMenu={backToMenu}
        onOpenPanel={openPanel}
      >
        {children}
      </AppShell>
      <CallOverlay />
    </SectionNavProvider>
  );
}

/**
 * A chip's title: the room's `#name` when the shell knows the room, the peer's display name
 * for a direct chat, and the summary's own title as the honest fallback.
 */
function chipTitleOf(
  conversationId: Id,
  items: ConversationSummary[],
  rooms: RoomsContextValue,
  accountId: Id | null,
  profiles: Map<Id, ResolvedProfile>,
): string {
  const summary = items.find((item) => item.conversationId === conversationId);
  if (summary === undefined) {
    return 'Chat';
  }
  if (summary.kind === ConversationKind.Direct) {
    const other = summary.members?.find((member) => member !== accountId) ?? null;
    return (other !== null ? profiles.get(other)?.displayName : undefined) ?? 'Direct message';
  }
  const room = rooms.infoFor(conversationId);
  return `#${room?.name ?? summary.title ?? 'room'}`;
}
