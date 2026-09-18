package com.migo.app.call

import kotlin.coroutines.resume
import kotlin.math.roundToLong
import kotlinx.coroutines.suspendCancellableCoroutine
import org.webrtc.PeerConnection
import org.webrtc.RTCStatsReport

/**
 * What a call measures about its own link, and where that measurement puts the link on the
 * degradation ladder.
 *
 * The ladder is not invented here. Section 165 pins the order in which a call gives things up --
 * bitrate, then resolution, then frame rate, then video, with audio never a rung and the bottom
 * rung "video off, audio alive" -- and the web build and the node's own decision core classify a
 * link with exactly these numbers. A client that classified the same link differently would make
 * the three disagree about a call none of them can see the whole of, so the arithmetic is ported
 * rather than re-derived: the thresholds are the node's defaults, the score is the node's score,
 * and the ramp is the node's interval.
 *
 * Two halves live in this file on purpose. Reading a connection's counters is the platform's half
 * and can only run against a real [PeerConnection]; everything the reading becomes is pure, takes
 * plain numbers, and is pinned by unit tests that need no network at all. The split is the same one
 * the web build draws between reading `getStats()` and the model that consumes it.
 *
 * What this file does not do yet is act on the rung: the ladder currently decides only when a
 * report leaves the device. Applying a rung to this side's own senders, and showing the rung on the
 * call screen, come with the video caps they need.
 */
enum class LinkQuality {
    Full,
    BitrateCapped,
    ResolutionLowered,
    FrameRateLowered,
    VideoOff,
    ;

    /** The rung's place on the ladder, counting the top rung as zero. */
    val rung: Int get() = ordinal
}

/** The numbers section 165 names, as one link's transport observes them between two readings. */
data class LinkStats(
    /** Loss on the leg over the stretch, whole percents. */
    val packetLossPct: Long,
    /** Round-trip time, milliseconds. */
    val rttMs: Long,
    /** Jitter, milliseconds. */
    val jitterMs: Long,
    /** Bandwidth the leg can still carry, kilobits per second. */
    val availableKbps: Long,
    /** What the leg was sent over the stretch, kilobits per second. */
    val sentKbps: Long,
    /** Frames the receiver dropped over the stretch, whole percents. */
    val droppedFramePct: Long,
)

/** Where a link's congestion score crosses each rung; the node's defaults, the same numbers. */
data class AdaptiveThresholds(
    val rttBaselineMs: Long,
    val bitrateAt: Long,
    val resolutionAt: Long,
    val frameRateAt: Long,
    val videoOffAt: Long,
)

/**
 * The thresholds a call starts with: round-trip time contributes nothing below 120 milliseconds,
 * which is a normal mobile leg rather than a congested one, and the rungs cross at 12, 25, 40, and
 * 60.
 */
val DEFAULT_ADAPTIVE_THRESHOLDS = AdaptiveThresholds(
    rttBaselineMs = 120,
    bitrateAt = 12,
    resolutionAt = 25,
    frameRateAt = 40,
    videoOffAt = 60,
)

/** Recovery climbs one rung per this many milliseconds, since the link last moved. */
const val RAMP_INTERVAL_MS = 3_000L

/**
 * How often a live call samples its own link. Two seconds is the web build's cadence and it is a
 * cadence rather than a rate: a sample is one round trip to the platform's stats collector, and a
 * stretch shorter than this would divide counters that have barely moved and call the noise loss.
 */
const val QUALITY_POLL_MS = 2_000L

/** One raw reading of a connection's cumulative counters, as the platform reports them. */
data class LinkCounters(
    val at: Long,
    val packetsLost: Long,
    val packetsReceived: Long,
    val framesReceived: Long,
    val framesDropped: Long,
    val bytesSent: Long,
    val rttMs: Long,
    val jitterMs: Long,
    /** An outgoing-bandwidth estimate, when the connection reports one; null when it does not. */
    val availableKbps: Long?,
    /**
     * Whether the nominated pair is a relay pair, so the media is going through TURN rather than
     * straight between the two devices. Not a rung and not a score term: section 180 asks for it as
     * a product number, the share of calls that fell back, because that share is what says where the
     * next relay belongs. Null when the connection has named no nominated pair yet, which is not the
     * same fact as false.
     */
    val relay: Boolean?,
)

/** Where a link's congestion score crosses the rungs, from one link's numbers. */
fun congestionScore(stats: LinkStats, thresholds: AdaptiveThresholds): Long {
    val loss = maxOf(0L, stats.packetLossPct) * 4
    val latency = maxOf(0L, stats.rttMs - thresholds.rttBaselineMs) / 10.0
    val jitter = maxOf(0L, stats.jitterMs) / 5.0
    val deficitKbps = maxOf(0L, stats.sentKbps - stats.availableKbps)
    val deficit = deficitKbps / 100
    val dropped = maxOf(0L, stats.droppedFramePct) / 2.0
    return (loss + latency + jitter + deficit + dropped).roundToLong()
}

