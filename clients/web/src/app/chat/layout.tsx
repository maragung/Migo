'use client';

/**
 * The authenticated shell: a left panel of lists, a right panel of tabs.
 *
 * The new-ui-02 IA (docs/design mockup `new-ui-02.tsx`) is a split: the LEFT panel holds the
 * account's lists behind its own tab state (`leftTab`) — Main (friends), Chats (the
 * conversations, the one list that answers "did somebody write me?", with its unread dot on
 * the strip), Rooms, Feed — and the RIGHT panel shows what the left panel's clicks
 * open, as closable tabs (`right`): a conversation (private, group, or room), or a secondary
 * panel the banner menu or a deep link reached. Games is the pane's resting chip — always
 * first, never closed — so a pane with nothing open shows the arcade, and the catalogue
 * never competes with the lists for the left panel. Closing a room or group tab also
 * leaves the conversation: the chip was the last open door to it, and a room a person has
 * walked out of should not linger as a tab they have to close twice. The two panels' states
 * are independent on purpose: reading Feed on the left never disturbs the thread open on
 * the right, which is the model's whole offer. Below the PC breakpoint the panes take
 * turns — the right pane covers the phone while it has something to show, and `dismissed`
 * remembers that its back chevron sent the screen home to the lists without closing a thing.
 *
 * The URL fragment stays the single source of truth for the open conversation (see
 * use-open-conversation.ts): every door into a thread — a conversation row, a room join, a
 * friend's row, a deep link — is `openConversation(id)`, and the fragment's effect below both
 * adds the chip and activates it. Clearing the fragment (Back, or the open thread's chip
 * closing) removes the thread's tab and falls through to the most recent remaining one, else
 * the Feed — Back and the close button can never disagree about what is open.
 *
 * How the right pane holds its chats is a display setting (see lib/chat-tabs-mode.ts), not a
 * session choice: the right-tabs default docks every open chat as a closable chip, and the
 * one-window mode drops the pane's tab bar entirely — the Chats list returns to the side tabs,
 * a chat opens as one full window at a time over whatever the pane was resting on, and a slim
 * title bar (one label, one close) replaces the chips. The setting writes through to
 * localStorage so the preference follows the person, not the tab they made it in.
 *
 * The rooms provider sits inside the conversations provider because the two lists describe the
 * same objects from two sides: a join notes the room in both, and the sidebar's room rows and
 * the thread header read the room record back out.
 *
 * The call manager wraps everything under the session gate — a call can start from a thread
 * header and must keep ringing across pane switches — and the overlay it feeds renders after the
 * app div so a live call sits over the whole shell, not inside one pane of it. The connection
 * Snackbar sits at the same level for the same reason: "Reconnecting…" is news about the session,
 * not about whichever pane is showing.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id } from '@migo/sdk';

import { AdminsPanel } from '@/components/admins-panel.js';
import { AppShell } from '@/components/app-shell.js';
import type { PanelTab, SystemTab } from '@/components/app-shell.js';
import type { RightTabChip, RightTabKind } from '@/components/app-shell.js';
import { AccountPanel } from '@/components/account-panel.js';
import { ConversationList } from '@/components/conversation-list.js';
import { FriendsPanel } from '@/components/friends-panel.js';
import { GamesPanel } from '@/components/games-panel.js';
import { NotificationsPanel } from '@/components/notifications-panel.js';
import { ProfilePanel } from '@/components/profile-panel.js';
import { RequireReady } from '@/components/require-ready.js';
import { RoomsPanel } from '@/components/rooms-panel.js';
import { SearchPanel } from '@/components/search-panel.js';
import { SettingsPanel } from '@/components/settings-panel.js';
import { SpacePanel } from '@/components/space-panel.js';
import { StorePanel } from '@/components/store-panel.js';
import { WalletPanel } from '@/components/wallet-panel.js';
import { PaneBar } from '@/components/right-tab-bar.js';
import { CallOverlay } from '@/components/call-overlay.js';
import { ConversationsProvider } from '@/lib/migo/conversations-provider.js';
import { RoomsProvider } from '@/lib/migo/rooms-provider.js';
import { MutedProvider } from '@/lib/migo/muted-provider.js';
import { CallManagerProvider } from '@/lib/migo/call-manager.js';
import { ConnectionSnackbar } from '@/components/connection-snackbar.js';
import { getChatTabsMode, setChatTabsMode } from '@/lib/chat-tabs-mode.js';
import type { ChatTabsMode } from '@/lib/chat-tabs-mode.js';
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
          <MutedProvider>
            <CallManagerProvider>
              <TabbedShell>{children}</TabbedShell>
            </CallManagerProvider>
          </MutedProvider>
        </RoomsProvider>
      </ConversationsProvider>
    </RequireReady>
  );
}

/** One open thing on the right pane; the chip's title is derived per render. */
interface RightTabItem {
  /** `chat:<conversation>` for a thread, the kind itself for one-per-kind tabs. */
  id: string;
  kind: RightTabKind;
  /** The conversation a chat tab shows. */
  conversationId?: Id;
}

