package com.migo.core.store

import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import java.math.BigInteger

/**
 * One tracked AVAX transaction: what was sent, and how the tracker ended (§184, spec #59).
 *
 * The chain has no "list transactions by sender" without an indexer, so the Activity list is a
 * client-side record — and it is sealed in the vault like everything else there, because it is the
 * account's financial history. Only the record rides: no key material, no private bytes, nothing
 * the chain itself could not republish.
 *
 * Written at broadcast with the outcome it had then and updated when the tracker settles, so a
 * process death mid-tracking loses the ending but never the fact that value left. The fields are
 * the ones the send screen confirmed — the same rule as the signature itself: what is recorded is
 * what was displayed.
 *
 * Not a `data class`: the derived `equals` over two transaction lists invites comparing whole
 * histories, and the wei fields are [BigInteger] precisely so no wider type can ever hold them.
 */
class TxRecord(
    /** The transaction hash, the handle the chain knows it by. */
    val txHash: ByteArray,
    /** The chain the transaction was signed for — EIP-155's replay protection, restated. */
    val chainId: Long,
    /** The recipient. */
    val to: ByteArray,
    /** The amount, wei. */
    val valueWei: BigInteger,
    /** The fee ceiling the user confirmed: `maxFeePerGas * gasLimit`, wei. */
    val feeWei: BigInteger,
    /** The gas limit that was signed. */
    val gasLimit: Long,
    /** When the transaction was broadcast, unix seconds. */
    val atUnix: Long,
    /** Spec #41's own word for where the transaction stands: `PENDING` at broadcast, one of the
     *  tracker's endings once it settles. */
    val outcome: String,
    /** The block that included the transaction, once one did. */
    val block: Long?,
    /** The gas the block actually spent on it, from the receipt — the ceiling's honest companion. */
    val gasUsed: BigInteger?,
) {
    /** Public shape only: amounts and states, never key material (there is none to print). */
    override fun toString(): String =
        "TxRecord(chain_id: $chainId, value_wei: $valueWei, outcome: $outcome, block: $block)"
}

/**
 * Writes one record. Wei magnitudes ride as decimal strings — the same convention the
 * cross-language account vectors use for transaction integers, because a 10^18 value does not fit
 * any wire integer and a string needs no second format to explain it.
 */
internal fun writeTxRecord(w: Writer, record: TxRecord) {
    w.bytes(record.txHash)
    w.u64(record.chainId)
    w.bytes(record.to)
    w.str(record.valueWei.toString(10))
    w.str(record.feeWei.toString(10))
    w.u64(record.gasLimit)
    w.u64(record.atUnix)
    w.str(record.outcome)
    if (record.block != null) {
        w.bool(true)
        w.u64(record.block)
    } else {
        w.bool(false)
    }
    if (record.gasUsed != null) {
        w.bool(true)
        w.str(record.gasUsed.toString(10))
    } else {
        w.bool(false)
    }
}

/**
 * Reads what [writeTxRecord] wrote. Any inconsistency throws — a malformed record refuses the
 * whole field rather than being half-read, because an Activity list that silently dropped its
 * middle is a lie about where money went.
 */
internal fun readTxRecord(r: Reader): TxRecord {
    val txHash = r.bytes()
    if (txHash.size != 32) throw VaultError.Unreadable
    val chainId = r.u64()
    val to = r.bytes()
    if (to.size != 20) throw VaultError.Unreadable
    val valueWei = BigInteger(r.str(), 10)
    val feeWei = BigInteger(r.str(), 10)
    val gasLimit = r.u64()
    val atUnix = r.u64()
    val outcome = r.str()
    val block = if (r.bool()) r.u64() else null
    val gasUsed = if (r.bool()) BigInteger(r.str(), 10) else null
    return TxRecord(txHash, chainId, to, valueWei, feeWei, gasLimit, atUnix, outcome, block, gasUsed)
}