/**
 * The rung a link's numbers say the link belongs on. A worse score can only name a lower rung,
 * because the thresholds are read in ladder order: the classification cannot skip bitrate and jump
 * to resolution.
 */
fun targetQuality(stats: LinkStats, thresholds: AdaptiveThresholds): LinkQuality {
    val score = congestionScore(stats, thresholds)
    return when {
        score >= thresholds.videoOffAt -> LinkQuality.VideoOff
        score >= thresholds.frameRateAt -> LinkQuality.FrameRateLowered
        score >= thresholds.resolutionAt -> LinkQuality.ResolutionLowered
        score >= thresholds.bitrateAt -> LinkQuality.BitrateCapped
        else -> LinkQuality.Full
    }
}

/**
 * Moves a link from where it is toward where its numbers say it belongs.
 *
 * Down the ladder is immediate: the ladder's order is the order of what is given up, so landing on
 * a low rung has passed through the same sacrifices whether or not each was held for a while. Up
 * the ladder is one rung per [rampIntervalMs] since the link last moved and never a jump, because a
 * jump is the oscillation the whole model exists to prevent -- a link that recovered for one sample
 * would otherwise be handed its full bitrate back and congest itself again.
 */
fun advanceQuality(
    current: LinkQuality,
    target: LinkQuality,
    changedAtMs: Long,
    nowMs: Long,
    rampIntervalMs: Long = RAMP_INTERVAL_MS,
): LinkQuality {
    if (target.rung >= current.rung) {
        return target
    }
    if (nowMs - changedAtMs >= maxOf(1L, rampIntervalMs)) {
        return LinkQuality.entries[maxOf(0, current.rung - 1)]
    }
    return current
}

/**
 * The stats of the stretch between two readings, or null when the stretch is too short or too empty
 * to say anything -- the first reading of a link has no stretch before it.
 *
 * The mapping is honest about what a client can measure: loss, round-trip time, jitter, sent
 * bitrate, and dropped frames are real. A bandwidth estimate is only read where the connection
 * reports one, and where it does not, available equals sent, so the deficit term contributes
 * nothing rather than inventing a number.
 */
fun linkStatsBetween(previous: LinkCounters, next: LinkCounters): LinkStats? {
    val seconds = (next.at - previous.at) / 1000.0
    if (seconds <= 0) {
        return null
    }
    val lost = next.packetsLost - previous.packetsLost
    val received = next.packetsReceived - previous.packetsReceived
    val denominator = lost + received
    if (denominator <= 0) {
        return null
    }
    val frames = next.framesReceived - previous.framesReceived
    val dropped = next.framesDropped - previous.framesDropped
    val sentKbps = maxOf(0.0, (next.bytesSent - previous.bytesSent) * 8.0 / 1000.0 / seconds)
    return LinkStats(
        packetLossPct = (maxOf(0L, lost).toDouble() / denominator * 100).roundToLong(),
        rttMs = maxOf(0L, next.rttMs),
        jitterMs = maxOf(0L, next.jitterMs),
        availableKbps = next.availableKbps ?: sentKbps.roundToLong(),
        sentKbps = sentKbps.roundToLong(),
        droppedFramePct = if (frames + dropped > 0) {
            (maxOf(0L, dropped).toDouble() / (frames + dropped) * 100).roundToLong()
        } else {
            0L
        },
    )
}

/** One step of the ladder: the rung the link is on now, from the reading that put it there. */
data class QualityAdvance(
    val quality: LinkQuality,
    val stats: LinkStats,
    /** The reading the step was taken from, which is what a report needs beside the numbers. */
    val counters: LinkCounters,
    /** Whether the rung moved, which is what a report is sent on. */
    val changed: Boolean,
)

/**
 * One sample of a link turned into a step of the ladder, or null when this sample cannot produce
 * one -- there is no previous reading to measure against, or the stretch between the two is empty.
 *
 * The callers this is written for sample on a timer and act only on a step that [QualityAdvance
 * changed] reports: the report leaving the device is what a rung move costs, and a steady link must
 * cost nothing.
 */
fun advanceMeasurement(
    current: LinkQuality,
    previous: LinkCounters?,
    counters: LinkCounters,
    changedAtMs: Long,
    nowMs: Long,
    thresholds: AdaptiveThresholds = DEFAULT_ADAPTIVE_THRESHOLDS,
    rampIntervalMs: Long = RAMP_INTERVAL_MS,
): QualityAdvance? {
    if (previous == null) {
        return null
    }
    val stats = linkStatsBetween(previous, counters) ?: return null
    val quality =
        advanceQuality(current, targetQuality(stats, thresholds), changedAtMs, nowMs, rampIntervalMs)
    return QualityAdvance(quality, stats, counters, quality != current)
}

