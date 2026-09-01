package com.migo.core.store

import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import java.math.BigInteger
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.fail
import org.junit.Test

/**
 * The tracked-transaction record's wire round-trip, and the refusals that guard it.
 *
 * The record is the account's financial history sealed in the vault, so the encoding is not allowed
 * to drift: what [writeTxRecord] writes is what [readTxRecord] reads, and a record that does not
 * hold its own shape refuses the whole field rather than being half-read.
 */
class TxRecordTest {
    /** A full record: every optional field present, wei magnitudes that need 18 digits. */
    private fun fullRecord() = TxRecord(
        txHash = ByteArray(32) { (it + 1).toByte() },
        chainId = 43_114L,
        to = ByteArray(20) { (it * 3).toByte() },
        valueWei = BigInteger("1500000000000000000"),
        feeWei = BigInteger("2625000000000000"),
        gasLimit = 21_000L,
        atUnix = 1_770_000_000L,
        outcome = "CONFIRMED",
        block = 620_000_000L,
        gasUsed = BigInteger("21000"),
    )

    @Test
    fun `a full record round-trips through the writer and reader`() {
        val record = fullRecord()
        val w = Writer()
        writeTxRecord(w, record)
        val read = readTxRecord(Reader(w.finish()))

        assertArrayEquals(record.txHash, read.txHash)
        assertEquals(record.chainId, read.chainId)
        assertArrayEquals(record.to, read.to)
        assertEquals(record.valueWei, read.valueWei)
        assertEquals(record.feeWei, read.feeWei)
        assertEquals(record.gasLimit, read.gasLimit)
        assertEquals(record.atUnix, read.atUnix)
        assertEquals(record.outcome, read.outcome)
        assertEquals(record.block, read.block)
        assertEquals(record.gasUsed, read.gasUsed)
    }

    @Test
    fun `a pending record round-trips with its optional fields absent`() {
        val record = TxRecord(
            txHash = ByteArray(32) { 0x5a },
            chainId = 43_113L,
            to = ByteArray(20) { 0x11 },
            valueWei = BigInteger.ONE,
            feeWei = BigInteger.TEN,
            gasLimit = 30_000L,
            atUnix = 42L,
            outcome = "PENDING",
            block = null,
            gasUsed = null,
        )
        val w = Writer()
        writeTxRecord(w, record)
        val read = readTxRecord(Reader(w.finish()))

        assertEquals("PENDING", read.outcome)
        assertEquals(null, read.block)
        assertEquals(null, read.gasUsed)
        assertEquals(record.valueWei, read.valueWei)
    }

    @Test
    fun `a hash that is not 32 bytes refuses the whole read`() {
        val record = fullRecord()
        val w = Writer()
        w.bytes(ByteArray(31))
        w.u64(record.chainId)
        w.bytes(record.to)
        w.str("1")
        w.str("1")
        w.u64(1L)
        w.u64(1L)
        w.str("PENDING")
        w.bool(false)
        w.bool(false)

        assertThrows(VaultError.Unreadable::class.java) { readTxRecord(Reader(w.finish())) }
    }

    @Test
    fun `a recipient that is not 20 bytes refuses the whole read`() {
        val w = Writer()
        w.bytes(ByteArray(32))
        w.u64(43_114L)
        w.bytes(ByteArray(19))
        w.str("1")
        w.str("1")
        w.u64(1L)
        w.u64(1L)
        w.str("PENDING")
        w.bool(false)
        w.bool(false)

        assertThrows(VaultError.Unreadable::class.java) { readTxRecord(Reader(w.finish())) }
    }

    @Test
    fun `a truncated frame never reads a record quietly`() {
        val record = fullRecord()
        val w = Writer()
        writeTxRecord(w, record)
        val whole = w.finish()
        // Every prefix shorter than the frame must refuse: a record that silently lost its middle
        // is a lie about where money went.
        for (cut in 0 until whole.size) {
            try {
                readTxRecord(Reader(whole.copyOf(cut)))
                fail("a frame cut at $cut bytes should not have read")
            } catch (_: VaultError) {
                // The read refused, which is the pass condition.
            } catch (_: Exception) {
                // Any other refusal of a hostile frame is also a refusal.
            }
        }
    }
}
