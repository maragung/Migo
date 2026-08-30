'use client';

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { PresenceState } from '@migo/sdk';

import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { ConnectionBadge } from './connection-badge.js';
import { ConversationList } from './conversation-list.js';
import { NewConversationDialog } from './new-conversation-dialog.js';
import { PresencePicker } from './presence-picker.js';

/**
 * The persistent left column: brand header, connection status, conversation list, and account footer.
 *
 * The footer is also where the account's own presence is published from ({@link PresencePicker}):
 * the sidebar holds the current state and performs the `setPresence` call, so the picker stays a
 * controlled view of the truth rather than a second source of it. The header's coin badge is the
 * wallet read once per session ({@link EconomyDomain.getBalance}) — the Gifts tab owns the live
 * money-side refresh after a spend, so the badge is the resting glance, not the ledger.
 */
export function Sidebar(): ReactNode {
  const { client, accountId, logout } = useMigo();
  const self = useProfile(accountId);
  const [dialogOpen, setDialogOpen] = useState(false);

  const [presence, setPresence] = useState<PresenceState>(PresenceState.Online);
  const [status, setStatus] = useState('');
  const [coins, setCoins] = useState<number | null>(null);

  // The wallet badge: one read per session. A failure leaves the badge absent rather than wrong.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.economy
      .getBalance()
      .then((wallet) => {
        if (!cancelled) {
          setCoins(wallet.balance);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client]);

  const onPresenceChange = useCallback(
    (state: PresenceState, nextStatus: string): void => {
      if (!client) {
        return;
      }
      setPresence(state);
      setStatus(nextStatus);
      void client.presence
        .setPresence(state, nextStatus.trim().length > 0 ? { customStatus: nextStatus } : {})
        .catch(() => {});
    },
    [client],
  );

  return (
    <aside className="sidebar">
      <header className="sidebar-header">
        <div className="brand">
          <span className="brand-mark">◆</span>
          <span className="brand-name">Migo</span>
        </div>
        {coins !== null ? (
          <span className="coin-badge" title="Coin balance" aria-label={`Coin balance: ${coins}`}>
            🪙 {coins}
          </span>
        ) : null}
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
        <div className="sidebar-self">
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
        </div>
        <PresencePicker state={presence} status={status} onChange={onPresenceChange} />
      </footer>

      {dialogOpen ? <NewConversationDialog onClose={() => setDialogOpen(false)} /> : null}
    </aside>
  );
}
