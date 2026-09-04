/**
 * The store app: shelves, prices, ownership, and the on-chain pay flow.
 *
 * # Boot
 *
 * The session is the web client's (same origin, same IndexedDB). Without one the store says so
 * plainly and links back to sign in — it cannot price *for* an anonymous visitor, because the
 * price check (the entitlements read) is the caller's own.
 *
 * # Prices and ownership
 *
 * The catalogue comes from the server (the price is the server's to change); ownership from
 * `ENTITLEMENTS`; the art from `lib/packs.ts`. A pack the catalogue prices but this client
 * cannot render is skipped rather than sold as a name with nothing inside.
 *
 * # The pay flow
 *
 * Chips choose the currency (AVAX native / USDT / USDC; a placeholder contract disables its
 * chip honestly). Buy → prepare (the chain's own fees/gas/nonce, quoted line by line) → confirm
 * (the exact fields the signature covers) → pay on Fuji → the entitlement is written when — and
 * only when — the chain says CONFIRMED.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { FUJI_TESTNET } from '@migo/sdk';
import type { Entitlement, GiftListing, MigoClient } from '@migo/sdk';

import { SHELVES } from './lib/packs.js';
import type { StorePack } from './lib/packs.js';
import {
  currencyAvailable,
  CURRENCY_META,
  mgoOf,
  payOnChain,
  preparePurchase,
} from './lib/chain-purchase.js';
import type { PayCurrency, PreparedPurchase, PurchaseProgress } from './lib/chain-purchase.js';
import { persistSnapshot, restoreSession } from './lib/session.js';

/** The boot states, in the order they happen. */
type Boot = 'restoring' | 'anonymous' | 'failed' | 'ready';

/** The URI path's shelf segment, `/store/<slug>` — this bundle is what the file server serves at both. */
function shelfSlugOf(pathname: string): string {
  const segments = pathname.replace(/\/+$/, '').split('/').filter(Boolean);
  // `/store` → none; `/store/stickers` → 'stickers'. Anything deeper still names its shelf.
  const storeIndex = segments.indexOf('store');
  const slug = storeIndex >= 0 ? segments[storeIndex + 1] : segments[0];
  return slug ?? '';
}

