'use client';

/**
 * Resolves account ids to public profiles, with a process-wide cache.
 *
 * Profiles are public, so caching them across components and across the session is safe and avoids
 * refetching the same display name repeatedly. A hook requests the ids it needs; any not already cached
 * are fetched once (in-flight ids are tracked so concurrent hooks do not duplicate the request).
 */

import { useEffect, useState } from 'react';

import type { Id, UserProfile } from '@migo/sdk';

import { useMigo } from './use-migo.js';

const cache = new Map<Id, UserProfile>();
const inFlight = new Set<Id>();

export function useProfiles(ids: readonly Id[]): Map<Id, UserProfile> {
  const { client } = useMigo();
  const [, setVersion] = useState(0);

  // A stable key so the effect runs only when the actual set of ids changes.
  const key = [...ids].sort().join(',');

  useEffect(() => {
    if (!client) {
      return;
    }
    const missing = ids.filter((id) => !cache.has(id) && !inFlight.has(id));
    if (missing.length === 0) {
      return;
    }
    for (const id of missing) {
      inFlight.add(id);
    }
    let cancelled = false;
    void client.profile
      .fetch(missing)
      .then((profiles) => {
        for (const profile of profiles) {
          cache.set(profile.userId, profile);
        }
      })
      .catch(() => {
        // Leave uncached; a later render retries.
      })
      .finally(() => {
        for (const id of missing) {
          inFlight.delete(id);
        }
        if (!cancelled) {
          setVersion((value) => value + 1);
        }
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, key]);

  const result = new Map<Id, UserProfile>();
  for (const id of ids) {
    const profile = cache.get(id);
    if (profile) {
      result.set(id, profile);
    }
  }
  return result;
}

/** Convenience for a single id. */
export function useProfile(id: Id | null): UserProfile | null {
  const map = useProfiles(id ? [id] : []);
  return id ? (map.get(id) ?? null) : null;
}
