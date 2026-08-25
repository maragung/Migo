/**
 * The profile domain: look up the public profiles of accounts by id.
 *
 * A profile is the public face of an account — display name, avatar, the shareable MGO public id,
 * level and badges — and carries nothing private: no plaintext, no key material. It is a plain
 * request/response lookup, batched so a client rendering a member list or a mention resolves many ids
 * in one round trip.
 */

import type { Id } from '@migo/wire';
import { OP, encodeProfileRequest, decodeProfileResponse } from '@migo/protocol';
import type { ProfileRequest, UserProfile } from '@migo/protocol';

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
}
