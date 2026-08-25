'use client';

/**
 * Tracks live presence for a set of accounts.
 *
 * Seeds from any presence already on a fetched profile, then follows the presence stream for updates.
 * Subscribing to a user's presence topic is the caller's responsibility (the client subscribes its own;
 * a conversation's members are watched when their profiles are shown). This hook only listens.
 */

import { useEffect, useState } from 'react';

import { PresenceState } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { useMigo } from './use-migo.js';

export function usePresence(): Map<Id, PresenceState> {
  const { client } = useMigo();
  const [presence, setPresence] = useState<Map<Id, PresenceState>>(new Map());

  useEffect(() => {
    if (!client) {
      return;
    }
    const off = client.presence.onPresence((event) => {
      setPresence((prev) => {
        const next = new Map(prev);
        next.set(event.userId, event.state);
        return next;
      });
    });
    return off;
  }, [client]);

  return presence;
}

/** A short label for a presence state. */
export function presenceLabel(state: PresenceState | undefined): string {
  switch (state) {
    case PresenceState.Online:
      return 'Online';
    case PresenceState.Away:
      return 'Away';
    case PresenceState.Busy:
      return 'Busy';
    case PresenceState.Offline:
      return 'Offline';
    default:
      return '';
  }
}
