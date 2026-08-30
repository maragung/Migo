/**
 * The profile domain: look up the public profiles of accounts by id, and edit your own.
 *
 * A profile is the public face of an account — display name, avatar, the shareable MGO public id,
 * level and badges — and carries nothing private: no plaintext, no key material. Reading is a plain
 * request/response lookup, batched so a client rendering a member list or a mention resolves many ids
 * in one round trip.
 *
 * # The edit surface is patch-shaped, and privacy settings ride on it
 *
 * {@link updateProfile} sends only the fields it is given; an absent field is left untouched, not
 * cleared. That matters most for the privacy fields ({@link ProfileUpdate.showLastSeen} and friends):
 * a caller editing only its display name must not silently re-state who may message it, so it passes
 * a one-field patch and the rest of the settings keep their server-side values.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  encodeProfileRequest,
  decodeProfileResponse,
  encodeProfileUpdate,
  decodeUserProfile,
} from '@migo/protocol';
import type { ProfileRequest, ProfileUpdate, UserProfile } from '@migo/protocol';

import type { Rpc } from './rpc.js';

/**
 * Fetch public account profiles.
 *
 * One instance per client. Stateless: callers that want a cache keep one themselves, since profiles
 * change rarely and a stale display name is harmless.
 */
export class ProfileDomain {
  readonly #rpc: Rpc;

  constructor(rpc: Rpc) {
    this.#rpc = rpc;
  }

  /**
   * Fetches the profiles for a batch of account ids.
   *
   * The returned array holds a profile for each id the server chose to serve, in no guaranteed order
   * and possibly shorter than the request — an id the server withholds (unknown, or not visible to the
   * caller) is simply absent rather than an error. Match results back to ids by {@link
   * UserProfile.userId}.
   */
  async fetch(userIds: Id[]): Promise<UserProfile[]> {
    const request: ProfileRequest = { userIds };
    const response = await this.#rpc.call(
      OP.PROFILE_FETCH,
      encodeProfileRequest,
      decodeProfileResponse,
      request,
    );
    return response.profiles;
  }

  /**
   * Fetches one account's profile, or `null` if the server served none for that id.
   *
   * A convenience over {@link fetch} for the single-id case, resolving the "absent means withheld"
   * rule to a `null` the caller can branch on.
   */
  async fetchOne(userId: Id): Promise<UserProfile | null> {
    const profiles = await this.fetch([userId]);
    return profiles.find((profile) => profile.userId === userId) ?? null;
  }

  /**
   * Edits the caller's own profile, sending only the fields present in `patch`.
   *
   * Absent fields keep their current server-side values — the patch is a delta, not a replacement —
   * so a caller saving a new display name does not also have to know (and re-send) its privacy
   * settings. Resolves with the full updated profile, the same shape {@link fetch} returns, so the
   * caller can refresh its cached copy from the reply instead of re-reading.
   */
  async updateProfile(patch: Partial<ProfileUpdate>): Promise<UserProfile> {
    // Copy the set fields into a fresh struct: the caller's object stays untouched, and an explicitly
    // undefined field (present key, absent value) is dropped here rather than encoded.
    const request: ProfileUpdate = {};
    if (patch.displayName !== undefined) {
      request.displayName = patch.displayName;
    }
    if (patch.bio !== undefined) {
      request.bio = patch.bio;
    }
    if (patch.avatarMediaId !== undefined) {
      request.avatarMediaId = patch.avatarMediaId;
    }
    if (patch.birthYear !== undefined) {
      request.birthYear = patch.birthYear;
    }
    if (patch.showLastSeen !== undefined) {
      request.showLastSeen = patch.showLastSeen;
    }
    if (patch.whoCanMessage !== undefined) {
      request.whoCanMessage = patch.whoCanMessage;
    }
    if (patch.whoCanAdd !== undefined) {
      request.whoCanAdd = patch.whoCanAdd;
    }
    if (patch.searchable !== undefined) {
      request.searchable = patch.searchable;
    }
    return this.#rpc.call(OP.PROFILE_UPDATE, encodeProfileUpdate, decodeUserProfile, request);
  }
}
