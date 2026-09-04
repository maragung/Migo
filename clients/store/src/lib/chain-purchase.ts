/**
 * The on-chain purchase: priced in MGO, paid in AVAX/USDT/BTC.b, settled on Avalanche Fuji.
 *
 * # What "on-chain" means here
 *
 * The server's catalogue prices every pack in Migo coins (a virtual, non-monetary number),
 * and one coin is one MGO — that is the price the store *speaks*. The user's instruction
 * makes the *payment* on-chain and in a real currency: the buyer pays AVAX, USDT, or BTC.b,
 * and the amount is the MGO price converted through a live USD pair. The MGO side of the
 * pair is a policy constant (`MGO_USD`); the currency side is read live from a public spot
 * price (Coinbase's spot endpoint, CORS-friendly, no key) with a short cache so a shelf of
 * cards does not become a shelf of requests. USDT is quoted at its $1 peg — the user's own
 * example (150 MGO = 0.15 USDT) is the peg speaking.
 *
 * The flow, in order, with each step visible to the caller:
 *
 *   1. `preparePurchase` — the live rate, then the chain's own answers (fees, gas, nonce)
 *      and the exact transaction payload, before anything is signed.
 *   2. `payOnChain` — sign with wallet 0 of the account root (`EvmWallet.fromRoot`), broadcast
 *      through `ChainClient` (the Migo server is never a blockchain proxy and never sees the
 *      transaction), track to an honest ending.
 *   3. `client.economy.purchase(sku, clientKey, txHash)` — the server writes the entitlement and
 *      the ledger legs, with the tx hash riding along for audit.
 *
 * Paying with native AVAX sends `value` to the MGO treasury address; paying with USDT/BTC.b
 * sends an ERC-20 `transfer(address,uint256)` in the calldata, in the *token's* smallest
 * units (BTC.b carries 8 decimals where AVAX carries 18 — the conversion, not the MGO
 * amount, is what the chain receives). Either way the confirm screen quotes every field the
 * signature covers — the same rule the wallet's AVAX send keeps.
 *
 * # Idempotency
 *
 * `clientKey` is minted once per purchase intent; a retry after a network failure re-sends
 * the same key and the server returns the first purchase instead of charging twice. It is
 * derived from the SKU and the on-chain tx hash when one exists (a settled on-chain payment
 * followed by a failed RPC is the same purchase, retried) — so the same payment can never
 * buy twice.
 */

import { account, ChainClient, FUJI_TESTNET } from '@migo/sdk';
import type { MigoClient, TrackedOutcome } from '@migo/sdk';

import { hexOf } from './hex.js';

/**
 * The MGO token's ERC-20 contract on Fuji.
 *
 * **Deploy it and change this line** — until then the placeholder is the zero address, which
 * the prepare step refuses to build a payment for, so the UI can offer AVAX honestly rather
 * than a token transfer to nowhere. Address, not name: the chain knows it by nothing else.
 */
export const MGO_TOKEN_FUJI = '0x0000000000000000000000000000000000000000';

/** The treasury that receives the payment: the deployment's own wallet. Deploy and change this line. */
export const MGO_TREASURY_FUJI = '0x0000000000000000000000000000000000000000';

/**
 * USDT on Fuji. Testnet tokens are redeployed at the owner's whim; this build refuses a
 * USDT/BTC.b payment while the address is the placeholder rather than sending someone's
 * tokens to address zero.
 */
export const USDT_FUJI = '0x0000000000000000000000000000000000000000';

/**
 * BTC.b on Fuji — bridged native BTC, **8 decimals**, not ERC-20's 18. The payment path
 * carries the token's own unit; the conversion, not the MGO amount, is what moves.
 */
export const BTCB_FUJI = '0x0000000000000000000000000000000000000000';

/** MGO's decimals: the ERC-20 standard's own unit for the amount the chain receives. */
export const MGO_DECIMALS = 18n;

