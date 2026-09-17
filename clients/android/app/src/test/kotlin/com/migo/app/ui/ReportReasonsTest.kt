package com.migo.app.ui

import com.migo.core.domain.ReportReason
import com.migo.core.domain.ReportSubject
import com.migo.core.domain.ReportTarget
import com.migo.core.domain.openingReason
import com.migo.core.wire.NIL_ID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The reason menu, pinned where it differs by subject.
 *
 * Section 49 leaves four codes out of the generic menu on purpose, and one of them -- bot abuse --
 * is the code a bot report is filed under. The rule that reconciles the two is not "bot abuse is
 * never offered" but "bot abuse is offered exactly where the surface already knows the subject is a
 * bot", and a rule stated that way is one a rendering test cannot see: the menu is drawn inside a
 * sheet, and the interesting question is which rows are in the list, not what they look like.
 *
 * So this suite holds the *set* still, the same way the web suite's own reason test does -- the web
 * dialog seeds a hidden code instead, and the difference between the two clients is exactly what
 * these lines record: this sheet never opens with a reason its menu does not show, because on a
 * phone a live Send over an unseen pick is a report filed under a reason nobody chose.
 */
class ReportReasonsTest {

    @Test
    fun `the generic menu offers the nine a person can judge for themselves`() {
        val offered = REPORT_REASONS.map { it.reason }
        assertEquals("nine rows, and the catch-all is the last of them", 9, offered.size)
        assertEquals(ReportReason.Other, offered.last())

        // The four left out, each for its own reason. Flood is a rate the server already counts;
        // self-harm is not a list entry to pick while looking at somebody you are worried about;
        // child safety is a legal distinction the queue's prioritisation should draw rather than
        // the reporter; and bot abuse is reached by a surface that knows the subject is a bot.
        for (absent in listOf(
            ReportReason.Flood,
            ReportReason.SelfHarm,
            ReportReason.ChildSafety,
            ReportReason.BotAbuse,
        )) {
            assertFalse("$absent is not a generic menu row", offered.contains(absent))
        }

        assertEquals("a reason is never listed twice", offered.size, offered.toSet().size)
        assertTrue("the catch-all is not the first thing read", offered.first() != ReportReason.Other)
    }

    @Test
    fun `a bot subject is offered the bot reason, first, and keeps every other row`() {
        val menu = reportReasons(ReportSubject.Bot)
        assertEquals("the bot row is added, not swapped in", REPORT_REASONS.size + 1, menu.size)
        assertEquals(ReportReason.BotAbuse, menu.first().reason)
        assertEquals(
            "the reporter can still change their mind to anything the generic menu holds",
            REPORT_REASONS.map { it.reason },
            menu.drop(1).map { it.reason },
        )
        // Nothing is lost and nothing is duplicated: the menu is the same nine with one row in
        // front, which is what makes it safe to build by concatenation rather than by a second list
        // that would have to be kept in step by hand.
        assertEquals(menu.size, menu.map { it.reason }.toSet().size)
    }

    @Test
    fun `every other subject gets the generic menu untouched`() {
        for (subject in ReportSubject.entries.filter { it != ReportSubject.Bot }) {
            assertEquals(
                "$subject is not a bot, so its menu is the generic one",
                REPORT_REASONS.map { it.reason },
                reportReasons(subject).map { it.reason },
            )
        }
    }

    @Test
    fun `the reason a bot subject opens on is the row its menu shows first`() {
        // The pairing that matters: whatever the sheet opens with has to be a row the reporter can
        // see picked. The domain decides the reason and this file decides the rows, so the two are
        // asserted against each other here rather than assumed to agree.
        val opening = openingReason(ReportTarget(ReportSubject.Bot, NIL_ID))
        assertEquals(opening, reportReasons(ReportSubject.Bot).first().reason)
    }
}
