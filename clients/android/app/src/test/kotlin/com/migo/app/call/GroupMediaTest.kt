package com.migo.app.call

import com.migo.core.domain.GroupCallSeat
import com.migo.core.protocol.TurnServer
import com.migo.core.wire.Id
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The four decisions two seats of a mesh must agree on, pinned.
 *
 * Every one of these is a rule both ends of a link compute independently, so the case that matters
 * is the one where a wrong answer is silent: a pair that both offers, a pair that neither dials, or
 * a call that carries one video stream more than the product limit allows. None of those throw.
 */
class GroupMediaTest {

    private fun seat(user: String, device: String, joinedAt: Long = 0L) =
        GroupCallSeat(userId = Id(user), deviceId = Id(device), joinedAt = joinedAt)

    @Test
    fun videoIsRefusedAtTheProductLimitAndNotBefore() {
        assertTrue(videoAdmitted(remoteSeats = MAX_ACTIVE_VIDEO_STREAMS - 1, wantsVideo = true))
        assertFalse(videoAdmitted(remoteSeats = MAX_ACTIVE_VIDEO_STREAMS, wantsVideo = true))
    }

    @Test
    fun aSeatThatWantsNoVideoIsRefusedWhateverTheRosterHolds() {
        assertFalse(videoAdmitted(remoteSeats = 0, wantsVideo = false))
        assertFalse(videoAdmitted(remoteSeats = 99, wantsVideo = false))
    }

    @Test
    fun exactlyOneSideOfAGlareKeepsItsOffer() {
        val lower = Id("AAAAAAAAAAAAAAAAAAAAAAAAAA")
        val higher = Id("ZZZZZZZZZZZZZZZZZZZZZZZZZZ")

        assertTrue(iKeepMyOffer(lower, higher))
        assertFalse(iKeepMyOffer(higher, lower))
    }

    @Test
    fun aSeatDialsTheSeatsThatWereAlreadyThere() {
        val roster = listOf(
            seat("user-a", "device-a", joinedAt = 1),
            seat("user-b", "device-b", joinedAt = 2),
            seat("user-c", "device-c", joinedAt = 3),
        )

        // The last seat in dials both earlier ones; the first dials nobody.
        assertTrue(dialsRemote(roster, Id("user-c"), Id("device-a")))
        assertTrue(dialsRemote(roster, Id("user-c"), Id("device-b")))
        assertFalse(dialsRemote(roster, Id("user-a"), Id("device-b")))
        assertFalse(dialsRemote(roster, Id("user-a"), Id("device-c")))
    }

    @Test
    fun aSeatWithNoPlaceInTheRosterDialsNobody() {
        val roster = listOf(seat("user-a", "device-a"))

        assertFalse(dialsRemote(roster, Id("user-z"), Id("device-a")))
        assertFalse(dialsRemote(roster, Id("user-a"), Id("device-z")))
        assertFalse(dialsRemote(emptyList(), Id("user-a"), Id("device-a")))
    }

    @Test
    fun theRelayListIsCarriedWithItsCredentialsAndTheFallbackAlwaysFollows() {
        val relays = listOf(
            TurnServer(
                url = "turn:relay.example:3478",
                username = "migo",
                credential = "secret",
                ttlSeconds = 600,
                region = "sg",
            ),
            TurnServer(
                url = "turn:bare.example:3478",
                username = "",
                credential = "",
                ttlSeconds = 600,
                region = "sg",
            ),
        )

        val servers = groupIceServers(relays)

        assertEquals(relays.size + 1, servers.size)
        assertEquals("turn:relay.example:3478", servers[0].urls.first())
        assertEquals("migo", servers[0].username)
        assertEquals("secret", servers[0].password)
        assertEquals("stun:stun.l.google.com:19302", servers.last().urls.first())
    }

    @Test
    fun anEmptyRelayListStillYieldsTheFallback() {
        val servers = groupIceServers(emptyList())

        assertEquals(1, servers.size)
        assertEquals(GROUP_STUN_FALLBACK.urls.first(), servers[0].urls.first())
    }
}