/** What the one-window mode's pane can hold: any non-chat thing, the arcade included. */
type PanePanel = Exclude<RightTabKind, 'chat'>;

/** The right pane's whole state, in one object so no transition can tear it. */
interface RightPaneState {
  tabs: RightTabItem[];
  /** `'feed'`, or the id of the tab the pane is showing. */
  active: string;
}

/** The chip titles of the one-per-kind tabs; chat chips derive theirs from the live lists. */
const KIND_TITLES: Readonly<Record<Exclude<RightTabKind, 'chat'>, string>> = {
  notifications: 'Alerts',
  search: 'Search',
  wallet: 'TopUp',
  profile: 'Profile',
  account: 'Account',
  settings: 'Settings',
  admins: 'Admins',
  store: 'Store',
};

/** A chat tab's identity, so a conversation can never open twice. */
function chatIdOf(conversationId: Id): string {
  return `chat:${conversationId}`;
}

/**
 * The pane machine, inside the providers it reads.
 *
 * `right.active` is what the pane is showing — the Feed or one of its open tabs. The open
 * conversation is still the fragment's to name; the effect below reconciles the chat tab with
 * it, adding and activating on open, removing on close, and falling through to the most recent
 * remaining tab (else the Feed) whenever the showing tab goes away.
 */
