'use client';

/**
 * The account's own presence and status, as one publishable state.
 *
 * Presence is a publish, not a store: the me bar (the desktop's contacts window and the phone's
 * home card are the two that carry it) holds the current state locally, seeds it once from the
 * profile the cache already resolved, and performs the wire call on change — never optimistically
 * keeping a presence the server refused. Both surfaces need exactly this, so it lives here rather
 * than twice.
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
  /** Publishes the presence and the status together — the wire carries them as one call. */
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
      setPresence(state);
      setStatus(next);
      if (!client) {
        return;
      }
      void client.presence
        .setPresence(state, next.trim().length > 0 ? { customStatus: next } : {})
        .catch(() => {});
    },
    [client],
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
