'use client';

/**
 * The left panel's footer band: the account's credit balance and what the open list responds to.
 *
 * The reference design closes both its windows — the phone's home and the PC's contacts panel —
 * with the same pale band: a gold coin and "Credits: N" on the left, a small grey hint on the
 * right that changes with the tab ("double-click to chat", "Migo activity"). It is a status bar in
 * the oldest sense: the two things you want at a glance and never want to go looking for.
 *
 * The balance lives *here* rather than in the profile banner because that is where the design puts
 * it, and because a figure stated twice in one column is a figure someone has to reconcile. The
 * banner above owns who you are; the band below owns what you have.
 *
 * The read is one round trip per session — the resting glance, not the ledger — and a failed read
 * leaves the half empty rather than showing a zero the wallet never reported. The Wallet panel is
 * the place that refetches, and it is one click away through the banner menu.
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { useMigo } from '@/lib/migo/use-migo.js';

import { CoinMark } from './icons.js';
import type { SystemTab } from './tab-strip.js';

/**
 * What each list responds to, in the design's own register: a lowercase aside, not an instruction.
 *
 * The gesture named is the one the row actually implements — a hint for an affordance that is not
 * there would be worse than no hint, so each of these is the row's real primary action.
 */
const TAB_HINTS: Readonly<Record<SystemTab, string>> = {
  friends: 'click a friend to open a chat',
  chats: 'click a conversation to open it',
  rooms: 'click a room to join and open it',
  feed: 'Migo activity',
};

/**
 * The band itself.
 *
 * @param tab The left panel's active list, which chooses the hint.
 */
export function ListFooter({ tab }: { tab: SystemTab }): ReactNode {
  const { client } = useMigo();
  const [coins, setCoins] = useState<number | null>(null);

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

  return (
    <div className="list-footer">
      <span className="list-footer-credits">
        <CoinMark size={14} />
        {/* An unread balance says nothing rather than zero: a wallet that failed to load is not an
            empty one, and the difference matters to whoever is about to spend. */}
        <span>{coins !== null ? `Credits: ${coins.toLocaleString()}` : 'Credits'}</span>
      </span>
      <span className="list-footer-hint">{TAB_HINTS[tab]}</span>
    </div>
  );
}
