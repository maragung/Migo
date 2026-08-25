'use client';

import { useState } from 'react';
import type { ReactNode } from 'react';

import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { ConnectionBadge } from './connection-badge.js';
import { ConversationList } from './conversation-list.js';
import { NewConversationDialog } from './new-conversation-dialog.js';

/** The persistent left column: brand header, connection status, conversation list, and account footer. */
export function Sidebar(): ReactNode {
  const { accountId, logout } = useMigo();
  const self = useProfile(accountId);
  const [dialogOpen, setDialogOpen] = useState(false);

  return (
    <aside className="sidebar">
      <header className="sidebar-header">
        <div className="brand">
          <span className="brand-mark">◆</span>
          <span className="brand-name">Migo</span>
        </div>
        <button
          type="button"
          className="icon-btn"
          aria-label="New conversation"
          title="New conversation"
          onClick={() => setDialogOpen(true)}
        >
          ＋
        </button>
      </header>

      <ConnectionBadge />

      <ConversationList />

      <footer className="sidebar-footer">
        <Avatar
          name={self?.displayName ?? 'You'}
          id={accountId ?? 'self'}
          size={36}
          avatarUrl={self?.avatarUrl}
        />
        <div className="sidebar-me">
          <div className="name">{self?.displayName ?? 'You'}</div>
          <div className="muted">{self?.username ? `@${self.username}` : 'Signed in'}</div>
        </div>
        <button
          type="button"
          className="icon-btn"
          aria-label="Sign out"
          title="Sign out"
          onClick={() => void logout()}
        >
          ⏻
        </button>
      </footer>

      {dialogOpen ? <NewConversationDialog onClose={() => setDialogOpen(false)} /> : null}
    </aside>
  );
}
