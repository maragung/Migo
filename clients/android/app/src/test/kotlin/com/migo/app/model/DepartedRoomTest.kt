package com.migo.app.model

import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.MemberChange
import com.migo.core.protocol.RoomMemberEvent
import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The one frame a removed member gets to keep, pinned the way the web suite pins its own room
 * projection (audit area 4: the kicked member must lose the surface, not just the delivery).
 *
 * The server publishes the removal [RoomMemberEvent] naming the removed member and then takes
 * the room's topics away, so the projection under test is the whole client-side story: an event
 * naming this account with `Kicked`, `Banned`, or `Left` must resolve to the conversation the
 * shell has to drop -- the list row, the window tab, the open chat -- and nothing else may:
 * a join or a return keeps the account seated, a disconnect revokes nothing server-side, and a
 * departure naming someone else is the room's business, not this shell's.
 */
class DepartedRoomTest {

    private val me: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
    private val other: Id = parseId("0123456789ABCDEFGHJKMNPQRT")
    private val room: Id = parseId("0123456789ABCDEFGHJKMNPQRX")
    private val conversation: Id = parseId("0123456789ABCDEFGHJKMNPQRV")

    private fun rows() = listOf(
        ConversationRow(
            conversationId = conversation,
            title = "Observatory",
            kind = ConversationKind.Room,
            roomId = room,
        ),
    )

    private fun event(change: MemberChange, userId: Id = me, roomId: Id = room) = RoomMemberEvent(
        roomId = roomId,
        userId = userId,
        joined = change == MemberChange.Joined,
        memberCount = 11,
        change = change,
    )

    @Test
    fun aKickABanAndALeaveNamingThisAccountEachNameTheConversationToDrop() {
        for (change in listOf(MemberChange.Kicked, MemberChange.Banned, MemberChange.Left)) {
            assertEquals(
                conversation,
                departedRoomConversation(event(change), me, rows(), emptyList(), null),
            )
        }
    }

    @Test
    fun theConversationIsFoundThroughTheWindowTabWhenTheListRowNeverLearnedTheRoom() {
        val tab = listOf(WindowTab(conversationId = conversation, title = "Observatory", roomId = room))
        assertEquals(
            conversation,
            departedRoomConversation(event(MemberChange.Kicked), me, emptyList(), tab, null),
        )
    }

    @Test
    fun theConversationIsFoundThroughTheOpenChatWhenNeitherRowNorTabLearnedTheRoom() {
        val open = ChatState(conversationId = conversation, title = "Observatory", roomId = room)
        assertEquals(
            conversation,
            departedRoomConversation(event(MemberChange.Banned), me, emptyList(), emptyList(), open),
        )
    }

    @Test
    fun aJoinAReturnOrADisconnectNamingThisAccountKeepsTheRoom() {
        for (change in listOf(MemberChange.Joined, MemberChange.Reconnected, MemberChange.Disconnected)) {
            assertNull(departedRoomConversation(event(change), me, rows(), emptyList(), null))
        }
    }

    @Test
    fun aDepartureNamingSomeoneElseIsNotThisShellSToActOn() {
        for (change in listOf(MemberChange.Kicked, MemberChange.Banned, MemberChange.Left)) {
            assertNull(departedRoomConversation(event(change, userId = other), me, rows(), emptyList(), null))
        }
    }

    @Test
    fun aLegacyEventWithNoChangeIsNoDepartureEvenWhenItsJoinedFlagIsFalse() {
        val legacy = RoomMemberEvent(roomId = room, userId = me, joined = false, memberCount = 11)
        assertNull(departedRoomConversation(legacy, me, rows(), emptyList(), null))
    }

    @Test
    fun aRoomNoSurfacePairsWithAConversationHasNothingToDrop() {
        assertNull(
            departedRoomConversation(
                event(MemberChange.Kicked, roomId = parseId("0123456789ABCDEFGHJKMNPQRW")),
                me,
                rows(),
                emptyList(),
                null,
            ),
        )
        // No account signed in means nobody to name.
        assertNull(departedRoomConversation(event(MemberChange.Kicked), null, rows(), emptyList(), null))
    }
}
