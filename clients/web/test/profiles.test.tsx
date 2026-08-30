/**
 * The profile cache and the avatar it resolves: names land at once, pictures land behind them,
 * and a name never waits on its picture.
 *
 * The cache's own logic is pinned here as pure async functions over client doubles (the {@link
 * ProfileClient} and {@link MediaClient} slices the cache actually reads, which is why no whole
 * client or React renderer is needed), plus the render rule every avatar consumer depends on:
 * a resolved URL is an image, and everything else — no avatar, not resolved yet — is initials,
 * never a broken img. Three rules carry correctness weight:
 *
 *   1. **An avatar is a media object, not a URL.** The wire names it by id; the fetchable URL is
 *      minted per session through the media domain and cached there, so resolving the same
 *      profile twice costs one download.
 *   2. **A failed resolution is not a verdict.** The initials stay and the next fetch retries;
 *      a media server briefly unavailable says nothing about the object.
 *   3. **A slow resolution must not graft an old avatar onto a newer profile.** When a profile is
 *      refreshed while its previous avatar's URL was still resolving, the newer entry wins.
 *
 * The profiles under test are built with the avatar media field by hand because the generated
 * wire type is mid-regeneration (`avatar_url` becomes `avatar_media_id`); {@link
 * avatarMediaIdOf} is the seam both sides compile against, and its test pins the read.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { Id, UserProfile } from '@migo/sdk';

import { Avatar } from '../src/components/avatar.js';
import type { MediaClient } from '../src/lib/migo/media.js';
import { avatarMediaIdOf, cacheProfile, refreshProfile } from '../src/lib/migo/use-profiles.js';
import type { ProfileClient } from '../src/lib/migo/use-profiles.js';

const URL_TTL_MS = 3_600_000;

/**
 * A wire profile with the avatar media field set by hand: the generated type does not carry the
 * field on every side of its regeneration yet, so the literal is built loosely and cast — the
 * same posture the seam in use-profiles takes.
 */
function wireProfile(userId: Id, avatarMediaId?: Id): UserProfile {
  const profile: Record<string, unknown> = {
    userId,
    publicId: `MGO-${String(userId)}`,
    username: 'ada',
    displayName: 'Ada Lovelace',
  };
  if (avatarMediaId !== undefined) {
    profile.avatarMediaId = avatarMediaId;
  }
  return profile as unknown as UserProfile;
}

/** A client double serving fixed profiles and a canned download, recording what was asked. */
function double(
  profiles: UserProfile[],
  download: (objectId: Id) => Promise<{ url: string; expiresAt: number }>,
): {
  client: ProfileClient & MediaClient;
  fetches: Id[][];
  downloads: Id[];
} {
  const fetches: Id[][] = [];
  const downloads: Id[] = [];
  const client: ProfileClient & MediaClient = {
    profile: {
      fetch: (userIds: Id[]) => {
        fetches.push(userIds);
        return Promise.resolve(profiles.filter((profile) => userIds.includes(profile.userId)));
      },
    },
    media: {
      upload: () => Promise.reject(new Error('upload is not under test here')),
      download: (objectId: Id) => {
        downloads.push(objectId);
        return download(objectId);
      },
    },
  };
  return { client, fetches, downloads };
}

test('the migration seam reads the avatar media id the wire will send', () => {
  const media = 'media_ada' as Id;
  assert.equal(avatarMediaIdOf(wireProfile('prof_ada' as Id, media)), media);
  assert.equal(
    avatarMediaIdOf(wireProfile('prof_none' as Id)),
    undefined,
    'a profile without an avatar names no media object',
  );
});

