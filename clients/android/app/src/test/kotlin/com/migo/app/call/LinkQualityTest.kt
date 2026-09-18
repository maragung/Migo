package com.migo.app.call

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The link model pinned against the numbers the web build and the node's decision core use for the
 * same arithmetic.
 *
 * Every case here is a number a real call produces at one rung's edge: the thresholds are checked at
 * the value they cross and one below it, because a ladder that classifies a link differently from
 * the other two clients is a call the three disagree about, and the ramp is checked on both sides of
 * its interval because climbing early is the oscillation the ramp exists to prevent. Nothing in this
 * file touches a connection: reading one is the platform's half of the model and the half a unit
 * test cannot honestly stand in for.
 */
class LinkQualityTest {

    private fun counters(
        at: Long = 0,
        packetsLost: Long = 0,
        packetsReceived: Long = 0,
        framesReceived: Long = 0,
        framesDropped: Long = 0,
        bytesSent: Long = 0,
        rttMs: Long = 0,
        jitterMs: Long = 0,
        availableKbps: Long? = null,
        relay: Boolean? = null,
    ) = LinkCounters(
        at = at,
        packetsLost = packetsLost,
        packetsReceived = packetsReceived,
        framesReceived = framesReceived,
        framesDropped = framesDropped,
        bytesSent = bytesSent,
        rttMs = rttMs,
        jitterMs = jitterMs,
        availableKbps = availableKbps,
        relay = relay,
    )

    private fun stats(
        packetLossPct: Long = 0,
        rttMs: Long = 0,
        jitterMs: Long = 0,
        availableKbps: Long = 0,
        sentKbps: Long = 0,
        droppedFramePct: Long = 0,
    ) = LinkStats(
        packetLossPct = packetLossPct,
        rttMs = rttMs,
        jitterMs = jitterMs,
        availableKbps = availableKbps,
        sentKbps = sentKbps,
        droppedFramePct = droppedFramePct,
    )

    @Test
    fun aStretchMeasuresWhatTheCountersMoved() {
        val previous = counters(at = 0, packetsReceived = 1000)
        val next = counters(
            at = 2000,
            packetsLost = 20,
            packetsReceived = 1980,
            framesReceived = 300,
            framesDropped = 30,
            bytesSent = 500_000,
            rttMs = 140,
            jitterMs = 10,
        )

        val measured = linkStatsBetween(previous, next)

        assertNotNull(measured)
        assertEquals(2L, measured?.packetLossPct)
        assertEquals(9L, measured?.droppedFramePct)
        assertEquals(2000L, measured?.sentKbps)
        assertEquals(140L, measured?.rttMs)
        assertEquals(10L, measured?.jitterMs)
    }

    @Test
    fun anAbsentBandwidthEstimateFallsBackToWhatIsBeingSent() {
        val measured = linkStatsBetween(
            counters(at = 0, packetsReceived = 100),
            counters(at = 1000, packetsReceived = 200, bytesSent = 250_000),
        )

        assertEquals(2000L, measured?.availableKbps)
        assertEquals(2000L, measured?.sentKbps)
        assertEquals(0L, congestionScore(measured!!, DEFAULT_ADAPTIVE_THRESHOLDS))
    }

    @Test
    fun aStretchTooShortOrTooEmptySaysNothing() {
        assertNull(linkStatsBetween(counters(at = 0), counters(at = 0)))
        assertNull(linkStatsBetween(counters(at = 1000), counters(at = 500)))
        // Nothing arrived and nothing was lost, so there is no denominator to call loss a share of.
        assertNull(linkStatsBetween(counters(at = 0), counters(at = 1000)))
    }

    @Test
    fun anImpossibleReportNeverScoresBetterThanAnHonestOne() {
        // Counters can move backwards in a platform's report; the score clamps at zero rather than
        // letting a negative term buy a better rung than the link has earned.
        val backwards = linkStatsBetween(
            counters(at = 0, packetsLost = 500, packetsReceived = 500, bytesSent = 100_000),
            counters(at = 1000, packetsLost = 100, packetsReceived = 1500, bytesSent = 0, rttMs = -5),
        )

        assertNotNull(backwards)
        assertEquals(0L, backwards?.packetLossPct)
        assertEquals(0L, backwards?.sentKbps)
        assertEquals(0L, backwards?.rttMs)
    }

    @Test
    fun theScoreIsTheSumOfTheTermsSection165Names() {
        val thresholds = DEFAULT_ADAPTIVE_THRESHOLDS
        // Loss: 3 per cent is twelve points. Jitter: fifteen milliseconds is three. Round trip time:
        // one hundred and twenty is the baseline and contributes nothing at all.
        assertEquals(
            15L,
            congestionScore(stats(packetLossPct = 3, jitterMs = 15, rttMs = 120), thresholds),
        )
        // Ten milliseconds over the baseline is one point, and the deficit term is truncated to
        // whole hundreds of kilobits: five hundred short is five.
        assertEquals(
            6L,
            congestionScore(
                stats(rttMs = 130, availableKbps = 1500, sentKbps = 2000),
                thresholds,
            ),
        )
        // A link sending less than its estimate is not in deficit.
        assertEquals(
            0L,
            congestionScore(stats(rttMs = 120, availableKbps = 2000, sentKbps = 1500), thresholds),
        )
    }

