'use client';

/**
 * The authenticated shell: a left panel of lists, a right panel of tabs.
 *
 * The new-ui-02 IA (docs/design mockup `new-ui-02.tsx`) is a split: the LEFT panel holds the
 * account's lists behind its own tab state (`leftTab`), and the RIGHT panel shows what the left
 * panel's clicks open, as closable tabs (`right`): a conversation (private, group, or room), the
 * games arcade, or a secondary panel the banner menu or a deep link reached. The Feed is not a
 * tab but the pane's resting chip — always first, never closed — so a pane with nothing open
 * shows the Feed, exactly the fallback an empty state owes. The two panels' states are
 * independent on purpose: reading Games on the left never disturbs the thread open on the right,
 * which is the model's whole offer. Below the PC breakpoint the panes take turns — the right
 * pane covers the phone while it has something to show, and `dismissed` remembers that its back
 * chevron sent the screen home to the lists without closing a thing.
 *
 * The URL fragment stays the single source of truth for the open conversation (see
 * use-open-conversation.ts): every door into a thread — a conversation row, a room join, a
 * friend's row, a deep link — is `openConversation(id)`, and the fragment's effect below both
 * adds the chip and activates it. Clearing the fragment (Back, or the open thread's chip
 * closing) removes the thread's tab and falls through to the most recent remaining one, else
 * the Feed — Back and the close button can never disagree about what is open.
 *
 * The rooms provider sits inside the conversations provider because the two lists describe the
 * same objects from two sides: a join notes the room in both, and the sidebar's room rows and
 * the thread header read the room record back out.
 *
 * The call manager wraps everything under the session gate — a call can start from a thread
 * header and must keep ringing across pane switches — and the overlay it feeds renders after the
 * app div so a live call sits over the whole shell, not inside one pane of it.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id } from '@migo/sdk';

import { AppShell } from '@/components/app-shell.js';
import type { PanelTab, SystemTab } from '@/components/app-shell.js';
import type { RightTabChip, RightTabKind } from '@/components/app-shell.js';
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

/** One open thing on the right pane; the chip's title is derived per render. */
interface RightTabItem {
  /** `chat:<conversation>` for a thread, the kind itself for one-per-kind tabs. */
  id: string;
  kind: RightTabKind;
  /** The conversation a chat tab shows. */
  conversationId?: Id;
}

/** The right pane's whole state, in one object so no transition can tear it. */
interface RightPaneState {
  tabs: RightTabItem[];
  /** `'feed'`, or the id of the tab the pane is showing. */
  active: string;
}

/** The chip titles of the one-per-kind tabs; chat chips derive theirs from the live lists. */
const KIND_TITLES: Readonly<Record<Exclude<RightTabKind, 'chat'>, string>> = {
  games: 'Games',
  notifications: 'Alerts',
  search: 'Search',
  wallet: 'TopUp',
  profile: 'Profile',
  settings: 'Settings',
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
  const { accountId } = useMigo();
  const openId = useOpenConversation();
  const { items } = useConversations();
  const rooms = useRooms();

  // A session opens on its people: the mockup's first tab is Main (the friends list).
  const [leftTab, setLeftTab] = useState<SystemTab>('friends');
  const [right, setRight] = useState<RightPaneState>({ tabs: [], active: 'feed' });
  // The right pane's back chevron (single-column only): the pane keeps its tabs, the screen
  // goes home to the lists until the next chip opens or activates.
  const [dismissed, setDismissed] = useState(false);

  // The previous fragment, so a cleared fragment can remove exactly the thread it named.
  const prevOpenRef = useRef<Id | null>(null);

  // The fragment effect: every door into a thread lands here, whatever opened it. Clearing the
  // fragment (Back, the chip's close) lands here too, and takes the thread's tab with it.
  useEffect(() => {
    const prev = prevOpenRef.current;
    prevOpenRef.current = openId;
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
  }, [openId]);

  /** Opens (or activates) a one-per-kind tab: the arcade, a secondary panel. */
  const openRightTab = useCallback((kind: Exclude<RightTabKind, 'chat'>): void => {
    setRight((s) => ({
      tabs: s.tabs.some((tab) => tab.id === kind) ? s.tabs : [...s.tabs, { id: kind, kind }],
      active: kind,
    }));
    setDismissed(false);
  }, []);

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

  const closeRight = useCallback(
    (id: string): void => {
      const tab = right.tabs.find((item) => item.id === id);
      setRight((s) => {
        const tabs = s.tabs.filter((item) => item.id !== id);
        const active = s.active === id ? (tabs[tabs.length - 1]?.id ?? 'feed') : s.active;
        return { tabs, active };
      });
      if (tab?.conversationId !== undefined && tab.conversationId === openId) {
        // The open thread's chip: the fragment is cleared, and its effect is a no-op on a tab
        // already removed — the fall-through above is the one that ran.
        closeConversation();
      }
    },
    [openId, right.tabs],
  );

  // The cross-section navigation every deep surface shares: the system tabs drive the left
  // panel, the secondary panels arrive as the right pane's tabs.
  const navigate = useCallback(
    (tab: SystemTab | PanelTab): void => {
      if (tab === 'friends' || tab === 'rooms' || tab === 'games' || tab === 'feed') {
        setLeftTab(tab);
        return;
      }
      openRightTab(tab);
    },
    [openRightTab],
  );

  const openInTab = useCallback((conversationId: Id): void => {
    openConversation(conversationId);
  }, []);

  // The chips' titles: a room is its #name, a direct chat is the peer's display name — the
  // same derivation the conversation rows use, read from the same sources.
  const peerIds = right.tabs.flatMap((tab) => {
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

  // What the pane is showing: the thread (the route's own page) for a chat tab, the owner's
  // content for everything else — the Feed included, as the pane's resting tab.
  const activeItem =
    right.active === 'feed' ? null : (right.tabs.find((tab) => tab.id === right.active) ?? null);
  const activeChat = activeItem?.kind === 'chat' ? (activeItem.conversationId ?? null) : null;
  const rightContent = (() => {
    switch (activeItem?.kind ?? 'feed') {
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
        return <ProfilePanel onOpenSettings={() => openRightTab('settings')} />;
      case 'settings':
        return <SettingsPanel />;
      case 'chat':
        return null;
    }
  })();

  const leftContent = (() => {
    switch (leftTab) {
      case 'friends':
        return <FriendsPanel onOpenConversation={openInTab} />;
      case 'rooms':
        return <RoomsPanel onOpenConversation={openInTab} />;
      case 'games':
        return <GamesPanel onActivate={() => openRightTab('games')} />;
      case 'feed':
        return <SpacePanel onOpenConversation={openInTab} />;
    }
  })();

  return (
    <SectionNavProvider navigate={navigate}>
      <AppShell
        leftTab={leftTab}
        leftContent={leftContent}
        rightTabs={chatChips}
        activeRight={right.active}
        activeChat={activeChat}
        rightContent={rightContent}
        showRight={!dismissed && !(right.tabs.length === 0 && right.active === 'feed')}
        onSelectSystem={setLeftTab}
        onSelectRight={selectRight}
        onCloseRight={closeRight}
        onBackToLists={() => setDismissed(true)}
        onOpenPanel={openRightTab}
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
