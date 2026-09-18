package com.migo.app.call

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The rung-to-caps mapping pinned on both sides of every number it writes.
 *
 * The cases that matter are the ones where a cap is *given back*: a climbing call must write the
 * neutral value over the cap it left behind, because a member left out of a sender's parameters is a
 * member the receiver keeps. A test that only checked the ceilings going down would pass on an
 * implementation that can never recover.
 */
class VideoCapsTest {

    @Test
    fun theTopRungWritesNeutralValuesRatherThanNothing() {
        val caps = videoCaps(LinkQuality.Full, measuredKbps = 900)

        assertTrue(caps.active)
        assertEquals(100_000_000, caps.maxBitrateBps)
        assertEquals(1.0, caps.scaleResolutionDownBy, 0.0)
        assertEquals(60, caps.maxFramerate)
    }

    @Test
    fun theFirstRungDownCapsBitrateAndNothingElse() {
        val caps = videoCaps(LinkQuality.BitrateCapped, measuredKbps = 1000)

        assertTrue(caps.active)
        assertEquals(600_000, caps.maxBitrateBps)
        assertEquals(1.0, caps.scaleResolutionDownBy, 0.0)
        assertEquals(60, caps.maxFramerate)
    }

    @Test
    fun aCapIsAFloorAndNotAScalingOfNothing() {
        // Sixty per cent of a link measuring almost nothing is not a video rate, so the cap stops at
        // the floor rather than asking the encoder for a few kilobits it cannot produce.
        val caps = videoCaps(LinkQuality.BitrateCapped, measuredKbps = 50)

        assertEquals(60_000, caps.maxBitrateBps)
    }

    @Test
    fun theTwoMiddleRungsSacrificeInLadderOrder() {
        val resolution = videoCaps(LinkQuality.ResolutionLowered, measuredKbps = 1000)
        assertEquals(600_000, resolution.maxBitrateBps)
        assertEquals(2.0, resolution.scaleResolutionDownBy, 0.0)
        assertEquals(60, resolution.maxFramerate)

        val frameRate = videoCaps(LinkQuality.FrameRateLowered, measuredKbps = 1000)
        assertEquals(400_000, frameRate.maxBitrateBps)
        assertEquals(2.0, frameRate.scaleResolutionDownBy, 0.0)
        assertEquals(15, frameRate.maxFramerate)
    }

    @Test
    fun theBottomRungSendsNoVideo() {
        val caps = videoCaps(LinkQuality.VideoOff, measuredKbps = 1000)

        assertFalse(caps.active)
    }

    @Test
    fun onlyAVideoCallOnTheBottomRungIsDegraded() {
        assertTrue(degradedAt(LinkQuality.VideoOff, isVideo = true))
        // A voice call has no video to pause, so its rung cannot describe a loss it never had.
        assertFalse(degradedAt(LinkQuality.VideoOff, isVideo = false))
        assertFalse(degradedAt(LinkQuality.FrameRateLowered, isVideo = true))
        assertFalse(degradedAt(LinkQuality.Full, isVideo = true))
    }

    @Test
    fun theRungsAUserMayPinAreAllButTheBottomOne() {
        assertEquals(
            listOf(
                LinkQuality.Full,
                LinkQuality.BitrateCapped,
                LinkQuality.ResolutionLowered,
                LinkQuality.FrameRateLowered,
            ),
            QUALITY_CEILINGS,
        )
        // Video off is the camera button's job: pinning it would put the call into a state that says
        // the quality dropped, which is not what a user choosing it did.
        assertFalse(QUALITY_CEILINGS.contains(LinkQuality.VideoOff))
    }

    @Test
    fun aCeilingNeverLiftsACallAboveItsOwnLink() {
        // Automatic: the ladder's own rung, untouched.
        assertEquals(
            LinkQuality.BitrateCapped,
            cappedQuality(LinkQuality.BitrateCapped, ceiling = null, lowBandwidth = false),
        )
        // A pin below the measured rung lowers the call.
        assertEquals(
            LinkQuality.FrameRateLowered,
            cappedQuality(LinkQuality.BitrateCapped, LinkQuality.FrameRateLowered, lowBandwidth = false),
        )
        // A pin above it changes nothing: no control makes a link carry more than it can.
        assertEquals(
            LinkQuality.BitrateCapped,
            cappedQuality(LinkQuality.BitrateCapped, LinkQuality.Full, lowBandwidth = false),
        )
    }

    @Test
    fun lowBandwidthIsASecondCeilingAndNotItsOwnState() {
        // The mode alone pins the call to the lowest rung that still carries video.
        assertEquals(
            LinkQuality.FrameRateLowered,
            cappedQuality(LinkQuality.Full, ceiling = null, lowBandwidth = true),
        )
        // It is a ceiling like the pin is, so the lower of the two wins and neither lifts the other.
        assertEquals(
            LinkQuality.FrameRateLowered,
            cappedQuality(LinkQuality.Full, LinkQuality.BitrateCapped, lowBandwidth = true),
        )
        // Turning the mode off leaves the rung the pin alone would have left, because the mode was
        // never a state of its own to restore.
        assertEquals(
            LinkQuality.ResolutionLowered,
            cappedQuality(LinkQuality.Full, LinkQuality.ResolutionLowered, lowBandwidth = false),
        )
        // The ladder may still descend below both: a control is a ceiling, not a floor, so a call
        // whose link fell to the bottom is not lifted off it by anything the user pinned.
        assertEquals(
            LinkQuality.VideoOff,
            cappedQuality(LinkQuality.VideoOff, LinkQuality.Full, lowBandwidth = true),
        )
    }

    @Test
    fun theAudioCapIsWrittenInBothDirections() {
        assertEquals(16_000, audioCaps(lowBandwidth = true))
        // Lifted by a number rather than by an omission, or the call stays in narrowband for life.
        assertEquals(100_000_000, audioCaps(lowBandwidth = false))
    }
}