/** One MGO in its smallest unit, the way `10n ** 18n` is one AVAX in wei. */
const MGO_UNIT = 10n ** MGO_DECIMALS;

/**
 * The coin→MGO conversion: one coin prices one MGO.
 *
 * A fixed, whole-number rate keeps every confirm screen exact — no rounding at the moment of
 * payment, and the server's coin price and the chain's MGO amount never disagree about magnitude.
 */
const COINS_PER_MGO = 1n;

/**
 * What one MGO is worth in USD — the MGO side of every conversion pair.
 *
 * A policy constant, not a market read: MGO is this deployment's own unit, and the store's
 * promise is that the number is stable (0.001 USD is the rate behind the store's own example:
 * a 150-MGO pack prices at 0.15 USDT). Change it here and every pair follows.
 */
export const MGO_USD = 0.001;

/** The payment currencies the store accepts, as the chips name them. */
export type PayCurrency = 'avax' | 'usdt' | 'btcb';

/** The label, small print, and on-chain unit each currency carries on its chip. */
export const CURRENCY_META: Readonly<
  Record<PayCurrency, { label: string; note: string; decimals: number }>
> = {
  avax: { label: 'AVAX', note: 'native, pays as a direct transfer', decimals: 18 },
  usdt: { label: 'USDT', note: 'ERC-20 transfer', decimals: 18 },
  btcb: { label: 'BTC.b', note: 'bridged BTC, 8 decimals', decimals: 8 },
};

/** The ERC-20 contract a token payment targets (null for native AVAX). */
export function tokenOf(currency: PayCurrency): string | null {
  if (currency === 'avax') {
    return null;
  }
  return currency === 'usdt' ? USDT_FUJI : BTCB_FUJI;
}

/** Whether a currency is actually payable in this build: a placeholder contract is not. */
export function currencyAvailable(currency: PayCurrency): boolean {
  if (currency === 'avax') {
    return MGO_TREASURY_FUJI !== '0x0000000000000000000000000000000000000000';
  }
  const contract = tokenOf(currency);
  return contract !== null && contract !== '0x0000000000000000000000000000000000000000';
}

/**
 * How long a live rate is believed. A shelf of cards shares one read per currency; a
 * checkout re-reads rather than trusting a rate older than this.
 */
const RATE_TTL_MS = 60_000;

/** The live rates this module has read, keyed by currency. */
const rateCache = new Map<PayCurrency, { usd: number; at: number }>();

/**
 * One currency's live USD price.
 *
 * AVAX and BTC.b are read from Coinbase's public spot endpoint (CORS-friendly, no key);
 * USDT is quoted at its $1 peg, which is the number the store's own example speaks. Throws
 * on a failed read — a rate the store cannot verify is a price it must not quote.
 */
export async function fetchUsdPrice(currency: PayCurrency): Promise<number> {
  if (currency === 'usdt') {
    return 1;
  }
  const cached = rateCache.get(currency);
  if (cached !== undefined && Date.now() - cached.at < RATE_TTL_MS) {
    return cached.usd;
  }
  const pair = currency === 'avax' ? 'AVAX-USD' : 'BTC-USD';
  const response = await fetch(`https://api.coinbase.com/v2/prices/${pair}/spot`);
  if (!response.ok) {
    throw new Error(`The live ${pair} rate could not be read (${response.status}).`);
  }
  const body = (await response.json()) as { data?: { amount?: string } };
  const usd = Number(body.data?.amount);
  if (!Number.isFinite(usd) || usd <= 0) {
    throw new Error(`The live ${pair} rate came back unusable.`);
  }
  rateCache.set(currency, { usd, at: Date.now() });
  return usd;
}

/** The coin price of an item as the MGO amount the price speaks, smallest units. */
export function mgoAmountFor(coins: number): bigint {
  const mgo = BigInt(Math.max(0, Math.trunc(coins))) * COINS_PER_MGO;
  return mgo * MGO_UNIT;
}

