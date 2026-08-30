package com.migo.core.domain

import com.migo.core.protocol.BadgeWire
import com.migo.core.protocol.BadgesReq
import com.migo.core.protocol.BadgesResponse
import com.migo.core.protocol.GiftCatalogueReq
import com.migo.core.protocol.GiftCatalogueResponse
import com.migo.core.protocol.GiftListing
import com.migo.core.protocol.GiftSend
import com.migo.core.protocol.GiftSendResult
import com.migo.core.protocol.LeaderboardReq
import com.migo.core.protocol.LeaderboardResponse
import com.migo.core.protocol.LedgerEntryWire
import com.migo.core.protocol.LedgerReq
import com.migo.core.protocol.LedgerResponse
import com.migo.core.protocol.Op
import com.migo.core.protocol.ProgressionReq
import com.migo.core.protocol.ProgressionWire
import com.migo.core.protocol.RankWire
import com.migo.core.protocol.WalletReq
import com.migo.core.protocol.WalletView
import com.migo.core.wire.Id

/**
 * The virtual economy: the caller's wallet, the gift shop, the statement, progression, badges, and
 * the leaderboards.
 *
 * A port of `packages/sdk/src/domains/economy.ts`. Everything here is either the caller's own
 * economy or a public catalogue/standing, so every method is a plain read except [sendGift], and
 * even that is one atomic server-side move: the price is deducted and the transfer recorded together,
 * and a short balance rejects rather than leaving a half-sent gift.
 *
 * # The ledger's sign comes from the reason, never the amount
 *
 * A [LedgerEntryWire]'s `amount` is a magnitude; the *reason* names the direction (`gift_purchase`
 * debits, `gift_reputation` credits). A client that read the sign off anything else would show money
 * moving the wrong way -- invisible to any schema check, because the number is still there, just
 * wrong. The same closed mapping is what the caller renders.
 *
 * # Progression and badges are public facts
 *
 * [getProgression] and [getBadges] serve any account id, not only the caller's own -- the caller's
 * own wallet ([getBalance], [getLedger]) is the only private half, and the server never serves one
 * account's ledger to another.
 */
class EconomyDomain(
    private val rpc: Rpc,
) {
    /** Reads the caller's wallet: the coin balance and the points balance. */
    suspend fun getBalance(): WalletView {
        val request = WalletReq()
        return rpc.call(Op.BALANCE_FETCH, { w -> request.encode(w) }, { r -> WalletView.decode(r) })
    }

    /**
     * Buys and delivers a gift to an account.
     *
     * `gift` is a SKU from [getGiftCatalogue]. The price is deducted from the caller's balance and
     * the transfer recorded, both atomically server-side; a short balance rejects with an error
     * rather than a partial send. `conversationId`, when the gift is being sent inside an open
     * conversation, lets the server attach the transfer to it for the participants' ledgers.
     */
    suspend fun sendGift(gift: String, recipient: Id, conversationId: Id? = null): GiftSendResult {
        val request = GiftSend(gift, recipient, conversationId)
        return rpc.call(Op.GIFT_SEND, { w -> request.encode(w) }, { r -> GiftSendResult.decode(r) })
    }

    /**
     * Reads the gift catalogue: SKU, name, price, and category per listing.
     *
     * The catalogue is global and versionless -- prices change server-side and a client re-reads the
     * catalogue before charging a user's eyes with a price, rather than caching it across sessions.
     */
    suspend fun getGiftCatalogue(): List<GiftListing> {
        val request = GiftCatalogueReq()
        val response = rpc.call(
            Op.GIFT_CATALOGUE,
            { w -> request.encode(w) },
            { r -> GiftCatalogueResponse.decode(r) },
        )
        return response.gifts
    }

    /**
     * Reads the caller's own statement, newest first.
     *
     * A line carries the transfer's reason (the sign comes from it, not the amount -- see the class
     * doc), the balance after, and an optional reference id (the other party of a transfer). Only the
     * caller's own ledger is ever served. `limit` is clamped server-side; null for the default page.
     */
    suspend fun getLedger(limit: Long? = null): List<LedgerEntryWire> {
        val request = LedgerReq(limit = limit)
        val response = rpc.call(
            Op.LEDGER_HISTORY,
            { w -> request.encode(w) },
            { r -> LedgerResponse.decode(r) },
        )
        return response.entries
    }

    /**
     * Reads one account's XP progression: level, XP into the level, and XP the next level needs.
     *
     * Public: pass any account id (typically the profile being viewed, or the caller's own). The
     * progress bar is `xpIntoLevel` of `xpForNextLevel`.
     */
    suspend fun getProgression(ofAccount: Id): ProgressionWire {
        val request = ProgressionReq(ofAccount)
        return rpc.call(Op.PROGRESSION, { w -> request.encode(w) }, { r -> ProgressionWire.decode(r) })
    }

    /**
     * Reads one account's badges: code and award timestamp.
     *
     * Public, like progression. The badge codes are a closed server-owned vocabulary; a client maps
     * them to labels and art it ships itself.
     */
    suspend fun getBadges(ofAccount: Id): List<BadgeWire> {
        val request = BadgesReq(ofAccount)
        val response = rpc.call(
            Op.BADGES,
            { w -> request.encode(w) },
            { r -> BadgesResponse.decode(r) },
        )
        return response.badges
    }

    /**
     * Reads a leaderboard page, strongest first.
     *
     * `board` names which standing to read (the closed server-owned vocabulary, e.g. `"xp"`); `limit`
     * is clamped server-side and null asks for the server's default page. The page is a snapshot:
     * ranks move as accounts earn, and a client re-reads rather than patching locally.
     */
    suspend fun getLeaderboard(board: String, limit: Long? = null): List<RankWire> {
        val request = LeaderboardReq(board, limit)
        val response = rpc.call(
            Op.LEADERBOARD,
            { w -> request.encode(w) },
            { r -> LeaderboardResponse.decode(r) },
        )
        return response.ranks
    }
}
