package com.migo.app.call

import kotlin.math.roundToLong

/**
 * What one rung of the ladder asks of this side's own video sender.
 *
 * The ladder in [LinkQuality] says where a link stands; this says what to do about it, and it is the
 * web build's mapping down to the numbers so that the two clients send the same picture for the same
 * rung. Every rung names all of its caps, the ones it imposes nothing on included, and that is the
 * point rather than a style: WebRTC reads a member that is absent as "leave the value alone", so a
 * rung that gives a cap back has to write the neutral value over it, or the sacrifice a climbing
 * call has already left behind stays on the wire while the screen shows a tier the call is not on.
 *
 * The caps are a ceiling and never a rate: the bandwidth estimate still decides what actually leaves
 * the machine, so the top rung writes numbers above anything a call can send rather than nothing at
 * all. Two of them have no neutral in the specification -- bitrate and frame rate have no sentinel
 * for "unlimited" -- which is why they take a deliberately high number and resolution takes one,
 * the one value that means no scaling.
 */
data class VideoCaps(
    /** Whether this rung sends video at all; the bottom rung stops the stream without detaching. */
    val active: Boolean,
    /** The most the encoder may send, bits per second. */
    val maxBitrateBps: Int,
    /** How far the picture is scaled down before it is encoded; one is not at all. */
    val scaleResolutionDownBy: Double,
    /** The most frames per second the encoder may send. */
    val maxFramerate: Int,
)

/** The bitrate a rung with no cap asks for: above anything a call can put on a link. */
const val NO_CAP_KBPS = 100_000L

/** The frame rate a rung with no cap asks for, for the same reason. */
const val NO_CAP_FRAMERATE = 60

/** The scale a rung with no resolution sacrifice asks for: no scaling at all. */
const val NO_CAP_SCALE = 1.0

/** The bitrate cap of the first rung down, as a percentage of what the link is sending. */
const val BITRATE_CAP_PCT = 60L

/** The bitrate cap once frame rate has been sacrificed too, as a percentage of the same. */
const val LOW_BITRATE_CAP_PCT = 40L

/** The frame rate the frame-rate rung asks for: half of what a call's camera sends. */
const val FRAME_RATE_HALVED = 15

/**
 * The floor under a bitrate cap. A cap computed from a link that is measuring almost nothing would
 * otherwise be a few kilobits, which is not video at all; a call that has fallen that far gives up
 * video by taking the bottom rung, where a floor would silently be doing the ladder's job for it.
 */
const val MIN_CAP_KBPS = 60L

/**
 * The caps one rung asks of this side's video, measured against what the link is sending.
 *
 * The measured bitrate is what the link is really carrying, so a cap of sixty per cent of it is a
 * request for the same picture at less cost rather than a request for a picture the link never
 * carried. A bottom rung carries no cap because it carries no video: the stream is stopped, and the
 * rung that brings it back writes its own caps over this one.
 */
fun videoCaps(quality: LinkQuality, measuredKbps: Long): VideoCaps = when (quality) {
    LinkQuality.Full -> VideoCaps(true, NO_CAP_KBPS.bps(), NO_CAP_SCALE, NO_CAP_FRAMERATE)
    LinkQuality.BitrateCapped -> VideoCaps(
        active = true,
        maxBitrateBps = cappedBps(measuredKbps, BITRATE_CAP_PCT),
        scaleResolutionDownBy = NO_CAP_SCALE,
        maxFramerate = NO_CAP_FRAMERATE,
    )
    LinkQuality.ResolutionLowered -> VideoCaps(
        active = true,
        maxBitrateBps = cappedBps(measuredKbps, BITRATE_CAP_PCT),
        scaleResolutionDownBy = 2.0,
        maxFramerate = NO_CAP_FRAMERATE,
    )
    LinkQuality.FrameRateLowered -> VideoCaps(
        active = true,
        maxBitrateBps = cappedBps(measuredKbps, LOW_BITRATE_CAP_PCT),
        scaleResolutionDownBy = 2.0,
        maxFramerate = FRAME_RATE_HALVED,
    )
    LinkQuality.VideoOff -> VideoCaps(false, NO_CAP_KBPS.bps(), NO_CAP_SCALE, NO_CAP_FRAMERATE)
}

