package com.migo.core.domain

import com.migo.core.protocol.ConversationRole
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The group's two permission gates, pinned: a founder's controls (mute, kick, rename) reach only a
 * plain member from a founder who is not acting on themselves, and the vote reaches anyone but a
 * founder and one's own row. These are the same two functions the member sheet's rows read, so a
 * change here is a change to what every member's screen offers.
 */
class GroupPermissionsTest {

    // --- the founder controls ---

    @Test
    fun `a founder acts on a plain member`() {
        assertTrue(canFounderAct(ConversationRole.Founder, ConversationRole.Member, isSelf = false))
    }

    @Test
    fun `the two founders are beyond each other`() {
        assertFalse(canFounderAct(ConversationRole.Founder, ConversationRole.Founder, isSelf = false))
    }

    @Test
    fun `a plain member holds no founder controls`() {
        assertFalse(canFounderAct(ConversationRole.Member, ConversationRole.Member, isSelf = false))
        assertFalse(canFounderAct(ConversationRole.Member, ConversationRole.Founder, isSelf = false))
    }

    @Test
    fun `nobody acts on their own row`() {
        assertFalse(canFounderAct(ConversationRole.Founder, ConversationRole.Member, isSelf = true))
    }

    @Test
    fun `an unknown role is not a founder`() {
        assertFalse(canFounderAct(ConversationRole.Unknown, ConversationRole.Member, isSelf = false))
    }

    // --- the vote ---

    @Test
    fun `every member may vote against a plain member`() {
        assertTrue(canVoteKickGroup(ConversationRole.Member, isSelf = false))
        assertTrue(canVoteKickGroup(ConversationRole.Founder, isSelf = false))
    }

    @Test
    fun `a founder is immune to the vote`() {
        assertFalse(canVoteKickGroup(ConversationRole.Founder, isSelf = false))
    }

    @Test
    fun `nobody votes against themselves`() {
        assertFalse(canVoteKickGroup(ConversationRole.Member, isSelf = true))
        assertFalse(canVoteKickGroup(ConversationRole.Founder, isSelf = true))
    }

    @Test
    fun `an unknown role may still be voted on`() {
        assertTrue(canVoteKickGroup(ConversationRole.Unknown, isSelf = false))
    }
}