export function App(): ReactNode {
  const [boot, setBoot] = useState<Boot>('restoring');
  const [client, setClient] = useState<MigoClient | null>(null);
  const [username, setUsername] = useState<string | null>(null);
  const [catalogue, setCatalogue] = useState<Map<string, GiftListing> | null>(null);
  const [entitlements, setEntitlements] = useState<Set<string>>(new Set());
  const [shelfSlug, setShelfSlug] = useState(shelfSlugOf(window.location.pathname));

  // The web client's theme decision is per-tab (it stamps `data-theme` on its own `<html>`), so
  // the store follows the system preference — the same default a first web visit sees.
  const [dark, setDark] = useState(
    typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches,
  );
  useEffect(() => {
    document.documentElement.setAttribute('data-theme', dark ? 'dark' : 'light');
    const query = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = (event: MediaQueryListEvent): void => setDark(event.matches);
    query.addEventListener('change', onChange);
    return () => query.removeEventListener('change', onChange);
  }, [dark]);

  // Resume the web client's session once on mount.
  useEffect(() => {
    let cancelled = false;
    restoreSession()
      .then((restored) => {
        if (cancelled) {
          restored?.client.disconnect().catch(() => {});
          return;
        }
        if (restored === null) {
          setBoot('anonymous');
          return;
        }
        setClient(restored.client);
        setBoot('ready');
      })
      .catch(() => {
        if (!cancelled) {
          setBoot('failed');
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // The display name is for the banner chip; best-effort, the id would do.
  useEffect(() => {
    if (client === null) {
      return;
    }
    let cancelled = false;
    client.profile
      .fetchOne(client.accountId)
      .then((profile) => {
        if (!cancelled && profile?.username) {
          setUsername(profile.username);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client]);

  // Prices and ownership: one read each, on boot. Prices are the server's to change, so a
  // session-scoped read rather than a build-time copy; entitlements refresh after every purchase.
  const readEconomy = useCallback(async (target: MigoClient): Promise<void> => {
    const [listings, owned] = await Promise.all([
      target.economy.getGiftCatalogue(),
      target.economy.getEntitlements(),
    ]);
    setCatalogue(new Map(listings.map((listing) => [listing.sku, listing])));
    setEntitlements(new Set(owned.map((entry: Entitlement) => entry.sku)));
  }, []);

  useEffect(() => {
    if (client === null) {
      return;
    }
    void readEconomy(client).catch(() => {
      setCatalogue(new Map());
    });
  }, [client, readEconomy]);

  // The shelf selection follows the URL so a shared link lands where it pointed. The pathnames
  // are real directories the file server resolves to this bundle, so `history.pushState` is
  // honest bookkeeping, not routing by trickery.
  const selectShelf = useCallback((slug: string): void => {
    setShelfSlug(slug);
    window.history.pushState(null, '', slug === '' ? '/store/' : `/store/${slug}/`);
  }, []);
  useEffect(() => {
    function onPopState(): void {
      setShelfSlug(shelfSlugOf(window.location.pathname));
    }
    window.addEventListener('popstate', onPopState);
    return () => window.removeEventListener('popstate', onPopState);
  }, []);

  const shelf = useMemo(() => {
    return SHELVES.find((candidate) => candidate.slug === shelfSlug) ?? SHELVES[0] ?? null;
  }, [shelfSlug]);

  // The packs on the active shelf, joined with the catalogue's prices and the entitlements' ownership.
  const priced = useMemo(() => {
    if (catalogue === null) {
      return [];
    }
    return (
      shelf?.packs
        .map((pack) => ({
          pack,
          listing: catalogue.get(pack.sku) ?? null,
          owned: entitlements.has(pack.sku),
        }))
        .filter((row) => row.listing !== null) ?? []
    );
  }, [shelf, catalogue, entitlements]);

  const body = useMemo((): ReactNode => {
    if (boot === 'restoring') {
      return <div className="center-state">Opening your session…</div>;
    }
    if (boot === 'anonymous' || boot === 'failed') {
      return (
        <div className="center-state">
          <h2>The store signs in with Migo</h2>
          <p>
            {boot === 'anonymous'
              ? 'Sign in to the Migo app in this browser first, then come back — the store shares that session.'
              : 'Your session could not be resumed. Sign in to the Migo app in this browser, then come back.'}
          </p>
          <p>
            <a href="/">Open Migo</a>
          </p>
        </div>
      );
    }
    if (catalogue === null) {
      return <div className="center-state">Reading the catalogue…</div>;
    }
    if (shelf === null) {
      return <div className="center-state">That shelf does not exist.</div>;
    }
    return (
      <section className="store-section" aria-label={shelf.label}>
        <h1 className="store-heading">{shelf.label}</h1>
        {shelf.packs.length === 0 ? (
          <p className="store-sub">
            This shelf is not stocked yet on this build — the packs live on Emoticon Packs and
            Stickers.
          </p>
        ) : priced.length === 0 ? (
          <p className="store-sub">Nothing on this shelf in the server's catalogue.</p>
        ) : (
          <div className="pack-grid">
            {priced.map(({ pack, listing, owned }) => (
              <PackCard
                key={pack.sku}
                pack={pack}
                coins={listing?.price ?? 0}
                owned={owned}
                client={client}
                onPurchased={() => {
                  if (client !== null) {
                    void readEconomy(client).catch(() => {});
                  }
                }}
              />
            ))}
          </div>
        )}
      </section>
    );
  }, [boot, catalogue, shelf, priced, client, readEconomy]);

  return (
    <div className="store-shell">
      <header className="store-banner">
        <div className="store-logo">
          <span aria-hidden="true">🛍️</span> Migo Store
        </div>
        <div className="store-banner-sub">
          On-chain purchases · <span className="store-chain-chip">{FUJI_TESTNET.name}</span>
        </div>
        <div className="store-banner-meta">
          {username !== null ? (
            <span className="store-account-chip" title={`Signed in as @${username}`}>
              @{username}
            </span>
          ) : null}
        </div>
      </header>

      <nav className="store-nav" aria-label="Store shelves">
        <a
          href="/store/"
          className={shelfSlug === '' ? 'active' : ''}
          onClick={(event) => {
            event.preventDefault();
            selectShelf('');
          }}
        >
          Home
        </a>
        {SHELVES.map((candidate) => (
          <a
            key={candidate.slug}
            href={`/store/${candidate.slug}/`}
            className={shelfSlug === candidate.slug ? 'active' : ''}
            onClick={(event) => {
              event.preventDefault();
              selectShelf(candidate.slug);
            }}
          >
            {candidate.label}
          </a>
        ))}
      </nav>

      {body}
    </div>
  );
}

/** One pack as a card: the art, the price, the currency chips, and the buy button. */
function PackCard({
  pack,
  coins,
  owned,
  client,
  onPurchased,
}: {
  pack: StorePack;
  coins: number;
  owned: boolean;
  client: MigoClient | null;
  onPurchased: () => void;
}): ReactNode {
  const [currency, setCurrency] = useState<PayCurrency>('avax');
  const [sheet, setSheet] = useState(false);

  const preview = pack.items.slice(0, 8);

  return (
    <div className="pack-card">
      <div className="pack-preview" aria-hidden="true">
        {preview.map((item, index) => (
          <span key={index}>{item}</span>
        ))}
      </div>
      <div className="pack-name">{pack.name}</div>
      <div className="pack-meta">
        {owned ? (
          <span className="pack-owned">Owned ✓</span>
        ) : (
          <>
            <span className="pack-price">{coins} coins</span>
            <span>· {mgoOf(BigInt(coins) * 10n ** 18n)} MGO</span>
          </>
        )}
        <span className="currency-note">on {FUJI_TESTNET.name}</span>
      </div>
      {!owned ? (
        <>
          <div className="currency-row" role="group" aria-label="Pay with">
            {(Object.keys(CURRENCY_META) as PayCurrency[]).map((candidate) => {
              const available = currencyAvailable(candidate);
              return (
                <button
                  key={candidate}
                  type="button"
                  className={`currency-chip ${currency === candidate ? 'active' : ''}`}
                  disabled={!available}
                  title={
                    available
                      ? CURRENCY_META[candidate].note
                      : 'No contract address in this build yet'
                  }
                  aria-pressed={currency === candidate}
                  onClick={() => setCurrency(candidate)}
                >
                  {CURRENCY_META[candidate].label}
                </button>
              );
            })}
          </div>
          <button
            type="button"
            className="btn btn-primary"
            disabled={client === null || !currencyAvailable(currency)}
            onClick={() => setSheet(true)}
          >
            Buy on-chain
          </button>
        </>
      ) : null}

      {sheet && client !== null ? (
        <BuySheet
          pack={pack}
          coins={coins}
          currency={currency}
          client={client}
          onClose={() => setSheet(false)}
          onPurchased={() => {
            setSheet(false);
            onPurchased();
          }}
        />
      ) : null}
    </div>
  );
}

/** The steps the sheet narrates, in order, with the one the payment is on marked busy. */
const STEPS: ReadonlyArray<{ id: string; label: string }> = [
  { id: 'preparing', label: 'Building the transaction from the chain' },
  { id: 'signed', label: 'Signed with wallet 0' },
  { id: 'broadcast', label: 'Broadcast to Fuji' },
  { id: 'pending', label: 'Waiting for the chain' },
  { id: 'settled', label: 'Payment confirmed' },
  { id: 'entitled', label: 'Entitlement written' },
];

/** The buy flow: prepare, quote, pay, track, done. */
function BuySheet({
  pack,
  coins,
  currency,
  client,
  onClose,
  onPurchased,
}: {
  pack: StorePack;
  coins: number;
  currency: PayCurrency;
  client: MigoClient;
  onClose: () => void;
  onPurchased: () => void;
}): ReactNode {
  const [prepared, setPrepared] = useState<PreparedPurchase | null>(null);
  const [progress, setProgress] = useState<PurchaseProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Prepare as soon as the sheet opens: the confirm screen is what the user lands on, not a wait.
  useEffect(() => {
    let cancelled = false;
    setProgress({ step: 'preparing', txHash: null, outcome: null });
    preparePurchase({ client, sku: pack.sku, name: pack.name, coins, currency })
      .then((next) => {
        if (!cancelled) {
          setPrepared(next);
          setProgress(null);
        }
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setError(cause instanceof Error ? cause.message : 'The transaction could not be built.');
          setProgress(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client, pack, coins, currency]);

  const pay = useCallback((): void => {
    if (prepared === null || busy) {
      return;
    }
    setBusy(true);
    setError(null);
    payOnChain(prepared, client, setProgress)
      .then((result) => {
        // The tracked purchase is on the sealed snapshot now; persist so the web client's
        // Activity list sees it.
        return persistSnapshot(client).then(() => {
          setDone(result.txHash);
          onPurchased();
        });
      })
      .catch((cause: unknown) => {
        setError(cause instanceof Error ? cause.message : 'The payment failed.');
      })
      .finally(() => {
        setBusy(false);
      });
  }, [prepared, busy, client, onPurchased]);

  const activeIndex = progress !== null ? STEPS.findIndex((step) => step.id === progress.step) : -1;

  return (
    <div className="sheet-backdrop" role="presentation" onClick={busy ? undefined : onClose}>
      <div
        className="sheet"
        role="dialog"
        aria-label={`Buy ${pack.name}`}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="sheet-head">
          <h2 className="sheet-title">Buy {pack.name}</h2>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            disabled={busy}
            aria-label="Close"
          >
            ✕
          </button>
        </div>

        {done !== null ? (
          <>
            <p className="success-line">Purchased — the pack is in your composer now.</p>
            <p className="tx-link" title="The on-chain transaction">
              tx {done}
            </p>
            <div className="form-actions">
              <button type="button" className="btn btn-primary" onClick={onClose}>
                Done
              </button>
            </div>
          </>
        ) : prepared !== null ? (
          <>
            <div>
              <div className="quoted-line">
                <span className="quoted-label">Pack</span>
                <span className="quoted-value">{prepared.name}</span>
              </div>
              <div className="quoted-line">
                <span className="quoted-label">Price</span>
                <span className="quoted-value">
                  {mgoOf(prepared.mgoUnits)} MGO ({coins} coins)
                </span>
              </div>
              <div className="quoted-line">
                <span className="quoted-label">Pay with</span>
                <span className="quoted-value">{CURRENCY_META[prepared.currency].label}</span>
              </div>
              <div className="quoted-line">
                <span className="quoted-label">From</span>
                <span className="quoted-value">{prepared.from}</span>
              </div>
              {prepared.currency === 'avax' ? (
                <div className="quoted-line">
                  <span className="quoted-label">To (treasury)</span>
                  <span className="quoted-value">{prepared.to}</span>
                </div>
              ) : (
                <>
                  <div className="quoted-line">
                    <span className="quoted-label">Token</span>
                    <span className="quoted-value">{prepared.to}</span>
                  </div>
                  <div className="quoted-line">
                    <span className="quoted-label">To (treasury)</span>
                    <span className="quoted-value">{prepared.treasury}</span>
                  </div>
                </>
              )}
              <div className="quoted-line">
                <span className="quoted-label">Max fee</span>
                <span className="quoted-value">
                  {(prepared.maxFeePerGas * BigInt(prepared.gasLimit)).toString()} wei
                </span>
              </div>
              <div className="quoted-line">
                <span className="quoted-label">Gas limit</span>
                <span className="quoted-value">{prepared.gasLimit}</span>
              </div>
              <div className="quoted-line">
                <span className="quoted-label">Nonce</span>
                <span className="quoted-value">{prepared.nonce}</span>
              </div>
              <div className="quoted-line">
                <span className="quoted-label">Chain</span>
                <span className="quoted-value">{FUJI_TESTNET.name}</span>
              </div>
            </div>
            {error !== null ? <p className="form-error">{error}</p> : null}
            <div className="form-actions">
              <button type="button" className="btn" onClick={onClose} disabled={busy}>
                Cancel
              </button>
              <button type="button" className="btn btn-primary" disabled={busy} onClick={pay}>
                {busy ? 'Paying…' : 'Confirm payment'}
              </button>
            </div>
          </>
        ) : (
          <>
            {error !== null ? <p className="form-error">{error}</p> : null}
            <div className="progress-line">
              <span className="progress-dot busy" aria-hidden="true" />
              <span>Building the transaction from the chain's own answers…</span>
            </div>
          </>
        )}

        {busy || progress !== null ? (
          <ol
            style={{
              margin: 0,
              paddingLeft: 0,
              listStyle: 'none',
              display: 'flex',
              flexDirection: 'column',
              gap: 4,
            }}
          >
            {STEPS.map((step, index) => {
              const state =
                done !== null || (activeIndex >= 0 && index < activeIndex)
                  ? 'done'
                  : index === activeIndex
                    ? 'busy'
                    : 'todo';
              return (
                <li key={step.id} className="progress-line">
                  <span
                    className={`progress-dot ${state === 'todo' ? '' : state}`}
                    aria-hidden="true"
                  />
                  <span>{step.label}</span>
                </li>
              );
            })}
          </ol>
        ) : null}
      </div>
    </div>
  );
}
