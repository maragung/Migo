'use client';

/**
 * The caller's personal mute set, and the filter that acts on it.
 *
 * A mute is a personal, one-sided choice (see {@link SocialDomain.muteUser}): it hides a muted
 * account's *room* chatter for the muter, and nothing more. It does not block delivery, does not
 * touch a friendship, and — the line this provider draws — does not touch direct messages: a person
 * you muted in the rooms can still reach you one to one, and that thread is never filtered. Muting
 * is for the noise of a crowd, not for cutting someone off; cutting off is a block.
 *
 * The set is server-owned (it lives in the relationship graph as Mute edges), so this provider reads
 * it once the session is ready and re-reads it on a session reset, rather than trying to keep a
 * local mirror in step across devices. A mute or unmute made here updates the local set the moment
 * the server accepts it, so the UI reflects the choice without waiting for a round-trip re-read.
 */

import { createContext, useCallback, useContext, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { useMigo } from './use-migo.js';

/**
 * Hides muted senders from a message list.
 *
 * Pure, so a test can pin it. An empty set is the common case and returns the input array
 * unchanged — same reference — so a caller's memo does not churn when nothing is muted. Only the
 * caller decides *where* this runs: it is applied to room transcripts, never to direct messages.
 */
export function muteFilter<T extends { senderId: Id }>(
  messages: readonly T[],
  muted: ReadonlySet<Id>,
): readonly T[] {
  if (muted.size === 0) {
    return messages;
  }
  return messages.filter((message) => !muted.has(message.senderId));
}

export interface MutedContextValue {
  /** The accounts the caller has muted; membership is what {@link muteFilter} checks. */
  muted: ReadonlySet<Id>;
  /** True when this account's room chatter is hidden for the caller. */
  isMuted: (userId: Id) => boolean;
  /** Mutes (`on`) or unmutes an account, reflecting it locally once the server accepts. */
  setMuted: (userId: Id, on: boolean) => Promise<void>;
}

const MutedContext = createContext<MutedContextValue | null>(null);

export function MutedProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, status, resetNonce } = useMigo();
  const [muted, setMutedState] = useState<ReadonlySet<Id>>(new Set());

  // The set is read once the session is ready, and again after a reset re-reads the graph. A failed
  // read leaves the set empty — hiding nothing is the safe default, and the next mute reconciles it.
  useEffect(() => {
    if (!client || status !== 'ready') {
      return;
    }
    let cancelled = false;
    client.social
      .mutedAccounts()
      .then((ids) => {
        if (!cancelled) {
          setMutedState(new Set(ids));
        }
      })
      .catch(() => {
        // See above: an empty set is the safe default.
      });
    return () => {
      cancelled = true;
    };
  }, [client, status, resetNonce]);

  const setMuted = useCallback(
    async (userId: Id, on: boolean): Promise<void> => {
      if (!client) {
        return;
      }
      await client.social.muteUser(userId, on);
      setMutedState((prev) => {
        const next = new Set(prev);
        if (on) {
          next.add(userId);
        } else {
          next.delete(userId);
        }
        return next;
      });
    },
    [client],
  );

  const isMuted = useCallback((userId: Id): boolean => muted.has(userId), [muted]);

  const value: MutedContextValue = { muted, isMuted, setMuted };
  return <MutedContext.Provider value={value}>{children}</MutedContext.Provider>;
}

export function useMuted(): MutedContextValue {
  const value = useContext(MutedContext);
  if (value === null) {
    throw new Error('useMuted must be used within a MutedProvider');
  }
  return value;
}
