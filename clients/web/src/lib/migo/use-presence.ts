'use client';

/**
 * Tracks live presence for a set of accounts.
 *
 * Seeds from any presence already on a fetched profile, then follows the presence stream for updates.
 * Subscribing to a user's presence topic is the caller's responsibility (the client subscribes its own;
 * a conversation's members are watched when their profiles are shown). This hook only listens.
 */

import { useEffect, useMemo, useState } from 'react';

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

/**
 * The presence of a known set of accounts: seeded, subscribed, and live.
 *
 * Presence is the messenger's ambient information — the spec asks for it everywhere people
 * appear — and this hook is the one honest way to get it for a list: each account's user topic
 * is subscribed (the live stream), the profiles' own presence field seeds the map before any
 * event arrives (the server's last statement), and the live stream overrides the seed from then
 * on. Profiles are optional: a caller that has none gets the live half alone.
 */
export function usePresenceOf(
  ids: readonly Id[],
  profiles?: ReadonlyMap<Id, { presence?: PresenceState }>,
): Map<Id, PresenceState> {
  const { client } = useMigo();
  const live = usePresence();
  // One stable key so the subscription effect runs only when the actual set changes.
  const key = useMemo(() => [...ids].sort().join(','), [ids]);

  // Subscribe the whole set in ONE SUBSCRIBE frame — one frame per friend is exactly the burst
  // the rate limiter prices worst. The SDK re-subscribes across a session reset.
  useEffect(() => {
    if (!client || ids.length === 0) {
      return;
    }
    void client.watchUsers(ids).catch(() => {
      // A refusal (privacy, a capped subscription set) leaves the seed standing.
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, key]);

  return useMemo(() => {
    const merged = new Map<Id, PresenceState>();
    if (profiles !== undefined) {
      for (const id of ids) {
        const seed = profiles.get(id)?.presence;
        if (seed !== undefined) {
          merged.set(id, seed);
        }
      }
    }
    for (const [id, state] of live) {
      merged.set(id, state);
    }
    return merged;
    // `live` carries every update; `ids`/`profiles` re-derive on their own changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [live, key, profiles]);
}
