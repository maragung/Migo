package com.migo.core.security

import com.migo.core.net.DeviceSummary
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The security checkup's rules (§50), tested where they run: the :core unit tests the Android
 * CI gate executes on a plain JVM.
 *
 * The two rules under test are the ones a screen must not improvise. The 30-day device rule
 * decides which phone gets named in a warning, and the backup-freshness transitions are the
 * checkup's only stateful logic — written on a successful export and a successful rotation,
 * read back on every launch — so a wrong transition here is a wrong statement about whether a
 * container can still vouch for the account, shown to the one person who acted on it.
 */
class CheckupTest {
    /** A device row, positional for the fields the rule reads and constants for the rest. */
    private fun device(
        displayName: String,
        status: String,
        lastSeenAtMs: Long,
    ): DeviceSummary = DeviceSummary(
        deviceId = "test-device",
        displayName = displayName,
        platform = "android",
        status = status,
        createdAtMs = 0L,
        lastSeenAtMs = lastSeenAtMs,
        hasCredential = true,
        isCurrent = false,
    )

    // --- the device-age rule ----------------------------------------------------------

    @Test
    fun oldActiveDevices_noneWhenAllRecent() {
        val now = 1_000_000_000_000L
        val devices = listOf(
            device("Migo for Android", "active", now),
            device("Migo for Android", "active", now - 5 * 86_400_000L),
        )
        assertTrue(oldActiveDevices(devices, now).isEmpty())
    }

    @Test
    fun oldActiveDevices_namesTheStaleActiveDevice() {
        val now = 1_000_000_000_000L
        val stale = device("Old phone", "active", now - OLD_DEVICE_THRESHOLD_MS - 1L)
        val devices = listOf(device("New phone", "active", now), stale)
        assertEquals(listOf("Old phone"), oldActiveDevices(devices, now).map { it.displayName })
    }

    @Test
    fun oldActiveDevices_includesTheBoundaryDayExactly() {
        val now = 1_000_000_000_000L
        // Exactly thirty days is over thirty days old, not "nearly": the warning's words say
        // "over 30 days ago" and the rule agrees with them.
        val boundary = device("Boundary phone", "active", now - OLD_DEVICE_THRESHOLD_MS)
        assertEquals(1, oldActiveDevices(listOf(boundary), now).size)
    }

    @Test
    fun oldActiveDevices_ignoresRevokedAndPendingHoweverStale() {
        val now = 1_000_000_000_000L
        val longAgo = now - 10 * OLD_DEVICE_THRESHOLD_MS
        val devices = listOf(
            device("Revoked phone", "revoked", longAgo),
            device("Pending phone", "pending", longAgo),
        )
        // A revoked device cannot authenticate any more, so however long ago it was seen is
        // history rather than a warning; a pending one never connected at all.
        assertTrue(oldActiveDevices(devices, now).isEmpty())
    }

    @Test
    fun oldActiveDevices_futureLastSeenIsRecentNotAncient() {
        val now = 1_000_000_000_000L
        // A server clock ahead of the device's is clock skew, not a device unseen for a
        // negative number of days; the rule must not decide "infinitely old" from it.
        val skewed = device("Skewed clock", "active", now + 86_400_000L)
        assertTrue(oldActiveDevices(listOf(skewed), now).isEmpty())
    }

    // --- the backup-freshness transitions ----------------------------------------------

    @Test
    fun backupFreshness_neverWhenNoContainerWasWritten() {
        assertEquals(BackupFreshness.NeverBackedUp, backupFreshness(lastExportMs = 0L, lastIdentityRotationMs = 0L))
        // A rotation without any export is still "never backed up": there is no container to
        // be out of date, and the warning that matters is the missing one.
        assertEquals(BackupFreshness.NeverBackedUp, backupFreshness(lastExportMs = 0L, lastIdentityRotationMs = 500L))
    }

    @Test
    fun backupFreshness_freshWhenExportFollowsRotation() {
        val state = backupFreshness(lastExportMs = 1_000L, lastIdentityRotationMs = 500L)
        assertEquals(BackupFreshness.BackedUp(1_000L), state)
    }

    @Test
    fun backupFreshness_freshWhenNothingEverRotated() {
        val state = backupFreshness(lastExportMs = 1_000L, lastIdentityRotationMs = 0L)
        assertEquals(BackupFreshness.BackedUp(1_000L), state)
    }

    @Test
    fun backupFreshness_outdatedWhenRotationFollowsExport() {
        val state = backupFreshness(lastExportMs = 500L, lastIdentityRotationMs = 1_000L)
        assertEquals(BackupFreshness.Outdated(atMs = 500L, rotatedAtMs = 1_000L), state)
    }

    @Test
    fun backupFreshness_exportAtTheRotationMillisecondIsFresh() {
        // The tie is not a real state — no container lands in the same millisecond as a
        // rotation the person triggered separately — and "outdated" must never be the answer a
        // rounding decides.
        val state = backupFreshness(lastExportMs = 1_000L, lastIdentityRotationMs = 1_000L)
        assertEquals(BackupFreshness.BackedUp(1_000L), state)
    }

    // --- the date label ------------------------------------------------------------------

    @Test
    fun backupDateLabel_blankForNeverAndForUnusableTimestamps() {
        assertEquals("", backupDateLabel(0L))
        assertEquals("", backupDateLabel(Long.MIN_VALUE))
    }

    @Test
    fun backupDateLabel_namesTheDate() {
        // The exact string is the device's locale and calendar, which the test should not
        // guess; the contract is that a real timestamp produces a non-empty date.
        assertTrue(backupDateLabel(1_700_000_000_000L).isNotEmpty())
    }
}