/**
 * Whether a call at this rung is degraded in the sense section 180 defines: connected, with video
 * paused because the quality dropped.
 *
 * Only a video call can be degraded, because a voice call has no video to pause -- its rung would
 * describe a sacrifice the call never had to make, and the screen would then say a voice call had
 * lost something it never carried.
 */
fun degradedAt(quality: LinkQuality, isVideo: Boolean): Boolean =
    isVideo && quality == LinkQuality.VideoOff

/** A cap in kilobits as the bits per second the sender's parameters carry. */
private fun cappedBps(measuredKbps: Long, pct: Long): Int =
    maxOf(MIN_CAP_KBPS, (measuredKbps * pct / 100.0).roundToLong()).bps()

private fun Long.bps(): Int = (this * 1000).toInt()

/**
 * The rungs a user may pin a call to, top first.
 *
 * The bottom rung is deliberately not among them. A user who wants no video has the camera button,
 * which says so plainly; a ceiling of "video off" would instead put the call into Degraded, a state
 * section 180 defines as video paused because the quality dropped -- and a sacrifice the user chose
 * is not a drop, so offering it here would make the screen say something untrue about why the camera
 * is off.
 */
val QUALITY_CEILINGS: List<LinkQuality> = LinkQuality.entries.dropLast(1)

/**
 * Where low bandwidth mode pins a call: the lowest rung that still carries video.
 *
 * The mode is for a link the user knows is expensive or thin, and it asks for the smallest call that
 * is still the call they started. Audio is capped as well, through [LOW_BANDWIDTH_AUDIO_BPS],
 * because on a voice call the ladder has nothing to give up and the mode would otherwise mean
 * nothing at all.
 */
val LOW_BANDWIDTH_CEILING = LinkQuality.FrameRateLowered

/**
 * What low bandwidth mode asks of the audio sender, bits per second.
 *
 * Audio is never a rung of the ladder -- the bottom of the ladder is "video off, audio alive" -- so
 * the mode reaches a voice call through the audio sender's own cap instead, which is what section
 * 180's low audio bitrate mode asks for. Opus at sixteen kilobits is narrowband speech:
 * intelligible, and about a fifth of what a comfortable full-band call uses. The number is chosen to
 * keep the call alive rather than to sound good, because the mode exists for a link that cannot
 * carry the call the user would rather have.
 */
const val LOW_BANDWIDTH_AUDIO_BPS = 16_000

/**
 * What one audio sender's cap should be, given the mode.
 *
 * Both directions are written, and the lifted one is a number rather than an omission, for the same
 * reason every video cap is: a member left out of a sender's parameters is a member the receiver
 * keeps, so clearing the cap by omitting it would leave a call in narrowband for the rest of its
 * life.
 */
fun audioCaps(lowBandwidth: Boolean): Int =
    if (lowBandwidth) LOW_BANDWIDTH_AUDIO_BPS else NO_CAP_KBPS.bps()

/**
 * The rung a call is actually on, once the two controls that cap it have had their say.
 *
 * A ceiling, never a floor: the ladder may still descend below whatever the user pinned, because no
 * control can make a link carry more than it can, and a client that pretended otherwise would send
 * video into a congested path while the screen claimed a tier the call was not on. [ceiling] is the
 * manual choice and null means automatic; [lowBandwidth] is the mode, and it is applied as a second
 * ceiling rather than as its own state, so turning it off restores exactly the rung that was there
 * before it.
 */
fun cappedQuality(
    quality: LinkQuality,
    ceiling: LinkQuality?,
    lowBandwidth: Boolean,
): LinkQuality {
    var capped = quality
    if (ceiling != null) {
        capped = lowerOf(capped, ceiling)
    }
    if (lowBandwidth) {
        capped = lowerOf(capped, LOW_BANDWIDTH_CEILING)
    }
    return capped
}

/** The rung that gives up more of the two. The ladder's order is what decides, not a second table. */
private fun lowerOf(a: LinkQuality, b: LinkQuality): LinkQuality =
    if (a.rung >= b.rung) a else b
