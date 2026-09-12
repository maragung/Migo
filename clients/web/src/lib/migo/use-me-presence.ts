'use client';

/**
 * The account's own presence and status, as one publishable state.
 *
 * Presence is a publish, not a store: the me bar (the desktop's contacts window and the phone's
 * home card are the two that carry it) holds the current state locally, seeds it once from the
 * profile the cache already resolved, and performs the wire call on change. Both surfaces need
 * exactly this, so it lives here rather than twice.
 *
 * The two halves travel on two wires, because the server keeps them in two places. The presence
 * state is a presence entry, which evaporates with the cache; the free-text status is a profile
 * column that outlives the session — and presence refuses that field outright, so the profile patch
 * is the only way to write one. {@link publish} therefore writes each half to its own home, and a
 * refused write is dropped rather than retried: the bars publish again on the next change, and the
 * status is re-seeded from the profile the cache holds the next time this hook mounts.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

import { PresenceState } from '@migo/sdk';
import type { PresenceState as PresenceStateValue } from '@migo/sdk';

import { useMigo } from './use-migo.js';
import { useProfile } from './use-profiles.js';

/** What the me bar renders and publishes. */
export interface MeState {
  displayName: string;
  username: string;
  avatarUrl: string | undefined;
  presence: PresenceStateValue;
  status: string;
  /** Publishes the presence state and the status, each to the wire that stores it. */
  publish: (state: PresenceStateValue, status: string) => void;
}

/** The account's own presence, status, and name, as the me bars render them. */
export function useMePresence(): MeState {
  const { client, accountId } = useMigo();
  const self = useProfile(accountId);

  const [presence, setPresence] = useState<PresenceStateValue>(PresenceState.Online);
  const [status, setStatus] = useState('');
  const seeded = useRef(false);

  // The profile the cache resolved seeds both halves once: a returning session says what it said
  // yesterday rather than offering an empty box beside a profile that plainly has one.
  useEffect(() => {
    if (seeded.current || self === null) {
      return;
    }
    seeded.current = true;
    setStatus(self.customStatus ?? '');
    if (self.presence !== undefined && self.presence !== PresenceState.Unknown) {
      setPresence(self.presence);
    }
  }, [self]);

  const publish = useCallback(
    (state: PresenceStateValue, next: string): void => {
      const trimmed = next.trim();
      // The away toggle publishes the status it already has, so the two halves move independently
      // here: the state always goes out, the status only when the box actually moved.
      const statusMoved = trimmed !== status;
      setPresence(state);
      setStatus(trimmed);
      if (!client) {
        return;
      }
      void client.presence.setPresence(state).catch(() => {});
      if (statusMoved) {
        void client.profile.updateProfile({ customStatus: trimmed }).catch(() => {});
      }
    },
    [client, status],
  );

  return {
    displayName: self?.displayName ?? 'You',
    username: self?.username ?? '',
    avatarUrl: self?.avatarUrl,
    presence,
    status,
    publish,
  };
}
