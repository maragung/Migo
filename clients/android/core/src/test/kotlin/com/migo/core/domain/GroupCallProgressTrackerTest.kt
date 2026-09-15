package com.migo.core.domain

import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * What the join affordance is allowed to know, pinned the way the roster fold's own suite pins it.
 *
 * The roster fold answers the seated screen's question -- who is in *my* call -- and its suite
 * (`GroupCallTest`) pins exactly that. This suite pins the other half of the same announcements:
 * the fold that answers the chat header's question before anyone taps, *is there a call to join
 * here*. Every member receives the conversation topic's join and departure announcements seated
 * or not, so a member who never joined still watches the call's whole life pass by -- and these
 * tests hold the tracker to the discipline that makes that stream a trustworthy affordance:
 *
 *   - an announcement for a call this device holds no seat in populates the entry, and the
 *     server's own tally is the count the affordance names;
 *   - a departure's count of zero is the retirement, the same signal that ends a seated screen;
 *   - a session that signs in mid-call hears no join for it -- the first departure it does hear
 *     still carries the call's id, conversation and size, and discovers the call rather than
 *     missing it;
 *   - a stale departure naming a call the tracker does not hold must not retire or resize the
 *     live entry;
 *   - [GroupCallProgressTracker.forget] is the join's own correction, dropping the conversation's
 *     entry the moment this device asks for a seat in it.
 */
class GroupCallProgressTrackerTest {

    // The same fixture shape the roster suite uses, so the two files read side by side. No "me"
    // fixture here on purpose: the announcements never name the device that hears them, and this
    // fold is exactly the view of a member who holds no seat.

    private val ADA: Id = parseId("0123456789ABCDEFGHJKMNPQRY")
    private val ADA_LAPTOP: Id = parseId("0123456789ABCDEFGHJKMNPQRZ")
    private val BEN: Id = parseId("0123456789ABCDEFGHJKMNPQ23")
    private val BEN_PHONE: Id = parseId("0123456789ABCDEFGHJKMNPQ24")
    private val CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
    private val LATER_CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRT")
    private val GROUP: Id = parseId("0123456789ABCDEFGHJKMNPQRW")
    private val OTHER_GROUP: Id = parseId("0123456789ABCDEFGHJKMNPQRX")

    private fun joined(
        callId: Id = CALL,
        conversationId: Id = GROUP,
        userId: Id = ADA,
        deviceId: Id = ADA_LAPTOP,
        participantCount: Long,
    ) = GroupCallJoinedEvent(
        callId = callId,
        conversationId = conversationId,
        userId = userId,
        deviceId = deviceId,
        participantCount = participantCount,
    )

    private fun left(
        callId: Id = CALL,
        conversationId: Id = GROUP,
        userId: Id = ADA,
        deviceId: Id = ADA_LAPTOP,
        participantCount: Long,
    ) = GroupCallLeftEvent(
        callId = callId,
        conversationId = conversationId,
        userId = userId,
        deviceId = deviceId,
        participantCount = participantCount,
    )

    @Test
    fun joinAnnouncementsForACallThisDeviceIsNotSeatedInPopulateTheEntry() {
        val tracker = GroupCallProgressTracker()
        tracker.onJoined(joined(userId = ADA, participantCount = 1))
        tracker.onJoined(joined(userId = BEN, deviceId = BEN_PHONE, participantCount = 2))
        assertEquals(mapOf(GROUP to GroupCallInProgress(CALL, 2)), tracker.snapshot())
    }

    @Test
    fun departuresMoveTheCountAndACountOfZeroRetiresTheEntry() {
        val tracker = GroupCallProgressTracker()
        tracker.onJoined(joined(participantCount = 3))
        tracker.onLeft(left(userId = BEN, deviceId = BEN_PHONE, participantCount = 2))
        assertEquals(mapOf(GROUP to GroupCallInProgress(CALL, 2)), tracker.snapshot())
        tracker.onLeft(left(participantCount = 0))
        assertEquals(emptyMap<Id, GroupCallInProgress>(), tracker.snapshot())
    }

    @Test
    fun aDepartureHeardWithoutAnyJoinStillDiscoversTheRunningCall() {
        val tracker = GroupCallProgressTracker()
        // A session that signs in mid-call: the next announcement is somebody leaving, and it
        // carries everything the affordance needs -- the call, the conversation, the size.
        tracker.onLeft(left(participantCount = 2))
        assertEquals(mapOf(GROUP to GroupCallInProgress(CALL, 2)), tracker.snapshot())
    }

    @Test
    fun aDepartureHeardWithoutAnyJoinDiscoversNothingWhenTheCallRetiredWithIt() {
        val tracker = GroupCallProgressTracker()
        tracker.onLeft(left(participantCount = 0))
        assertEquals(emptyMap<Id, GroupCallInProgress>(), tracker.snapshot())
    }

    @Test
    fun aStaleDepartureNamingAnotherCallNeitherResizesNorRetiresTheLiveEntry() {
        val tracker = GroupCallProgressTracker()
        tracker.onJoined(joined(callId = CALL, participantCount = 4))
        // A departure for a call the tracker never held in this conversation: a fact about a call
        // that has already been replaced, not the one the affordance offers.
        tracker.onLeft(left(callId = LATER_CALL, participantCount = 3))
        tracker.onLeft(left(callId = LATER_CALL, participantCount = 0))
        assertEquals(mapOf(GROUP to GroupCallInProgress(CALL, 4)), tracker.snapshot())
    }

    @Test
    fun aJoinAlwaysWinsTheEntryEvenOverOneHeldForAnOlderCall() {
        val tracker = GroupCallProgressTracker()
        tracker.onJoined(joined(callId = CALL, participantCount = 1))
        // Two live calls in one conversation cannot stand, so the arrival names the live one.
        tracker.onJoined(joined(callId = LATER_CALL, participantCount = 2))
        assertEquals(mapOf(GROUP to GroupCallInProgress(LATER_CALL, 2)), tracker.snapshot())
    }

    @Test
    fun twoConversationsTrackIndependentlyAndForgettingDropsOnlyTheNamedOne() {
        val tracker = GroupCallProgressTracker()
        tracker.onJoined(joined(conversationId = GROUP, participantCount = 2))
        tracker.onJoined(joined(callId = LATER_CALL, conversationId = OTHER_GROUP, participantCount = 5))
        tracker.forget(GROUP)
        assertEquals(mapOf(OTHER_GROUP to GroupCallInProgress(LATER_CALL, 5)), tracker.snapshot())
        // What a join of the running call owes: the conversation's own entry is gone, and the
        // affordance falls back to starting a call rather than joining a retired one.
        tracker.forget(OTHER_GROUP)
        assertNull(tracker.snapshot()[OTHER_GROUP])
        assertEquals(emptyMap<Id, GroupCallInProgress>(), tracker.snapshot())
    }
}
