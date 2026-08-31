'use client';

/**
 * The authenticated shell: one app, a tab strip, and every surface as a tab.
 *
 * The v0.9.0 IA (docs/design/new-client-ui.tsx) is tab-based: five system tabs — Friends,
 * Chats, Rooms, Games, Feed — plus a closable chip for every open conversation and every
 * secondary panel. The tab list is session state, so it lives here rather than in the shell
 * component: a chat tab is added when a conversation opens and removed when its chip closes,
 * and the strip is just the drawing of that list.
 *
 * The URL fragment stays the single source of truth for the open conversation (see
 * use-open-conversation.ts): every door into a thread — a conversation row, a room join, a
 * friend's row, a deep link — is `openConversation(id)`, and the fragment's effect below both
 * adds the chip and activates it. Closing a chip is the reverse: it removes the tab and, for
 * the conversation the fragment names, clears the fragment (so Back and the close button can
 * never disagree about what is open).
 *
 * The rooms provider sits inside the conversations provider because the two lists describe the
 * same objects from two sides: a join notes the room in both, and the sidebar's room rows and
 * the thread header read the room record back out.
 *
 * The call manager wraps everything under the session gate — a call can start from a thread
 * header and must keep ringing across tab switches — and the overlay it feeds renders after
 * the app div so a live call sits over the whole shell, not inside one pane of it.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id } from '@migo/sdk';

import { AppShell } from '@/components/app-shell.js';
import type { PanelTab } from '@/components/app-shell.js';
import { FriendsPanel } from '@/components/friends-panel.js';
import { GamesPanel } from '@/components/games-panel.js';
import { NotificationsPanel } from '@/components/notifications-panel.js';
import { ProfilePanel } from '@/components/profile-panel.js';
import { RequireReady } from '@/components/require-ready.js';
import { RoomsPanel } from '@/components/rooms-panel.js';
import { SearchPanel } from '@/components/search-panel.js';
import { SettingsPanel } from '@/components/settings-panel.js';
import { Sidebar } from '@/components/sidebar.js';
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
 * The tab machine, inside the providers it reads.
 *
 * `active` is one of: a system tab id, `chat:<conversationId>`, or `panel:<id>`. The open
 * conversations are held as a plain ordered id list; their chip titles are derived per render
 * from the conversation list (a room's name, a direct chat's peer) exactly the way the
 * conversation rows derive theirs, so a tab always says what the row would.
 */
function TabbedShell({ children }: { children: ReactNode }): ReactNode {
  const { accountId } = useMigo();
  const openId = useOpenConversation();
  const { items } = useConversations();
  const rooms = useRooms();

  const [chatTabs, setChatTabs] = useState<Id[]>([]);
  const [panelTabs, setPanelTabs] = useState<PanelTab[]>([]);
  // A session opens on its conversations: the messenger's own first screen.
  const [active, setActive] = useState<string>('chats');

  // The fragment effect: every door into a thread lands here, whatever opened it.
  useEffect(() => {
    if (openId === null) {
      return;
    }
    setChatTabs((prev) => (prev.includes(openId) ? prev : [...prev, openId]));
    setActive(`chat:${openId}`);
  }, [openId]);

  // Clearing the fragment (Back, or the last chip closing) deactivates the thread surface.
  useEffect(() => {
    if (openId === null && active.startsWith('chat:')) {
      setActive('chats');
    }
  }, [openId, active]);

  const selectChat = useCallback(
    (conversationId: Id): void => {
      if (conversationId === openId) {
        // The thread is already the open one; the fragment is set, so only the active tab lags.
        setActive(`chat:${conversationId}`);
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
        // Closing the open thread: fall through to the most recent remaining one, else the list.
        const remaining = chatTabs.filter((id) => id !== conversationId);
        const next = remaining.length > 0 ? remaining[remaining.length - 1] : undefined;
        if (next !== undefined) {
          openConversation(next);
        } else {
          closeConversation();
        }
        return;
      }
      if (active === `chat:${conversationId}`) {
        setActive('chats');
      }
    },
    [chatTabs, openId, active],
  );

  const openPanel = useCallback((panel: PanelTab): void => {
    setPanelTabs((prev) => (prev.includes(panel) ? prev : [...prev, panel]));
    setActive(`panel:${panel}`);
  }, []);

  const closePanel = useCallback(
    (panel: PanelTab): void => {
      setPanelTabs((prev) => prev.filter((id) => id !== panel));
      if (active === `panel:${panel}`) {
        setActive('chats');
      }
    },
    [active],
  );

  // The cross-section navigation every deep surface shares: system tabs switch directly, the
  // secondary panels arrive as tabs of their own.
  const navigate = useCallback(
    (tab: 'friends' | 'chats' | 'rooms' | 'games' | 'feed' | PanelTab): void => {
      if (
        tab === 'friends' ||
        tab === 'chats' ||
        tab === 'rooms' ||
        tab === 'games' ||
        tab === 'feed'
      ) {
        setActive(tab);
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

  const section = (() => {
    if (active.startsWith('chat:') && openId !== null) {
      return <main className="thread-area">{children}</main>;
    }
    if (active.startsWith('panel:')) {
      const panel = active.slice('panel:'.length) as PanelTab;
      switch (panel) {
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
    }
    switch (active) {
      case 'friends':
        return <FriendsPanel onOpenConversation={openInTab} />;
      case 'rooms':
        return <RoomsPanel onOpenConversation={openInTab} />;
      case 'games':
        return <GamesPanel />;
      case 'feed':
        return <SpacePanel onOpenConversation={openInTab} />;
      case 'chats':
        return (
          <div className="chats-pane">
            <Sidebar />
          </div>
        );
    }
    return null;
  })();

  return (
    <SectionNavProvider navigate={navigate}>
      <AppShell
        active={active}
        chatTabs={chatChips}
        panelTabs={panelTabs}
        onSelectSystem={(tab) => setActive(tab)}
        onSelectChat={selectChat}
        onSelectPanel={(panel) => setActive(`panel:${panel}`)}
        onCloseChat={closeChat}
        onClosePanel={closePanel}
        onOpenPanel={openPanel}
      >
        {section}
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