test('a profile with an avatar media id resolves its URL through the media domain, once', async () => {
  const media = 'media_once' as Id;
  const { client, downloads } = double([wireProfile('prof_once' as Id, media)], (objectId) =>
    Promise.resolve({
      url: `https://media.example.test/${String(objectId)}`,
      expiresAt: Date.now() + URL_TTL_MS,
    }),
  );

  const resolved = await cacheProfile(client, wireProfile('prof_once' as Id, media));
  assert.equal(resolved.avatarUrl, `https://media.example.test/${String(media)}`);
  assert.equal(resolved.displayName, 'Ada Lovelace', 'the wire fields ride along unchanged');

  // Resolving the same profile again is served by the session URL cache: one download total.
  const again = await cacheProfile(client, wireProfile('prof_once' as Id, media));
  assert.equal(again.avatarUrl, resolved.avatarUrl);
  assert.equal(downloads.length, 1, 'the second resolve must be served from the URL cache');
});

test('a profile without an avatar never touches the media domain', async () => {
  const { client, downloads } = double([], () =>
    Promise.reject(new Error('no download should happen')),
  );
  const resolved = await cacheProfile(client, wireProfile('prof_plain' as Id));
  assert.equal(resolved.avatarUrl, undefined, 'no avatar means initials, not a broken img');
  assert.equal(downloads.length, 0);
});

test('a failed avatar resolution is not a verdict: initials stay, nothing throws', async () => {
  const { client } = double([], () => Promise.reject(new Error('media unavailable')));
  const resolved = await cacheProfile(client, wireProfile('prof_flaky' as Id, 'media_flaky' as Id));
  assert.equal(resolved.avatarUrl, undefined);
});

test('a slow avatar resolution does not overwrite a newer profile for the same account', async () => {
  const user = 'prof_race' as Id;
  const oldMedia = 'media_race_old' as Id;
  const newMedia = 'media_race_new' as Id;
  const newUrl = 'https://media.example.test/new';
  let releaseOld: ((url: string) => void) | undefined;
  const { client } = double([wireProfile(user, newMedia)], (objectId) =>
    objectId === oldMedia
      ? new Promise((resolve) => {
          releaseOld = (url: string) => resolve({ url, expiresAt: Date.now() + URL_TTL_MS });
        })
      : Promise.resolve({ url: newUrl, expiresAt: Date.now() + URL_TTL_MS }),
  );

  // The old profile's avatar resolution is still pending when the newer profile lands.
  const pending = cacheProfile(client, wireProfile(user, oldMedia));
  const fresh = await cacheProfile(client, wireProfile(user, newMedia));
  assert.equal(fresh.avatarUrl, newUrl);

  releaseOld?.('https://media.example.test/old');
  const settled = await pending;
  assert.equal(
    settled.avatarUrl,
    newUrl,
    'the stale resolution is dropped, not grafted onto the newer entry',
  );
});

test('refreshProfile fetches one profile and returns it with the avatar resolved', async () => {
  const media = 'media_refresh' as Id;
  const { client, fetches } = double([wireProfile('prof_refresh' as Id, media)], () =>
    Promise.resolve({
      url: 'https://media.example.test/refresh',
      expiresAt: Date.now() + URL_TTL_MS,
    }),
  );
  const resolved = await refreshProfile(client, 'prof_refresh' as Id);
  assert.deepEqual(fetches, [['prof_refresh' as Id]], 'exactly one batched fetch of the one id');
  assert.equal(resolved?.avatarUrl, 'https://media.example.test/refresh');
  assert.equal(resolved?.displayName, 'Ada Lovelace');

  // An id the server serves no profile for is a null, not an error.
  assert.equal(await refreshProfile(client, 'prof_absent' as Id), null);
});

// --- the rendered avatar ---

test('the avatar renders the image when a URL is resolved, initials for everything else', () => {
  const withImage = renderToStaticMarkup(
    <Avatar name="Ada Lovelace" id="ada" avatarUrl="https://media.example.test/a" />,
  );
  assert.ok(
    withImage.includes('<img src="https://media.example.test/a"'),
    'the resolved URL is the img',
  );
  assert.ok(!withImage.includes('AL'), 'initials are not rendered behind the image');

  const noAvatar = renderToStaticMarkup(<Avatar name="Ada Lovelace" id="ada" />);
  assert.ok(!noAvatar.includes('<img'), 'no avatar means no img element to break');
  assert.ok(noAvatar.includes('AL'), 'the initials fallback renders');
});