/**
 * What a pack costs in the chosen currency, that currency's own smallest units.
 *
 * The conversion runs through USD at full precision and rounds *up* at the last unit: the
 * treasury must never receive less than the MGO value the confirm screen quoted, and a
 * fraction of a wei cannot be owed.
 */
export function paymentUnitsFor(coins: number, currency: PayCurrency, usdPrice: number): bigint {
  // value and price both scaled by 1e12 so the division is integer math; ceil, never floor.
  const valueScaled =
    BigInt(Math.max(0, Math.trunc(coins))) * COINS_PER_MGO * BigInt(Math.round(MGO_USD * 1e12));
  const priceScaled = BigInt(Math.round(usdPrice * 1e12));
  const scale = 10n ** BigInt(CURRENCY_META[currency].decimals);
  if (priceScaled <= 0n) {
    throw new Error('That currency has no usable rate right now.');
  }
  return (valueScaled * scale + priceScaled - 1n) / priceScaled;
}

/** A smallest-unit amount as a decimal string, trailing zeros trimmed, for any decimals. */
export function unitsOf(units: bigint, decimals: number): string {
  const unit = 10n ** BigInt(decimals);
  const whole = units / unit;
  let fraction = (units % unit).toString(10);
  if (fraction.match(/^0*$/)) {
    return whole.toString(10);
  }
  while (fraction.length < decimals) {
    fraction = `0${fraction}`;
  }
  return `${whole}.${fraction.replace(/0+$/, '')}`;
}

/** An MGO amount as a decimal string — `unitsOf` at the ERC-20 unit. */
export function mgoOf(units: bigint): string {
  return unitsOf(units, Number(MGO_DECIMALS));
}

/** What a prepared purchase quotes on the confirm screen: every field the signature covers. */
export interface PreparedPurchase {
  /** The pack's catalogue code. */
  sku: string;
  /** The pack's display name. */
  name: string;
  /** What is being paid with. */
  currency: PayCurrency;
  /** Wallet 0's checksummed address — the sender the signature will commit. */
  from: string;
  /** The recipient: the treasury for AVAX, the token contract for USDT/BTC.b. */
  to: string;
  /** The MGO amount the price speaks, smallest units — the number the user is agreeing to. */
  mgoUnits: bigint;
  /** What the chain actually receives: the converted amount, the currency's own smallest units. */
  payUnits: bigint;
  /** The live USD price the conversion used, restated on the confirm screen. */
  usdPrice: number;
  /** For a token payment, the token contract the transaction calls. */
  tokenContract: string | null;
  /** For a token payment, the treasury the calldata's transfer names. */
  treasury: string | null;
  /** EIP-1559 fee ceilings, wei per gas. */
  maxPriorityFeePerGas: bigint;
  maxFeePerGas: bigint;
  /** The gas limit, from the node's own estimate. */
  gasLimit: number;
  /** The account's next nonce, `pending`-aware. */
  nonce: number;
  /** Fuji's chain id, restated for the line the confirm screen always shows. */
  chainId: number;
}

/** One step of the payment's progress, as the UI narrates it. */
export type PurchaseStep =
  'preparing' | 'awaiting' | 'signed' | 'broadcast' | 'pending' | 'settled' | 'entitled';

/** Where the payment stands, once anything has left the page. */
export interface PurchaseProgress {
  step: PurchaseStep;
  /** The on-chain hash once one exists — the handle the explorer and the server both know. */
  txHash: string | null;
  /** The tracker's honest ending, once reached. */
  outcome: TrackedOutcome | null;
}

/**
 * Builds the transaction from the live rate and the chain's own answers.
 *
 * Parse failures and placeholder contracts refuse *here*, before a single RPC leaves — a confirm
 * screen must never quote a field this function could not fill from the network. The root check
 * is the same one the AVAX wallet makes: a device without the root has no wallet, and paying
 * needs one.
 *
 * @param usdPrice The live rate to convert through; when omitted, one is read here so the
 *   quote is always built from a number this function can name.
 * @throws {Error} when the currency's contract (or the treasury) is the placeholder, the
 *   device holds no account root, or the live rate cannot be read.
 */
