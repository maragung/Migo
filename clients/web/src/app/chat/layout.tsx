'use client';

import { useState } from 'react';
import type { ReactNode } from 'react';

import { DiscoverPanel } from '@/components/discover-panel.js';
import { FriendsPanel } from '@/components/friends-panel.js';
import { GiftsPanel } from '@/components/gifts-panel.js';
import { NotificationsPanel } from '@/components/notifications-panel.js';
import { ProfilePanel } from '@/components/profile-panel.js';
import { RequireReady } from '@/components/require-ready.js';
import { Sidebar } from '@/components/sidebar.js';
import { TabRail } from '@/components/tab-rail.js';
import type { AppTab } from '@/components/tab-rail.js';
import { ConversationsProvider } from '@/lib/migo/conversations-provider.js';
import { RoomsProvider } from '@/lib/migo/rooms-provider.js';
import { CallManagerProvider } from '@/lib/migo/call-manager.js';
import { openConversation, useOpenConversation } from '@/lib/migo/use-open-conversation.js';
import { CallOverlay } from '@/components/call-overlay.js';

/**
 * The authenticated shell: a navigation rail and, per section, the chats two-pane view or a panel.
 *
 * The chats view is the original shell untouched — sidebar plus thread pane, still collapsing to one
 * pane on narrow screens via `has-thread` — now rendered as one section among several. Section state
 * is plain client state rather than routes: the bundle is a static export, the open conversation
 * already lives in the URL fragment, and a section switch should neither unload the session nor
 * touch the URL. The one cross-section flow, joining a room in Discover, hands the opened
 * conversation back through a callback that switches to the chats section.
 *
 * The rooms provider sits inside the conversations provider because the two lists describe the
 * same objects from two sides: a join notes the room in both, and the sidebar's room rows and the
 * thread header read the room record back out.
 *
 * The call manager wraps everything under the session gate — a call can start from a thread header
 * and must keep ringing across section switches — and the overlay it feeds renders after the app
 * div so a live call sits over the whole shell, not inside one pane of it.
 */
export default function ChatLayout({ children }: { children: ReactNode }): ReactNode {
  const hasThread = useOpenConversation() !== null;
  const [tab, setTab] = useState<AppTab>('chats');

  return (
    <RequireReady>
      <ConversationsProvider>
        <RoomsProvider>
          <CallManagerProvider>
            <div className="app">
              <TabRail active={tab} onSelect={setTab} />
              {tab === 'chats' ? (
                <div className={`shell ${hasThread ? 'has-thread' : ''}`}>
                  <Sidebar />
                  <main className="thread-area">{children}</main>
                </div>
              ) : (
                <main className="panel-area">
                  {tab === 'friends' ? (
                    <FriendsPanel />
                  ) : tab === 'notifications' ? (
                    <NotificationsPanel />
                  ) : tab === 'gifts' ? (
                    <GiftsPanel />
                  ) : tab === 'profile' ? (
                    <ProfilePanel />
                  ) : (
                    <DiscoverPanel
                      onOpenConversation={(conversationId) => {
                        // Switch first so the shell is mounted when the fragment lands; the thread's own
                        // hook then subscribes and replays history as for any conversation.
                        setTab('chats');
                        openConversation(conversationId);
                      }}
                    />
                  )}
                </main>
              )}
            </div>
            <CallOverlay />
          </CallManagerProvider>
        </RoomsProvider>
      </ConversationsProvider>
    </RequireReady>
  );
}
