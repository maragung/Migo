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
}