export async function preparePurchase(input: {
  client: MigoClient;
  sku: string;
  name: string;
  coins: number;
  currency: PayCurrency;
  usdPrice?: number;
}): Promise<PreparedPurchase> {
  const root = input.client.keyStore.root();
  if (root === null) {
    throw new Error(
      'This device does not hold the account root, so it has no wallet to pay from; open the store on the device that holds the account backup.',
    );
  }
  if (!currencyAvailable(input.currency)) {
    throw new Error(
      'That currency has no contract address in this build yet; pay with one of the chips that is enabled.',
    );
  }
  const usdPrice = input.usdPrice ?? (await fetchUsdPrice(input.currency));
  const wallet = account.EvmWallet.fromRoot(root, 0);
  const chain = new ChainClient({ network: FUJI_TESTNET });
  const mgoUnits = mgoAmountFor(input.coins);
  const payUnits = paymentUnitsFor(input.coins, input.currency, usdPrice);
  const isNative = input.currency === 'avax';
  const to = isNative
    ? account.parseAddress(MGO_TREASURY_FUJI)
    : account.parseAddress(tokenOf(input.currency) ?? MGO_TREASURY_FUJI);
  // ERC-20 `transfer(address,uint256)`: the 4-byte selector, a treasury address, a padded amount.
  // For a token payment the transaction's `to` is the *contract*; the treasury rides in the calldata.
  const recipient = isNative ? to : account.parseAddress(MGO_TREASURY_FUJI);
  const data = isNative ? new Uint8Array(0) : erc20Transfer(recipient, payUnits);
  const [fees, gasLimit, nonce] = await Promise.all([
    chain.getFees(),
    chain.estimateGas({
      from: wallet.address(),
      to,
      value: isNative ? payUnits : 0n,
      data,
    }),
    chain.getNonce(wallet.address()),
  ]);
  return {
    sku: input.sku,
    name: input.name,
    currency: input.currency,
    from: wallet.addressChecksummed(),
    to: account.eip55(to),
    mgoUnits,
    payUnits,
    usdPrice,
    tokenContract: isNative ? null : account.eip55(to),
    treasury: isNative ? null : account.eip55(recipient),
    maxPriorityFeePerGas: fees.maxPriorityFeePerGas,
    maxFeePerGas: fees.maxFeePerGas,
    gasLimit,
    nonce,
    chainId: FUJI_TESTNET.chainId,
  };
}

/**
 * Signs, broadcasts, tracks, and then tells the server.
 *
 * The signature covers exactly the fields `preparePurchase` returned — nothing is re-derived
 * here except the wallet (whose address the prepared `from` is checked against, the same guard
 * the AVAX send keeps). The tracked transaction is recorded on the key store's sealed record and
 * the caller persists the snapshot so the web client's Activity list sees it.
 *
 * Only an on-chain `CONFIRMED` calls `purchase`; a `REVERTED`/`DROPPED`/`EXPIRED` ending throws
 * with the outcome named, because a payment that did not settle must not mint an entitlement.
 */
