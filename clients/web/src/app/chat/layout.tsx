'use client';

import type { ReactNode } from 'react';

import { RequireReady } from '@/components/require-ready.js';
import { Sidebar } from '@/components/sidebar.js';
import { ConversationsProvider } from '@/lib/migo/conversations-provider.js';
import { useOpenConversation } from '@/lib/migo/use-open-conversation.js';

/**
 * The authenticated two-pane shell: a persistent sidebar and the open thread.
 *
 * On narrow screens the two panes collapse to one; `has-thread` tells the CSS to show the thread pane
 * (and hide the sidebar) whenever a conversation is open, which the URL fragment records.
 */
export default function ChatLayout({ children }: { children: ReactNode }): ReactNode {
  const hasThread = useOpenConversation() !== null;

  return (
    <RequireReady>
      <ConversationsProvider>
        <div className={`shell ${hasThread ? 'has-thread' : ''}`}>
          <Sidebar />
          <main className="thread-area">{children}</main>
        </div>
      </ConversationsProvider>
    </RequireReady>
  );
}