    @Test
    fun theThresholdsAreReadInLadderOrder() {
        val thresholds = DEFAULT_ADAPTIVE_THRESHOLDS

        assertEquals(LinkQuality.Full, targetQuality(stats(packetLossPct = 2, jitterMs = 15), thresholds))
        assertEquals(
            LinkQuality.BitrateCapped,
            targetQuality(stats(packetLossPct = 3), thresholds),
        )
        assertEquals(
            LinkQuality.ResolutionLowered,
            targetQuality(stats(packetLossPct = 5, jitterMs = 25), thresholds),
        )
        assertEquals(
            LinkQuality.FrameRateLowered,
            targetQuality(stats(packetLossPct = 8, jitterMs = 40), thresholds),
        )
        assertEquals(
            LinkQuality.VideoOff,
            targetQuality(stats(packetLossPct = 12, jitterMs = 60), thresholds),
        )
        assertEquals(
            LinkQuality.FrameRateLowered,
            targetQuality(stats(packetLossPct = 11, jitterMs = 75), thresholds),
        )
    }

    @Test
    fun goingDownTheLadderIsImmediate() {
        assertEquals(
            LinkQuality.VideoOff,
            advanceQuality(LinkQuality.Full, LinkQuality.VideoOff, changedAtMs = 1000, nowMs = 1000),
        )
    }

    @Test
    fun climbingWaitsOneRungPerInterval() {
        val changedAt = 1000L
        assertEquals(
            LinkQuality.VideoOff,
            advanceQuality(LinkQuality.VideoOff, LinkQuality.Full, changedAt, changedAt + 2999),
        )
        assertEquals(
            LinkQuality.FrameRateLowered,
            advanceQuality(LinkQuality.VideoOff, LinkQuality.Full, changedAt, changedAt + 3000),
        )
        // The rung above the one just reached is a second interval away, never part of the same step.
        assertEquals(
            LinkQuality.ResolutionLowered,
            advanceQuality(LinkQuality.FrameRateLowered, LinkQuality.Full, changedAt, changedAt + 3000),
        )
        assertEquals(
            LinkQuality.BitrateCapped,
            advanceQuality(LinkQuality.ResolutionLowered, LinkQuality.Full, changedAt, changedAt + 3000),
        )
        assertEquals(
            LinkQuality.Full,
            advanceQuality(LinkQuality.BitrateCapped, LinkQuality.Full, changedAt, changedAt + 3000),
        )
        // A target better than the rung above is still climbed one rung at a time.
        assertEquals(
            LinkQuality.FrameRateLowered,
            advanceQuality(
                LinkQuality.VideoOff,
                LinkQuality.ResolutionLowered,
                changedAt,
                changedAt + 3000,
            ),
        )
        // An interval of zero has a floor of one millisecond rather than of nothing, so even a link
        // told to recover at once cannot move twice in the same instant -- and it does move at the
        // next one.
        assertEquals(
            LinkQuality.VideoOff,
            advanceQuality(LinkQuality.VideoOff, LinkQuality.Full, changedAt, changedAt, rampIntervalMs = 0),
        )
        assertEquals(
            LinkQuality.FrameRateLowered,
            advanceQuality(LinkQuality.VideoOff, LinkQuality.Full, changedAt, changedAt + 1, rampIntervalMs = 0),
        )
    }

    @Test
    fun aRungThatHasNotMovedIsNotAStep() {
        assertEquals(
            LinkQuality.Full,
            advanceQuality(LinkQuality.Full, LinkQuality.Full, changedAtMs = 1000, nowMs = 1000),
        )
    }

    @Test
    fun aFirstSampleEstablishesTheBaselineWithoutReporting() {
        val first = counters(at = 0, packetsReceived = 1000)
        val second = counters(at = 2000, packetsReceived = 2000)

        assertNull(advanceMeasurement(LinkQuality.Full, null, first, 0, 0))

        val step = advanceMeasurement(LinkQuality.Full, first, second, changedAtMs = 0, nowMs = 2000)

        assertNotNull(step)
        assertFalse(step!!.changed)
        assertEquals(LinkQuality.Full, step.quality)
        assertEquals(second, step.counters)
        assertEquals(0L, step.stats.packetLossPct)
    }

    @Test
    fun aStepOntoAnotherRungIsWhatAReportIsSentOn() {
        val previous = counters(at = 0, packetsReceived = 1000)
        val congested = counters(at = 2000, packetsLost = 400, packetsReceived = 1600)

        val step = advanceMeasurement(
            current = LinkQuality.Full,
            previous = previous,
            counters = congested,
            changedAtMs = 0,
            nowMs = 2000,
        )

        assertNotNull(step)
        assertTrue(step!!.changed)
        assertEquals(LinkQuality.VideoOff, step.quality)
        assertEquals(40L, step.stats.packetLossPct)
    }

    @Test
    fun theReportCarriesLossPerTenThousandAndAnUnknownRelayAsUnknown() {
        val measured = stats(packetLossPct = 2, rttMs = 140, jitterMs = 10)

        val straight = qualityReport(measured, counters(relay = false))
        assertEquals(200L, straight.packetLoss)
        assertEquals(140L, straight.rttMs)
        assertEquals(10L, straight.jitterMs)
        assertEquals(false, straight.usedTurn)

        val relayed = qualityReport(measured, counters(relay = true))
        assertEquals(true, relayed.usedTurn)

        // No nominated pair yet is not the same fact as a pair that is not relaying.
        assertNull(qualityReport(measured, counters(relay = null)).usedTurn)
    }
}