export async function payOnChain(
  prepared: PreparedPurchase,
  client: MigoClient,
  onProgress: (progress: PurchaseProgress) => void,
): Promise<{ txHash: string; duplicate: boolean }> {
  const root = client.keyStore.root();
  if (root === null) {
    throw new Error('The account root left this device; re-open the store and prepare again.');
  }
  const wallet = account.EvmWallet.fromRoot(root, 0);
  if (prepared.from !== wallet.addressChecksummed()) {
    throw new Error('The prepared purchase names a different sender; prepare it again here.');
  }
  const isNative = prepared.currency === 'avax';
  const to = account.parseAddress(prepared.to.trim());
  // For a token payment the prepared `to` is the contract and the treasury rides in the calldata;
  // the amount is the converted one, in the token's own smallest units.
  const recipient = isNative ? to : account.parseAddress(MGO_TREASURY_FUJI);
  const body = new account.Eip1559Tx({
    chainId: FUJI_TESTNET.chainId,
    nonce: prepared.nonce,
    maxPriorityFeePerGas: prepared.maxPriorityFeePerGas,
    maxFeePerGas: prepared.maxFeePerGas,
    gasLimit: prepared.gasLimit,
    to,
    value: isNative ? prepared.payUnits : 0n,
    data: isNative ? new Uint8Array(0) : erc20Transfer(recipient, prepared.payUnits),
  });
  onProgress({ step: 'signed', txHash: null, outcome: null });
  const signed = body.sign(wallet);
  const chain = new ChainClient({ network: FUJI_TESTNET });
  const txHash = await chain.broadcast(signed);
  onProgress({ step: 'broadcast', txHash, outcome: null });

  // The record is written at broadcast, not at settle: a reload mid-tracking loses the ending,
  // never the fact that value left.
  client.keyStore.trackedTxs().unshift({
    txHash: signed.txHash(),
    chainId: FUJI_TESTNET.chainId,
    to,
    valueWei: isNative ? prepared.payUnits : 0n,
    feeWei: prepared.maxFeePerGas * BigInt(prepared.gasLimit),
    gasLimit: prepared.gasLimit,
    atUnix: Math.floor(Date.now() / 1000),
    outcome: 'PENDING',
  });

  let outcome: TrackedOutcome;
  try {
    const result = await chain.track(txHash, {
      onState: (state) => {
        const step: PurchaseStep | null =
          state === 'PENDING' ? 'pending' : state === 'CONFIRMED' ? 'settled' : null;
        if (step !== null) {
          onProgress({ step, txHash, outcome: null });
        }
      },
    });
    outcome = result.outcome;
  } catch {
    outcome = 'EXPIRED';
  }
  settle(client, txHash, outcome);
  onProgress({ step: 'settled', txHash, outcome });
  if (outcome !== 'CONFIRMED') {
    throw new Error(
      `The payment did not settle on-chain (${outcome.toLowerCase()}); nothing was charged to your Migo account.`,
    );
  }

  // The payment settled; the entitlement is the server's to write, keyed by the tx hash so a
  // retry of this exact step is the same purchase, never a second one.
  onProgress({ step: 'entitled', txHash, outcome });
  const key = purchaseKey(prepared.sku, txHash);
  const result = await client.economy.purchase(prepared.sku, key, txHash);
  return { txHash, duplicate: result.duplicate };
}

/** The idempotency key for a purchase: the SKU and the payment that paid for it. */
export function purchaseKey(sku: string, txHash: string): string {
  return `${sku}:${txHash}`;
}

/** Writes a tracker's ending into the record the wallet's Activity list will next read. */
function settle(client: MigoClient, txHash: string, outcome: TrackedOutcome): void {
  const records = client.keyStore.trackedTxs();
  const index = records.findIndex((record) => `0x${hexOf(record.txHash)}` === txHash);
  const record = index >= 0 ? records[index] : undefined;
  if (record !== undefined && index >= 0) {
    records[index] = { ...record, outcome };
  }
}

/** The ERC-20 `transfer(address,uint256)` calldata: selector, padded address, padded amount. */
function erc20Transfer(recipient: Uint8Array, amount: bigint): Uint8Array {
  const selector = Uint8Array.of(0xa9, 0x05, 0x9c, 0xbb);
  const out = new Uint8Array(4 + 32 + 32);
  out.set(selector, 0);
  out.set(recipient.subarray(0, 20), 4 + 12);
  const hex = amount.toString(16).padStart(64, '0');
  for (let index = 0; index < 32; index += 1) {
    out[4 + 32 + index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  }
  return out;
}
