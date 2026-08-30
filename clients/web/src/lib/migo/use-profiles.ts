'use client';

/**
 * Resolves account ids to public profiles, with a process-wide cache.
 *
 * Profiles are public, so caching them across components and across the session is safe and avoids
 * refetching the same display name repeatedly. A hook requests the ids it needs; any not already
 * cached are fetched once (in-flight ids are tracked so concurrent hooks do not duplicate the
 * request), and every cache write notifies the mounted hooks — so a profile that arrived after a
 * component rendered, or an avatar that resolved after its profile, still shows.
 *
 * # The avatar is a media object, not a URL
 *
 * The wire names a profile's avatar as a media object id (`UserProfile`'s avatar media field);
 * a fetchable URL is minted per session through the media domain's download, deduped by the
 * session-wide URL cache in media.ts. Each cached profile therefore carries `avatarUrl` —
 * undefined while the URL is resolving or when there is no avatar, in which case renderers fall
 * back to initials rather than a broken image. Names land in the cache the moment a profile is
 * fetched; the avatar resolves behind the write and re-notifies, so a name never waits on its
 * picture.
 *
 * # The migration seam
 *
 * The avatar field is mid-regeneration on the wire (`avatar_url`, a string, becomes
 * `avatar_media_id`, an id). {@link avatarMediaIdOf} reads it through a shape that compiles
 * against either side of that change and reads the real field once the regenerated protocol
 * lands; the seam can then be removed.
 */

import { useEffect, useState } from 'react';

import type { Id, UserProfile } from '@migo/sdk';

import { resolveMediaUrl } from './media.js';
import type { MediaClient } from './media.js';
import { useMigo } from './use-migo.js';

/**
 * A profile as this client renders it: the wire's fields plus the avatar's resolved fetch URL.
 * `undefined` means "no avatar, or not resolved yet" — the initials fallback, never a broken img.
 */
export interface ResolvedProfile extends UserProfile {
  avatarUrl: string | undefined;
}

/**
 * The slice of the client the profile cache reads: the batched profile lookup itself. Avatar
 * resolution takes the media slice ({@link MediaClient}), so a caller (or a test) can supply a
 * double with just the two halves rather than a whole client.
 */
export interface ProfileClient {
  readonly profile: {
    fetch(userIds: Id[]): Promise<UserProfile[]>;
  };
}

/**
 * The avatar media id a profile names, read through a shape that compiles whichever side of the
 * wire's avatar-field regeneration supplied the profile. Once the regenerated protocol is in,
 * this reads the real field and the seam can be removed.
 */
type ProfileWithAvatarMediaId = UserProfile & { avatarMediaId?: Id };

/** The avatar media id a profile names, or `undefined` when the profile has no avatar. */
export function avatarMediaIdOf(profile: UserProfile): Id | undefined {
  return (profile as ProfileWithAvatarMediaId).avatarMediaId;
}

const cache = new Map<Id, ResolvedProfile>();
const inFlight = new Set<Id>();
/** Every mounted hook, notified on any cache write so a late-arriving profile or avatar still renders. */
const listeners = new Set<() => void>();

function writeCache(profile: ResolvedProfile): void {
  cache.set(profile.userId, profile);
  for (const listener of listeners) {
    listener();
  }
}

/**
 * Caches a profile the caller already holds (a fetch's reply, an update's reply) and resolves its
 * avatar.
 *
 * The profile lands in the cache the moment this is called — names must not wait on their
 * pictures — and the returned promise settles with the finished entry once the avatar URL has.
 * A failed resolution is not a verdict: the entry keeps its initials fallback and the next fetch
 * retries, because a media server briefly unavailable says nothing about the object. A resolution
 * that lands after a *newer* profile replaced this one is dropped, never grafted onto it.
 */
export async function cacheProfile(
  client: MediaClient,
  profile: UserProfile,
): Promise<ResolvedProfile> {
  const mediaId = avatarMediaIdOf(profile);
  const entry: ResolvedProfile = { ...profile, avatarUrl: undefined };
  writeCache(entry);
  if (mediaId === undefined) {
    return entry;
  }
  const url = await resolveMediaUrl(client, mediaId).catch(() => null);
  const current = cache.get(profile.userId);
  if (current === undefined || avatarMediaIdOf(current) !== mediaId) {
    // A newer profile replaced this one while its avatar resolved (an edit, a refresh): the
    // newer entry wins, and this resolution belongs to a media object it no longer names.
    return current ?? entry;
  }
  if (url === null) {
    return current;
  }
  const resolved: ResolvedProfile = { ...current, avatarUrl: url };
  writeCache(resolved);
  return resolved;
}

/**
 * Fetches one account's profile and caches it with its avatar resolved — the way in for a caller
 * that has just changed something and needs every other surface (and its own view) to move now,
 * without waiting on a refetch nothing else would trigger. Resolves `null` when the server served
 * no profile for the id.
 */
export async function refreshProfile(
  client: ProfileClient & MediaClient,
  userId: Id,
): Promise<ResolvedProfile | null> {
  const profiles = await client.profile.fetch([userId]);
  const fresh = profiles.find((profile) => profile.userId === userId);
  if (fresh === undefined) {
    return null;
  }
  return cacheProfile(client, fresh);
}

export function useProfiles(ids: readonly Id[]): Map<Id, ResolvedProfile> {
  const { client } = useMigo();
  const [, setVersion] = useState(0);

  // Re-render on any cache write — this hook's own fetches, another hook's, or the profile
  // panel's refresh — so a profile (or its avatar) that resolved after this render still shows.
  useEffect(() => {
    const bump = (): void => setVersion((version) => version + 1);
    listeners.add(bump);
    return () => {
      listeners.delete(bump);
    };
  }, []);

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
    void client.profile
      .fetch(missing)
      .then((profiles) => {
        // Each cached profile notifies the listeners itself, and its avatar resolves behind
        // the write so names never wait on it.
        for (const profile of profiles) {
          void cacheProfile(client, profile);
        }
      })
      .catch(() => {
        // Leave uncached; a later render retries.
      })
      .finally(() => {
        for (const id of missing) {
          inFlight.delete(id);
        }
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, key]);

  const result = new Map<Id, ResolvedProfile>();
  for (const id of ids) {
    const profile = cache.get(id);
    if (profile) {
      result.set(id, profile);
    }
  }
  return result;
}

/** Convenience for a single id. */
export function useProfile(id: Id | null): ResolvedProfile | null {
  const map = useProfiles(id ? [id] : []);
  return id ? (map.get(id) ?? null) : null;
}
