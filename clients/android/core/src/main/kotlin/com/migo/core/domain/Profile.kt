package com.migo.core.domain

import com.migo.core.protocol.Op
import com.migo.core.protocol.ProfileRequest
import com.migo.core.protocol.ProfileResponse
import com.migo.core.protocol.ProfileUpdate
import com.migo.core.protocol.UserProfile
import com.migo.core.wire.Id

/**
 * Looking up the public profiles of accounts by id.
 *
 * A port of `packages/sdk/src/domains/profile.ts`. A profile is the public face of an account --
 * display name, avatar, the shareable public id, level and badges -- and carries nothing private: no
 * message content and no key material, so unlike almost everything else in this SDK the response
 * needs no decryption and is safe to cache anywhere the app already caches images.
 *
 * Batched on purpose. Rendering a member list or resolving the mentions in one message means many
 * ids, and a per-id round trip would make a group's member list an N-request screen.
 */
class ProfileDomain(private val rpc: Rpc) {
    /**
     * Fetches the profiles for a batch of account ids.
     *
     * The result may be **shorter than the request and in any order**. An id the server chooses not
     * to serve -- unknown, deleted, or not visible to this caller -- is simply absent rather than an
     * error, which is section 48's hidden-existence rule: an error that distinguished "no such
     * account" from "not visible to you" would answer a question the caller is not entitled to ask.
     * Match results back by [UserProfile.userId] rather than by position.
     */
    suspend fun fetch(userIds: List<Id>): List<UserProfile> {
        val request = ProfileRequest(userIds)
        val response = rpc.call(
            Op.PROFILE_FETCH,
            { w -> request.encode(w) },
            { r -> ProfileResponse.decode(r) },
        )
        return response.profiles
    }

    /**
     * Fetches one account's profile, or null when the server served none for that id.
     *
     * A convenience over [fetch] that turns the absent-means-withheld rule into a null the caller can
     * branch on. The id is checked against the returned profile rather than assumed: a response is
     * untrusted input, and taking `profiles[0]` would mean a server that answered with somebody
     * else's profile got that profile displayed under the requested id.
     */
    suspend fun fetchOne(userId: Id): UserProfile? =
        fetch(listOf(userId)).firstOrNull { it.userId == userId }

    /**
     * Edits the caller's own profile, sending only the fields that are non-null in [patch].
     *
     * Null fields keep their current server-side values — the patch is a delta, not a replacement —
     * so a caller saving a new display name does not also have to know (and re-send) its privacy
     * settings. Returns the full updated profile, the same shape [fetch] returns, so the caller can
     * refresh its cached copy from the reply instead of re-reading. The one wire-level wrinkle: an
     * empty bio or a cleared display name is an *explicit* value, not an omission, so this port
     * takes `null` as "leave it" and the caller sends "" only on purpose.
     */
    suspend fun update(patch: ProfileUpdate): UserProfile =
        rpc.call(
            Op.PROFILE_UPDATE,
            { w -> patch.encode(w) },
            { r -> UserProfile.decode(r) },
        )
}
