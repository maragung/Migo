/**
 * The economy domain: the balance, the gift shop, and the statement.
 *
 * Everything here is virtual and non-monetary (the protocol's brief is explicit on this): a balance
 * of in-app currency, gifts priced in it, XP and badges earned through use. There is no fiat value,
 * no withdrawal, and no payment credential anywhere in this path — the only mutation is
 * {@link sendGift}, which moves the caller's own virtual balance to another account, server-ruled
 * and server-recorded.
 *
 * # Why every read is authenticated but addressed
 *
 * The wallet ({@link getBalance}) and the ledger ({@link getLedger}) are the caller's own and are
 * implied by the session. Progression and badges ({@link getProgression}, {@link getBadges}) are
 * public standing and take an explicit account id, so a profile view can show another account's
 * level and honours. The split is deliberate: money is private, standing is not.
 *
 * # Gifts
 *
 * A gift is bought by SKU from the catalogue ({@link getGiftCatalogue}) and delivered by
 * {@link sendGift}; the price is deducted and the transfer recorded as a ledger line, atomically
 * server-side. The result carries the transaction id and nothing else — the recipient's balance is
 * the recipient's business, and a gift already sent in the currently-open conversation needs no
 * separate announcement here (the conversation's own message flow is the announcement).
 */

import type { Id } from '@migo/wire';
import {
  OP,
  encodeWalletReq,
  decodeWalletView,
  encodeGiftSend,
  decodeGiftSendResult,
  encodeGiftCatalogueReq,
  decodeGiftCatalogueResponse,
  encodeLedgerReq,
  decodeLedgerResponse,
  encodeProgressionReq,
  decodeProgressionWire,
  encodeBadgesReq,
  decodeBadgesResponse,
  encodeLeaderboardReq,
  decodeLeaderboardResponse,
} from '@migo/protocol';
import type {
  BadgeWire,
  BadgesReq,
  GiftCatalogueReq,
  GiftListing,
  GiftSend,
  GiftSendResult,
  LedgerEntryWire,
  LedgerReq,
  LeaderboardReq,
  LeaderboardResponse,
  ProgressionReq,
  ProgressionWire,
  RankWire,
  WalletReq,
  WalletView,
} from '@migo/protocol';

import type { Rpc } from './rpc.js';

/**
 * Read the wallet, send gifts, and follow XP, badges, and the statement.
 *
 * One instance per client. Stateless: every method is a plain request/response, so there is nothing
 * to start or stop.
 */
export class EconomyDomain {
  readonly #rpc: Rpc;

  constructor(rpc: Rpc) {
    this.#rpc = rpc;
  }

  /**
   * Reads the caller's own wallet: virtual balance and points.
   *
   * Implied by the session; there is no way to read another account's wallet.
   */
  async getBalance(): Promise<WalletView> {
    const request: WalletReq = {};
    return this.#rpc.call(OP.BALANCE_FETCH, encodeWalletReq, decodeWalletView, request);
  }

  /**
   * Buys and delivers a gift to an account.
   *
   * `gift` is a SKU from {@link getGiftCatalogue}. The price is deducted from the caller's balance
   * and the transfer recorded, both atomically server-side; a short balance rejects with an error
   * rather than a partial send. `conversationId`, when the gift is being sent inside an open
   * conversation, lets the server attach the transfer to it for the participants' ledgers.
   */
  async sendGift(gift: string, recipient: Id, conversationId?: Id): Promise<GiftSendResult> {
    const request: GiftSend = { gift, recipient };
    if (conversationId !== undefined) {
      request.conversationId = conversationId;
    }
    return this.#rpc.call(OP.GIFT_SEND, encodeGiftSend, decodeGiftSendResult, request);
  }

  /**
   * Reads the gift catalogue: SKU, name, price, and category per listing.
   *
   * The catalogue is global and versionless — prices change server-side and a client re-reads the
   * catalogue before charging a user's eyes with a price, rather than caching it across sessions.
   */
  async getGiftCatalogue(): Promise<GiftListing[]> {
    const request: GiftCatalogueReq = {};
    const response = await this.#rpc.call(
      OP.GIFT_CATALOGUE,
      encodeGiftCatalogueReq,
      decodeGiftCatalogueResponse,
      request,
    );
    return response.gifts;
  }

  /**
   * Reads the caller's own statement, newest first.
   *
   * Each {@link LedgerEntryWire} carries the signed-by-convention amount (the `reason` names the
   * direction: a gift sent debits, a gift received credits), the balance after, and an optional
   * reference id (the other party of a transfer). Only the caller's own ledger is ever served.
   */
  async getLedger(limit?: number): Promise<LedgerEntryWire[]> {
    const request: LedgerReq = {};
    if (limit !== undefined) {
      request.limit = limit;
    }
    const response = await this.#rpc.call(
      OP.LEDGER_HISTORY,
      encodeLedgerReq,
      decodeLedgerResponse,
      request,
    );
    return response.entries;
  }

  /**
   * Reads one account's XP standing and level progress.
   *
   * Public: pass any account id (typically the profile being viewed, or the caller's own). The
   * progress bar is `xpIntoLevel` of `xpForNextLevel`.
   */
  async getProgression(ofAccount: Id): Promise<ProgressionWire> {
    const request: ProgressionReq = { ofAccount };
    return this.#rpc.call(OP.PROGRESSION, encodeProgressionReq, decodeProgressionWire, request);
  }

  /**
   * Reads one account's badges: code and award timestamp.
   *
   * Public, like progression. The badge codes are a closed server-owned vocabulary; a client maps
   * them to labels and art it ships itself.
   */
  async getBadges(ofAccount: Id): Promise<BadgeWire[]> {
    const request: BadgesReq = { ofAccount };
    const response = await this.#rpc.call(
      OP.BADGES,
      encodeBadgesReq,
      decodeBadgesResponse,
      request,
    );
    return response.badges;
  }

  /**
   * Reads a leaderboard page, strongest first.
   *
   * `board` names which standing to read (the closed server-owned vocabulary, e.g. `"xp"` or
   * `"reputation"`); each {@link RankWire} line carries the position, account, XP, and level.
   * `limit` bounds the page and is clamped server-side — omit it for the server's default page.
   */
  async getLeaderboard(board: string, limit?: number): Promise<RankWire[]> {
    const request: LeaderboardReq = { board };
    if (limit !== undefined) {
      request.limit = limit;
    }
    const response: LeaderboardResponse = await this.#rpc.call(
      OP.LEADERBOARD,
      encodeLeaderboardReq,
      decodeLeaderboardResponse,
      request,
    );
    return response.ranks;
  }
}
