/**
 * The economy domain: the balance, the gift shop, and the statement.
 *
 * Everything here is virtual and non-monetary (the protocol's brief is explicit on this): a balance
 * of in-app currency, gifts priced in it, XP and badges earned through use. There is no fiat value,
 * no withdrawal, and no payment credential anywhere in this path — the only mutation is
 * {@link sendGift}, which moves the caller's own virtual balance to another account, server-ruled
 * and server-recorded.
 *
 * # Kick Points
 *
 * A Kick Point is prepaid kick credit: an outright group kick costs one, falling back to a
 * 1-coin price when the balance is empty, and refusing when neither is there. The balance is
 * in-system only — it is bought with coins ({@link EconomyDomain.buyKickPoints}) or granted
 * through events, never withdrawn or transferred.
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
 *
 * # The live balance tick
 *
 * Every spend the caller makes — a gift sent, a store purchase, a Kick Point pack — is answered
 * by an {@link EconomyEvent} on the caller's *own user topic*, the topic every session subscribes
 * to at its handshake. The event is a cue, not a fact: it names the kind and the amount but not
 * the resulting balance, so the handler for it is "re-read my wallet" ({@link getBalance}), never
 * arithmetic applied to a number the server did not vouch for. It is what makes the wallet live on
 * a second device and the sender's own balance tick after a send, without either client polling.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  decodeEconomyEvent,
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
  encodeStorePurchase,
  decodeStorePurchaseResult,
  encodeKickPointsBuy,
  decodeKickPointsBuyResult,
  encodeEntitlementsReq,
  decodeEntitlementsResponse,
} from '@migo/protocol';
import type {
  BadgeWire,
  BadgesReq,
  EconomyEvent,
  Entitlement,
  EntitlementsReq,
  EntitlementsResponse,
  GiftCatalogueReq,
  GiftListing,
  GiftSend,
  GiftSendResult,
  KickPointsBuy,
  KickPointsBuyResult,
  LedgerEntryWire,
  LedgerReq,
  LeaderboardReq,
  LeaderboardResponse,
  ProgressionReq,
  ProgressionWire,
  RankWire,
  StorePurchase,
  StorePurchaseResult,
  WalletReq,
  WalletView,
} from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * Read the wallet, send gifts, and follow XP, badges, and the statement.
 *
 * One instance per client. The reads are plain request/response; the one live half is the
 * {@link EconomyEvent} tick, which delivers nothing until {@link start}, so a client registers
 * its handler first and does not miss the first tick.
 */
export class EconomyDomain {
  readonly #rpc: Rpc;
  readonly #listeners: ListenerSet<EconomyEvent>;
  #unsubscribe: (() => void) | null = null;

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#listeners = new ListenerSet(OP.ECONOMY_EVENT, onEventError);
  }

  /**
   * Begins delivering inbound economy events to registered handlers. Idempotent.
   *
   * The frames arrive on the caller's own user topic, which the client subscribes to at its
   * handshake, so there is no topic to choose here — only the handlers to begin feeding.
   */
  start(): void {
    if (this.#unsubscribe !== null) {
      return;
    }
    this.#unsubscribe = this.#rpc.on(OP.ECONOMY_EVENT, decodeEconomyEvent, (event) =>
      this.#listeners.deliver(event),
    );
  }

  /** Stops delivering economy events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }

  /**
   * Registers a handler for the caller's own economy events. Returns an unsubscribe function.
   *
   * Every event this build's server publishes — `gift_sent`, `purchase`, `kick_points_bought` —
   * means the caller's own wallet moved, so the handler is "refresh my wallet state", not a
   * switch over kinds: the resulting balance is never in the event and always one
   * {@link getBalance} away.
   */
  onEconomyEvent(handler: Listener<EconomyEvent>): () => void {
    return this.#listeners.add(handler);
  }

  /**
   * Reads the caller's own wallet: virtual balance, points, and Kick Points.
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
   *
   * `clientKey` is this gift intent's idempotency key: mint one per intent (e.g. once when the
   * picker opens for a chosen recipient) and send the same key on every retry. A retry with the
   * same key returns the first send — `duplicate` true on the result — instead of charging twice.
   * Without a key the server cannot tell a retry from a fresh intent and charges every attempt.
   */
  async sendGift(
    gift: string,
    recipient: Id,
    conversationId?: Id,
    clientKey?: string,
  ): Promise<GiftSendResult> {
    const request: GiftSend = { gift, recipient };
    if (conversationId !== undefined) {
      request.conversationId = conversationId;
    }
    if (clientKey !== undefined) {
      request.clientKey = clientKey;
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

  /**
   * Buys a catalogue item for the caller's own account.
   *
   * `sku` is a catalogue code (e.g. `"sticker.frog_set"`) from {@link getGiftCatalogue} — the
   * same call lists every category the server sells, not only gifts, because the wire's
   * `category` field already carries which shelf a listing sits on. `clientKey` is the
   * caller's idempotency key: one per purchase intent, so a retry after a network failure
   * returns the first purchase instead of charging twice. `txHash`, when given, claims the
   * purchase was already paid on-chain — the server cannot verify a chain it does not read,
   * so such a purchase is refused with `FEATURE_DISABLED` rather than settled on the claim.
   */
  async purchase(sku: string, clientKey: string, txHash?: string): Promise<StorePurchaseResult> {
    const request: StorePurchase = { sku, clientKey };
    if (txHash !== undefined) {
      request.txHash = txHash;
    }
    return this.#rpc.call(
      OP.STORE_PURCHASE,
      encodeStorePurchase,
      decodeStorePurchaseResult,
      request,
    );
  }

  /**
   * Buys one Kick Point pack, prepaid at a bulk discount.
   *
   * A Kick Point covers the price of an outright group kick (a founder's kick spends one KP
   * before touching the coin balance; a vote is always free). `packKp` is a pack size the node
   * sells — 1 KP for 1 coin, 10 for 9, 50 for 40 — any other size rejects before anything
   * moves. Kick Points are an in-system balance only: no chain, no withdrawal, no transfer.
   *
   * `clientKey` is the buy intent's idempotency key, exactly as in {@link purchase}: mint one
   * per intent and send the same key on every retry. A retry with the same key returns the
   * first buy — `duplicate` true on the result — instead of charging twice.
   */
  async buyKickPoints(packKp: number, clientKey: string): Promise<KickPointsBuyResult> {
    const request: KickPointsBuy = { packKp, clientKey };
    return this.#rpc.call(
      OP.KICK_POINTS_BUY,
      encodeKickPointsBuy,
      decodeKickPointsBuyResult,
      request,
    );
  }

  /**
   * Everything the caller owns, oldest first.
   *
   * The entitlements are the composer's purchased-pack source: a pack whose SKU appears here
   * is a pack the picker shows.
   */
  async getEntitlements(): Promise<Entitlement[]> {
    const request: EntitlementsReq = {};
    const response: EntitlementsResponse = await this.#rpc.call(
      OP.ENTITLEMENTS,
      encodeEntitlementsReq,
      decodeEntitlementsResponse,
      request,
    );
    return response.items;
  }
}
