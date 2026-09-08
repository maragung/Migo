package com.migo.core.security

import com.migo.core.net.DeviceSummary
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/**
 * The security checkup's pure rules (spec §50): the row set is fixed across clients — Identity,
 * Devices, Wallets, Backup, Recovery, E2EE — and each row's warning is a rule about data, not a
 * judgement a screen should improvise. The rules live here, in the SDK module with no Android
 * imports, so the same :core unit tests that run in CI cover exactly what the Profile screen
 * renders. A screen that computed "old device" inline would be a second place the 30-day rule
 * could drift from the one the desktop and web clients build.
 */

/** Thirty days in milliseconds: the age at which an active device stops being "recent". */
const val OLD_DEVICE_THRESHOLD_MS: Long = 30L * 24L * 60L * 60L * 1000L

/**
 * The devices that are still *active* — able to authenticate — but have not connected in longer
 * than the threshold.
 *
 * A revoked device never qualifies, however long ago it was seen: its row is history, and
 * warning about history would bury the one warning that matters, which is a device that can
 * still sign in and has not. A `lastSeenAtMs` in the future (server clock skew, a client clock
 * set by hand) reads as recent rather than infinitely old, because a clock that disagrees with
 * the device's own is not evidence the device is stale.
 */
fun oldActiveDevices(
    devices: List<DeviceSummary>,
    nowMs: Long,
    thresholdMs: Long = OLD_DEVICE_THRESHOLD_MS,
): List<DeviceSummary> = devices.filter { row ->
    row.status == "active" && nowMs - row.lastSeenAtMs >= thresholdMs
}

/**
 * The Backup row's honest state, derived from the two timestamps this device persists: when it
 * last wrote a `.migo` container, and when the account's identity key last rotated.
 *
 * The transitions are the whole point. A successful export records the first timestamp; a
 * successful rotation records the second, and every container sealed *before* that rotation
 * carries the retired key as its identity half — it can no longer vouch for the account onto a
 * new device, so the row says so rather than letting a pre-rotation backup reassure anybody.
 * Writing a fresh container after the rotation is the only thing that returns the row to
 * [BackupFreshness.BackedUp].
 */
sealed interface BackupFreshness {
    /** No `.migo` container has ever been written on this device. */
    data object NeverBackedUp : BackupFreshness

    /** A container was written, and nothing has retired what it seals since. */
    data class BackedUp(val atMs: Long) : BackupFreshness

    /**
     * A container was written, then the identity key rotated: the container still opens, but
     * its identity half is the retired key, so it can no longer vouch for the account.
     */
    data class Outdated(val atMs: Long, val rotatedAtMs: Long) : BackupFreshness
}

/**
 * Reduces the two persisted timestamps to the row's state. Zero means "never" for both, and an
 * export at the exact millisecond of a rotation counts as fresh — the tie is not a real state,
 * and "outdated" should never be the answer a rounding decides.
 */
fun backupFreshness(lastExportMs: Long, lastIdentityRotationMs: Long): BackupFreshness = when {
    lastExportMs <= 0L -> BackupFreshness.NeverBackedUp
    lastIdentityRotationMs > lastExportMs -> BackupFreshness.Outdated(lastExportMs, lastIdentityRotationMs)
    else -> BackupFreshness.BackedUp(lastExportMs)
}

/**
 * A backup timestamp as the device's own short date, for "Backed up <date>".
 *
 * Localised like the message timestamps are, because a date is only readable in the calendar
 * the person uses. Blank rather than crashing on a timestamp outside the supported range —
 * the caller shows the sentence without the date, not without the row.
 */
fun backupDateLabel(atMs: Long): String {
    if (atMs <= 0L) return ""
    return try {
        Instant.ofEpochMilli(atMs)
            .atZone(ZoneId.systemDefault())
            .format(DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM))
    } catch (_: RuntimeException) {
        ""
    }
}
