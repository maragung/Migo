'use client';

/**
 * The authenticated shell: one app, ten sections, three compositions.
 *
 * The layout owns the section state and hands it to {@link AppShell}, which composes the rail,
 * the mobile header, and the bottom bar around whichever section is current. The chats section
 * keeps its own two-pane shell — conversation list plus thread, collapsing to one pane on
 * narrow screens via `has-thread` — now rendered as one section among several; every other
 * section renders its panel through the shared `.panel-area`. Section state is plain client
 * state rather than routes: the bundle is a static export, the open conversation already lives
 * in the URL fragment, and a section switch should neither unload the session nor touch the
 * URL. Cross-section flows (joining a room from Home or Search, a $MIG reference opening the
 * wallet) hand the intent back through the {@link SectionNavProvider} context.
 *
 * The rooms provider sits inside the conversations provider because the two lists describe the
 * same objects from two sides: a join notes the room in both, and the sidebar's room rows and
 * the thread header read the room record back out.
 *
 * The call manager wraps everything under the session gate — a call can start from a thread
 * header and must keep ringing across section switches — and the overlay it feeds renders after
 * the app div so a live call sits over the whole shell, not inside one pane of it.
 */

import { useCallback, useState } from 'react';
import type { ReactNode } from 'react';

import { AppShell } from '@/components/app-shell.js';
import type { AppTab } from '@/components/app-shell.js';
import { FriendsPanel } from '@/components/friends-panel.js';
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
import { openConversation, useOpenConversation } from '@/lib/migo/use-open-conversation.js';
import { SectionNavProvider } from '@/lib/migo/section-nav.js';

export default function ChatLayout({ children }: { children: ReactNode }): ReactNode {
  const hasThread = useOpenConversation() !== null;
  // A session opens on its conversations: the messenger's own first screen, not a dashboard.
  const [tab, setTab] = useState<AppTab>('chats');

  // The cross-section flow every "open this conversation" surface shares: switch first so the
  // chats shell is mounted when the fragment lands, then let the thread's own hook subscribe
  // and replay history as for any conversation.
  const openInChats = useCallback(
    (conversationId: Parameters<typeof openConversation>[0]): void => {
      setTab('chats');
      openConversation(conversationId);
    },
    [],
  );

  const section = (() => {
    switch (tab) {
      case 'chats':
        return (
          <div className={`shell ${hasThread ? 'has-thread' : ''}`}>
            <Sidebar />
            <main className="thread-area">{children}</main>
          </div>
        );
      case 'rooms':
        return <RoomsPanel onOpenConversation={openInChats} />;
      case 'space':
        return <SpacePanel onOpenConversation={openInChats} />;
      case 'friends':
        return <FriendsPanel onOpenConversation={openInChats} />;
      case 'notifications':
        return <NotificationsPanel />;
      case 'search':
        return <SearchPanel onOpenConversation={openInChats} />;
      case 'wallet':
        return <WalletPanel />;
      case 'profile':
        return <ProfilePanel onOpenSettings={() => setTab('settings')} />;
      case 'settings':
        return <SettingsPanel />;
    }
  })();

  return (
    <RequireReady>
      <ConversationsProvider>
        <RoomsProvider>
          <CallManagerProvider>
            <SectionNavProvider navigate={setTab}>
              <AppShell active={tab} onSelect={setTab} hasThread={hasThread && tab === 'chats'}>
                {section}
              </AppShell>
              <CallOverlay />
            </SectionNavProvider>
          </CallManagerProvider>
        </RoomsProvider>
      </ConversationsProvider>
    </RequireReady>
  );
}