function TabbedShell({ children }: { children: ReactNode }): ReactNode {
  const { accountId, client } = useMigo();
  const openId = useOpenConversation();
  const { items, unread } = useConversations();
  const rooms = useRooms();

  // A session opens on its people: the mockup's first tab is Main (the friends list).
  const [leftTab, setLeftTab] = useState<SystemTab>('friends');
  const [right, setRight] = useState<RightPaneState>({ tabs: [], active: 'feed' });
  // The right pane's back chevron (single-column only): the pane keeps its tabs, the screen
  // goes home to the lists until the next chip opens or activates.
  const [dismissed, setDismissed] = useState(false);
  // How the pane holds its chats — a display setting, read once here and written through by
  // pickChatsMode below. In the one-window mode the pane's chips are gone, so `panel` holds the
  // one non-chat thing it can show instead, full-window like a chat.
  const [chatsMode, setChatsMode] = useState<ChatTabsMode>(() => getChatTabsMode());
  const [panel, setPanel] = useState<PanePanel | null>(null);

  // The previous fragment, so a cleared fragment can remove exactly the thread it named.
  const prevOpenRef = useRef<Id | null>(null);

  // The fragment effect: every door into a thread lands here, whatever opened it. Clearing the
  // fragment (Back, the chip's close) lands here too, and takes the thread's tab with it. The
  // one-window mode keeps the fragment as the truth — it just shows the thread full-window
  // instead of minting a chip for it.
  useEffect(() => {
    const prev = prevOpenRef.current;
    prevOpenRef.current = openId;
    if (chatsMode === 'list') {
      if (openId !== null) {
        // A chat opening takes the whole pane, whatever it was resting on.
        setPanel(null);
        setDismissed(false);
      }
      return;
    }
    if (openId === null) {
      if (prev !== null) {
        const gone = chatIdOf(prev);
        setRight((s) => {
          const tabs = s.tabs.filter((tab) => tab.id !== gone);
          const active = s.active === gone ? (tabs[tabs.length - 1]?.id ?? 'feed') : s.active;
          return { tabs, active };
        });
      }
      return;
    }
    const id = chatIdOf(openId);
    setRight((s) => ({
      tabs: s.tabs.some((tab) => tab.id === id)
        ? s.tabs
        : [...s.tabs, { id, kind: 'chat', conversationId: openId }],
      active: id,
    }));
    setDismissed(false);
  }, [openId, chatsMode]);

  // A message that arrives for a conversation with no tab mints the tab — the user's rule for
  // windows: chat windows for rooms, groups, and private chats come into being when a packet
  // arrives for them, not only when someone clicks. Create only: the tab joins the pane's chips
  // but does not take the active one, so a message landing mid-read never yanks the thread out
  // from under the reader — the unread badge on the chip is the attention signal, and the
  // fragment effect still owns activation whenever the person does click through. The
  // one-window mode is exempt: its single surface is the fragment's, and minting a hidden tab
  // there would only stockpile ghosts for the mode's back chevron to find.
  const tabIdsRef = useRef<ReadonlySet<string>>(new Set());
  tabIdsRef.current = new Set(right.tabs.map((tab) => tab.id));
  useEffect(() => {
    if (client === null || chatsMode === 'list') {
      return;
    }
    const off = client.messaging.onMessage((message) => {
      if (message.senderId === accountId) {
        // Echoes of this account's own sends (multi-device sync included): the sending device
        // already minted its tab through the open conversation.
        return;
      }
      const id = chatIdOf(message.conversationId);
      if (tabIdsRef.current.has(id)) {
        return;
      }
      setRight((s) => {
        if (s.tabs.some((tab) => tab.id === id)) {
          return s;
        }
        return {
          tabs: [...s.tabs, { id, kind: 'chat', conversationId: message.conversationId }],
          active: s.active,
        };
      });
    });
    return () => {
      off();
    };
  }, [client, chatsMode, accountId]);

  /** Writes a display choice through to storage and reconciles the pane with it. */
  const pickChatsMode = useCallback((mode: ChatTabsMode): void => {
    setChatsMode(mode);
    setChatTabsMode(mode);
    // The pane's shape changes wholesale: drop the chips (or the lone panel) and let the
    // fragment effect re-mint the open thread in the mode that was just chosen.
    setPanel(null);
    setRight({ tabs: [], active: 'feed' });
    setDismissed(false);
  }, []);

  /** Opens (or activates) a one-per-kind tab: the arcade, a secondary panel. */
  const openRightTab = useCallback((kind: Exclude<RightTabKind, 'chat'>): void => {
    setRight((s) => ({
      tabs: s.tabs.some((tab) => tab.id === kind) ? s.tabs : [...s.tabs, { id: kind, kind }],
      active: kind,
    }));
    setDismissed(false);
  }, []);

  /** Opens a secondary panel in whichever shape the display setting gives the pane. */
  const openPanel = useCallback(
    (kind: PanePanel): void => {
      if (chatsMode === 'list') {
        setPanel(kind);
        setDismissed(false);
        return;
      }
      openRightTab(kind);
    },
    [chatsMode, openRightTab],
  );

  const selectRight = useCallback(
    (id: string): void => {
      setDismissed(false);
      if (id === 'feed') {
        setRight((s) => (s.active === 'feed' ? s : { ...s, active: 'feed' }));
        return;
      }
      const chat = right.tabs.find((tab) => tab.id === id && tab.kind === 'chat');
      if (chat?.conversationId !== undefined) {
        // The thread is the fragment's to name; the effect lands the add and the activation.
        if (chat.conversationId !== openId) {
          openConversation(chat.conversationId);
        } else {
          setRight((s) => (s.active === id ? s : { ...s, active: id }));
        }
        return;
      }
      setRight((s) => (s.active === id ? s : { ...s, active: id }));
    },
    [openId, right.tabs],
  );

  /**
   * Leaves the conversation a closed tab held, when leaving is what closing means.
   *
   * Best-effort by design: the tab is gone either way, and a leave the server refused (offline,
   * already left) must not resurrect it. The failure the person sees is the room still in the
   * Chats list on the next load — the honest signal, not a toast over a tab that is closed.
   */
  const leaveWhenClosed = useCallback(
    (conversationId: Id): void => {
      if (client === null) {
        return;
      }
      const summary = items.find((item) => item.conversationId === conversationId);
      if (summary === undefined) {
        return;
      }
      if (summary.kind === ConversationKind.Room) {
        const room = rooms.infoFor(conversationId);
        if (room !== null) {
          void client.rooms
            .leave(room.roomId)
            .then(() => {
              // The leaver's own device is excluded from the member fan-out, so nothing on the
              // wire corrects the held record — drop it here or the directory row keeps showing
              // the counts the room had while this account was in it.
              rooms.forgetRoom(room.roomId);
            })
            .catch(() => undefined);
        }
        return;
      }
      if (summary.kind === ConversationKind.Group) {
        void client.conversations.leave(conversationId).catch(() => undefined);
      }
    },
    [client, items, rooms],
  );

  const closeRight = useCallback(
    (id: string): void => {
      const tab = right.tabs.find((item) => item.id === id);
      setRight((s) => {
        const tabs = s.tabs.filter((item) => item.id !== id);
        const active = s.active === id ? (tabs[tabs.length - 1]?.id ?? 'feed') : s.active;
        return { tabs, active };
      });
      if (tab?.conversationId !== undefined) {
        // Closing a room or group tab is walking out of it: the chip was the conversation's
        // last open door, and a room already left should not linger as a tab to close twice.
        // A direct chat is not membership — closing it is the whole goodbye.
        leaveWhenClosed(tab.conversationId);
        if (tab.conversationId === openId) {
          // The open thread's chip: the fragment is cleared, and its effect is a no-op on a tab
          // already removed — the fall-through above is the one that ran.
          closeConversation();
        }
      }
    },
    [leaveWhenClosed, openId, right.tabs],
  );

  // The cross-section navigation every deep surface shares: the system tabs drive the left
  // panel, the secondary panels arrive in whichever shape the pane's display setting gives.
  // Games is not among the destinations: it is the right pane's resting chip, not a list.
  const navigate = useCallback(
    (tab: SystemTab | PanelTab): void => {
      if (tab === 'friends' || tab === 'chats' || tab === 'rooms' || tab === 'feed') {
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
  // same derivation the conversation rows use, read from the same sources. The one-window
  // mode's slim bar titles the open thread the same way, so its peer rides along too.
  const tabPeerIds = right.tabs.flatMap((tab) => {
    if (tab.kind !== 'chat' || tab.conversationId === undefined) {
      return [];
    }
    const summary = items.find((item) => item.conversationId === tab.conversationId);
    if (summary?.kind !== ConversationKind.Direct) {
      return [];
    }
    const other = summary.members?.find((member) => member !== accountId) ?? null;
    return other !== null ? [other] : [];
  });
  const openPeerId = (() => {
    if (chatsMode !== 'list' || openId === null) {
      return null;
    }
    const summary = items.find((item) => item.conversationId === openId);
    if (summary?.kind !== ConversationKind.Direct) {
      return null;
    }
    return summary.members?.find((member) => member !== accountId) ?? null;
  })();
  const peerIds =
    openPeerId !== null ? Array.from(new Set([...tabPeerIds, openPeerId])) : tabPeerIds;
  const profiles = useProfiles(peerIds);

  const chatChips = right.tabs.map((tab): RightTabChip => ({
    id: tab.id,
    kind: tab.kind,
    conversationId: tab.conversationId,
    title:
      tab.kind === 'chat' && tab.conversationId !== undefined
        ? chipTitleOf(tab.conversationId, items, rooms, accountId, profiles)
        : KIND_TITLES[tab.kind as Exclude<RightTabKind, 'chat'>],
  }));

  // What the pane shows for a non-chat target: the panel the kind names. The system tabs'
  // content (Feed, the lists) is deliberately absent — those live in the left panel, and a pane
  // that could also draw them drew the same list twice, side by side. Games is the exception
  // that earns its place: it is the pane's resting chip, so the arcade is what an empty pane
  // shows and what closing the last chip falls through to.
  const paneContent = (kind: Exclude<RightTabKind, 'chat'> | 'feed'): ReactNode => {
    switch (kind) {
      case 'feed':
        return <GamesPanel />;
      case 'notifications':
        return <NotificationsPanel />;
      case 'search':
        return <SearchPanel onOpenConversation={openInTab} />;
      case 'wallet':
        return <WalletPanel />;
      case 'profile':
        return <ProfilePanel onOpenSettings={() => openPanel('settings')} />;
      case 'account':
        return <AccountPanel />;
      case 'settings':
        return <SettingsPanel chatTabsMode={chatsMode} onChatTabsMode={pickChatsMode} />;
      case 'admins':
        return <AdminsPanel />;
      case 'store':
        return <StorePanel />;
    }
  };

  // What the pane is showing: the thread (the route's own page) for a chat target, the owner's
  // content for everything else. The one-window mode shows the open thread full-window — unless
  // a panel took the pane after it, in which case closing the panel hands the pane back.
  const activeItem =
    right.active === 'feed' ? null : (right.tabs.find((tab) => tab.id === right.active) ?? null);
  const activeChat =
    chatsMode === 'list'
      ? panel === null
        ? openId
        : null
      : activeItem?.kind === 'chat'
        ? (activeItem.conversationId ?? null)
        : null;
  const rightContent =
    chatsMode === 'list'
      ? paneContent(panel ?? 'feed')
      : activeItem?.kind === 'chat'
        ? null
        : paneContent(activeItem?.kind ?? 'feed');

  const leftContent = (() => {
    switch (leftTab) {
      case 'friends':
        return <FriendsPanel onOpenConversation={openInTab} />;
      case 'chats':
        return (
          <div className="panel">
            <h1 className="panel-title">Chats</h1>
            <ConversationList />
          </div>
        );
      case 'rooms':
        return <RoomsPanel onOpenConversation={openInTab} />;
      case 'feed':
        return <SpacePanel onOpenConversation={openInTab} />;
    }
  })();

  // The Chats chip's dot: a live unread mark or a summary whose persisted read mark lags its
  // last message. Without it, a message that arrives while another tab is showing has no mark
  // anywhere — which is the messenger whose postman never rings.
  const chatsUnread = unread.size > 0 || items.some((item) => item.lastSeq > item.readSeq);

  // The pane shows while it holds something: a chip (or two) in the right-tabs mode, the open
  // thread or panel in the one-window mode — an empty pane gets out of the lists' way.
  const showRight =
    chatsMode === 'list'
      ? !dismissed && (openId !== null || panel !== null)
      : !dismissed && !(right.tabs.length === 0 && right.active === 'feed');

  // The one-window mode's slim bar: the open thing's name as a plain label, and a close that
  // takes the pane back to its resting Games. The right-tabs mode keeps the chip bar.
  const paneBar =
    chatsMode === 'list' ? (
      openId !== null && panel === null ? (
        <PaneBar
          title={chipTitleOf(openId, items, rooms, accountId, profiles)}
          // The close is a leave for rooms and groups here too: the gesture is the same one
          // the chip's X makes in the tab mode, and what closing means should not change
          // with the display setting.
          onClose={() => {
            leaveWhenClosed(openId);
            closeConversation();
          }}
          onBackToLists={() => setDismissed(true)}
        />
      ) : panel !== null ? (
        <PaneBar
          title={KIND_TITLES[panel]}
          onClose={() => setPanel(null)}
          onBackToLists={() => setDismissed(true)}
        />
      ) : null
    ) : undefined;

  return (
    <SectionNavProvider navigate={navigate}>
      <AppShell
        leftTab={leftTab}
        leftContent={leftContent}
        rightTabs={chatChips}
        activeRight={right.active}
        activeChat={activeChat}
        rightContent={rightContent}
        showRight={showRight}
        onSelectSystem={setLeftTab}
        onSelectRight={selectRight}
        onCloseRight={closeRight}
        onBackToLists={() => setDismissed(true)}
        onOpenPanel={openPanel}
        chatsUnread={chatsUnread}
        chatsTabHidden={chatsMode === 'right'}
        rightBarOverride={paneBar}
      >
        {children}
      </AppShell>
      <ConnectionSnackbar />
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