/** The measured half of a call-stats report, in the units the wire carries. */
data class LinkQualityReport(
    val rttMs: Long,
    /** Loss per ten thousand, which is the wire's unit rather than the percent it is measured in. */
    val packetLoss: Long,
    val jitterMs: Long,
    /** Null where the connection named no nominated pair, which must not be reported as false. */
    val usedTurn: Boolean?,
)

/**
 * What one reading becomes on the wire: only numbers this call measured. The relay fact rides along
 * as null when the connection never named a pair, since reporting "not relaying" for a connection
 * that has not decided is a claim about a decision nobody made.
 */
fun qualityReport(stats: LinkStats, counters: LinkCounters): LinkQualityReport = LinkQualityReport(
    rttMs = stats.rttMs,
    packetLoss = maxOf(0L, stats.packetLossPct * 100),
    jitterMs = stats.jitterMs,
    usedTurn = counters.relay,
)

/**
 * Reads one raw sample of a connection's counters, or null when it reports nothing usable yet.
 *
 * The platform's half of this file: the report arrives on a callback rather than as a return value,
 * so this waits for it, and a connection closed while the wait is outstanding leaves the wait
 * unanswered -- hence a caller sampling on a timer, which can drop one sample and take the next.
 */
suspend fun readLinkCounters(pc: PeerConnection): LinkCounters? =
    suspendCancellableCoroutine { continuation ->
        pc.getStats { report ->
            if (continuation.isActive) {
                continuation.resume(readCounters(report, System.currentTimeMillis()))
            }
        }
    }

/**
 * One report's members turned into counters, or null when the report holds nothing this model
 * reads.
 */
private fun readCounters(report: RTCStatsReport, at: Long): LinkCounters? {
    val stats = report.statsMap
    // Candidates are named by id from the nominated pair, so they are read before it is: the report
    // yields in no promised order, and a single pass that met the pair first would find no candidate
    // to look up.
    val candidateTypes = HashMap<String, String>()
    for (entry in stats.values) {
        val type = entry.type
        if (type == "local-candidate" || type == "remote-candidate") {
            candidateTypes[entry.id] = (entry.members["candidateType"] as? String) ?: ""
        }
    }
    var sawAnything = false
    var packetsLost = 0L
    var packetsReceived = 0L
    var framesReceived = 0L
    var framesDropped = 0L
    var bytesSent = 0L
    var rttMs = 0L
    var jitterMs = 0L
    var availableKbps: Long? = null
    var relay: Boolean? = null
    for (entry in stats.values) {
        val members = entry.members
        when (entry.type) {
            "inbound-rtp" -> {
                sawAnything = true
                packetsLost += maxOf(0L, count(members["packetsLost"]))
                packetsReceived += count(members["packetsReceived"])
                framesReceived += count(members["framesReceived"])
                framesDropped += count(members["framesDropped"])
                jitterMs = maxOf(jitterMs, millis(members["jitter"]))
            }
            "outbound-rtp" -> {
                sawAnything = true
                bytesSent += count(members["bytesSent"])
            }
            "candidate-pair" -> {
                if (members["state"] == "succeeded" && members["nominated"] == true) {
                    sawAnything = true
                    rttMs = millis(members["currentRoundTripTime"])
                    val estimate = count(members["availableOutgoingBitrate"])
                    if (estimate > 0) {
                        availableKbps = estimate / 1000
                    }
                    // Either end being a relay candidate means the media rides TURN: a relay pair is
                    // one whose local or remote candidate was reflexive from a relay, and both ends
                    // read it the same way.
                    val local = (members["localCandidateId"] as? String)?.let { candidateTypes[it] }
                    val remote = (members["remoteCandidateId"] as? String)?.let { candidateTypes[it] }
                    if (local != null || remote != null) {
                        relay = local == "relay" || remote == "relay"
                    }
                }
            }
        }
    }
    if (!sawAnything) {
        return null
    }
    return LinkCounters(
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
}

/** A report member read as a whole number; members arrive boxed and may be absent. */
private fun count(value: Any?): Long = (value as? Number)?.toLong() ?: 0L

/**
 * A report member holding seconds read as milliseconds.
 *
 * Rounding is guarded rather than trusted: a not-a-number out of a stats report would otherwise
 * throw from inside a platform callback, which is the one place an exception takes the call with it.
 */
private fun millis(value: Any?): Long {
    val seconds = (value as? Number)?.toDouble() ?: 0.0
    return if (seconds.isNaN()) 0L else (seconds * 1000).roundToLong()
}
