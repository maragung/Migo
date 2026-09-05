'use client';

/**
 * The authenticated shell's provider stack.
 *
 * The shell itself is the window manager (see components/app-shell.tsx): a desk of windows on a
 * PC, a tab strip on a phone. What lives here is only what must wrap it —
 *
 * The rooms provider sits inside the conversations provider because the two lists describe the
 * same objects from two sides: a join notes the room in both, and the contacts window's room rows
 * and the thread header read the room record back out. The call manager wraps everything under
 * the session gate — a call can start from a thread header and must keep ringing across window
 * switches — and the overlay it feeds renders after the desk so a live call sits over the whole
 * shell, not inside one window. The connection Snackbar sits at the same level for the same
 * reason: "Reconnecting…" is news about the session, not about whichever window is showing.
 */

import type { ReactNode } from 'react';

import { AppShell } from '@/components/app-shell.js';
import { CallOverlay } from '@/components/call-overlay.js';
import { ConnectionSnackbar } from '@/components/connection-snackbar.js';
import { RequireReady } from '@/components/require-ready.js';
import { CallManagerProvider } from '@/lib/migo/call-manager.js';
import { ConversationsProvider } from '@/lib/migo/conversations-provider.js';
import { MutedProvider } from '@/lib/migo/muted-provider.js';
import { RoomsProvider } from '@/lib/migo/rooms-provider.js';

export default function ChatLayout({ children }: { children: ReactNode }): ReactNode {
  return (
    <RequireReady>
      <ConversationsProvider>
        <RoomsProvider>
          <MutedProvider>
            <CallManagerProvider>
              {/* The route's own page renders nothing: the thread a conversation opens is the
                  shell's to draw, inside that conversation's window. */}
              {children}
              <AppShell />
              <ConnectionSnackbar />
              <CallOverlay />
            </CallManagerProvider>
          </MutedProvider>
        </RoomsProvider>
      </ConversationsProvider>
    </RequireReady>
  );
}
