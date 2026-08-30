'use client';

/**
 * The Wallet section: the MIG balance, the gift shop, the statement, XP standing, badges, and
 * the leaderboard — the caller's whole virtual economy under one address.
 *
 * Everything on this panel is the caller's own economy — balance, ledger, progression, badges —
 * or a global catalogue, so every fetch is a plain read on mount plus a refresh after the one
 * mutation ({@link sendGift} moves the balance and appends a ledger line; the panel re-reads
 * both rather than patching local state, because the server's arithmetic is the only arithmetic
 * worth showing).
 *
 * The coin is $MIG — the same ticker message text highlights as a token reference — so the
 * balance card leads with the mark and the statement's lines are plain facts about it. The
 * send flow is a picker, not a prompt: a gift is addressed to a friend from the relationship
 * graph, or to anyone found by username search, and the panel states the price before the
 * recipient is chosen so the spend is never a surprise. Feedback is a single line — success
 * names the gift and the recipient, failure carries the server's reason through {@link
 * friendlyError} — and the picker closes either way.
 *
 * The presentational pieces are exported pure components over plain data, so the panel's rules
 * (the XP bar's clamping, the ledger line's signed amounts, the disabled states) are testable
 * without a live client, exactly like the other panels' extracted logic.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { RelationshipKind } from '@migo/sdk';
import type {
  BadgeWire,
  GiftListing,
  Id,
  LedgerEntryWire,
  ProgressionWire,
  RankWire,
  RelationshipEntry,
  SuggestedUser,
  UserProfile,
  WalletView,
} from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { CoinMark } from './icons.js';
import { Spinner } from './spinner.js';

/**
 * The relationship kinds this panel files people under, as the plain numbers the wire carries.
 *
 * `RelationshipEntry.kind` is a `number` (a newer server may send a value this build has no name
 * for), and comparing a number against an enum member directly trips the workspace's
 * unsafe-enum-comparison rule — so the enum's numeric value is read into a number-typed constant
 * once, and the filter compares number to number.
 */
const KIND_FRIEND: number = RelationshipKind.Friend;

/** How many statement lines the panel shows; the ledger endpoint holds the rest. */
const LEDGER_ROWS = 10;

/** How deep the XP leaderboard's top list reads. */
const LEADERBOARD_ROWS = 10;

/**
 * The XP bar's filled fraction: `into` of `total`, clamped into 0–1.
 *
 * A total of zero (or a negative a hostile server sent) renders an empty bar rather than `NaN`%
 * or `Infinity`% width — an unfilled bar is honest, a broken stylesheet is not.
 */
export function xpFraction(into: number, total: number): number {
  if (total <= 0) {
    return 0;
  }
  return Math.min(1, Math.max(0, into / total));
}

/** The wallet as two plain facts: the $MIG coin balance and the points balance. */
export function BalanceCard({ balance }: { balance: WalletView }): ReactNode {
  return (
    <div className="balance-card">
      <span className="balance-fact balance-fact-coins">
        <CoinMark size={16} />
        <span className="balance-amount">{balance.balance.toLocaleString()}</span>
        <span className="balance-unit">$MIG</span>
      </span>
      <span className="balance-fact balance-fact-points">
        <span className="balance-amount">{balance.points.toLocaleString()}</span>
        <span className="balance-unit">points</span>
      </span>
    </div>
  );
}

/**
 * The badge shelf: the honours the server has awarded, one chip per badge.
 *
 * A badge is a code and a date and nothing else on the wire, so the chip states exactly that —
 * the code in humanised words, the date relative — and an empty shelf is stated honestly rather
 * than hidden, because a hidden section reads as a broken one.
 */
export function BadgeShelf({ badges }: { badges: BadgeWire[] }): ReactNode {
  if (badges.length === 0) {
    return <p className="muted">No badges yet — earn XP to collect them.</p>;
  }
  return (
    <ul className="badge-shelf" aria-label="Badges">
      {badges.map((badge) => (
        <li
          key={badge.badgeCode}
          className="badge-chip"
          title={`Awarded ${formatRelative(badge.awardedAt)}`}
        >
          {badgeCodeLabel(badge.badgeCode)}
        </li>
      ))}
    </ul>
  );
}

/** The wire's snake_case badge code as readable words (`welcome_user` → `Welcome user`). */
function badgeCodeLabel(code: string): string {
  const spaced = code.replaceAll('_', ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/**
 * The XP standing: level, a filled bar, and the numbers behind it.
 *
 * The bar is an ARIA progressbar whose min/max are the real XP bounds, so assistive technology
 * reads the same fraction sighted users see — the visible `into / total` label is the
 * confirmation, not the only channel.
 */
export function ProgressionCard({ progression }: { progression: ProgressionWire }): ReactNode {
  const { xpIntoLevel, xpForNextLevel } = progression;
  const percent = Math.round(xpFraction(xpIntoLevel, xpForNextLevel) * 100);
  return (
    <div className="progression-card">
      <div className="progression-head">Level {progression.level}</div>
      <div
        className="progress-bar"
        role="progressbar"
        aria-label={`XP into level ${progression.level}`}
        aria-valuenow={xpIntoLevel}
        aria-valuemin={0}
        aria-valuemax={xpForNextLevel}
      >
        <div className="progress-fill" style={{ width: `${percent}%` }} />
      </div>
      <div className="progression-label">
        {xpIntoLevel} / {xpForNextLevel} XP
      </div>
    </div>
  );
}

/** One gift in the shop grid: what it is, what it costs, and how to send it. */
export function GiftCard({
  gift,
  onSend,
  disabled,
}: {
  gift: GiftListing;
  onSend: (gift: GiftListing) => void;
  disabled: boolean;
}): ReactNode {
  return (
    <div className="gift-card">
      <div className="gift-name">{gift.name}</div>
      <div className="gift-category">{gift.category}</div>
      <div className="gift-price">{gift.price} coins</div>
      <button
        type="button"
        className="btn btn-primary"
        disabled={disabled}
        onClick={() => onSend(gift)}
      >
        Send
      </button>
    </div>
  );
}

/** The shop: every catalogue entry as a card, in a responsive grid. */
export function GiftGrid({
  gifts,
  onSend,
  disabled,
}: {
  gifts: GiftListing[];
  onSend: (gift: GiftListing) => void;
  disabled: boolean;
}): ReactNode {
  return (
    <div className="gift-grid">
      {gifts.map((gift) => (
        <GiftCard key={gift.sku} gift={gift} onSend={onSend} disabled={disabled} />
      ))}
    </div>
  );
}

/**
 * The signed amount a ledger line shows, e.g. `−10` for a gift bought.
 *
 * The wire's amount is a magnitude; the *reason* names the direction. This is the closed
 * mapping of the reasons this build's server writes: the spends subtract, the receipts add, and
 * an operator adjustment or an unknown word from a newer node renders unsigned rather than
 * guessing a direction for money.
 */
export function ledgerAmountLabel(entry: LedgerEntryWire): string {
  const credit =
    entry.reason === 'grant' ||
    entry.reason === 'gift_reputation' ||
    entry.reason === 'refund' ||
    entry.reason === 'game_payout';
  const debit =
    entry.reason === 'gift_purchase' ||
    entry.reason === 'purchase' ||
    entry.reason === 'game_stake';
  if (credit) {
    return `+${entry.amount}`;
  }
  if (debit) {
    return `−${entry.amount}`;
  }
  return String(entry.amount);
}

/** The recent statement: one line per transaction, newest first as the server ordered them. */
export function LedgerList({ entries }: { entries: LedgerEntryWire[] }): ReactNode {
  if (entries.length === 0) {
    return <p className="muted">No transactions yet.</p>;
  }
  return (
    <ul className="ledger-list">
      {entries.map((entry) => (
        <li key={entry.txId} className="ledger-row">
          <span className="ledger-reason">{entry.reason}</span>
          <span className="ledger-amount">{ledgerAmountLabel(entry)}</span>
          <span className="ledger-after">balance {entry.balanceAfter}</span>
          <span className="ledger-at">{formatRelative(entry.at)}</span>
        </li>
      ))}
    </ul>
  );
}

/**
 * The XP leaderboard's ranked list: position, avatar, name, level, and XP per row.
 *
 * Names and avatars resolve through the caller's profile map (the panel passes the shared cache's),
 * so a leaderboard row and a friend row show the same person the same way. An unresolved account
 * keeps a stable fallback rather than a blank line — the rank is the fact, the name is its label.
 */
export function LeaderboardList({
  ranks,
  profiles,
}: {
  ranks: RankWire[];
  /** Resolved profiles keyed by account id; entries may be missing, and the row degrades. */
  profiles: ReadonlyMap<Id, UserProfile & { avatarUrl?: string }>;
}): ReactNode {
  if (ranks.length === 0) {
    return <p className="muted">No one has earned XP yet.</p>;
  }
  return (
    <ol className="leaderboard-list" aria-label="XP leaderboard">
      {ranks.map((rank) => {
        const profile = profiles.get(rank.accountId);
        return (
          <li key={rank.accountId} className="leaderboard-row">
            <span className="leaderboard-position">#{rank.position}</span>
            <Avatar
              name={profile?.displayName ?? 'Someone'}
              id={rank.accountId}
              size={28}
              avatarUrl={profile?.avatarUrl}
            />
            <span className="person-name">{profile?.displayName ?? 'Someone'}</span>
            <span className="leaderboard-level">Level {rank.level}</span>
            <span className="leaderboard-xp">{rank.xp} XP</span>
          </li>
        );
      })}
    </ol>
  );
}

/** One candidate recipient: avatar, name, @username, and the Pick action. */
function RecipientRow({
  id,
  name,
  username,
  busy,
  onPick,
}: {
  id: Id;
  name: string;
  username?: string;
  busy: boolean;
  onPick: (id: Id) => void;
}): ReactNode {
  return (
    <div className="person-row">
      <Avatar name={name} id={id} size={36} />
      <div className="person-main">
        <span className="person-name">{name}</span>
        {username ? <span className="person-sub">@{username}</span> : null}
      </div>
      <div className="person-actions">
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy}
          onClick={() => onPick(id)}
        >
          Send
        </button>
      </div>
    </div>
  );
}

/**
 * The recipient picker: the friends list first, username search for anyone else.
 *
 * Search is submit-driven, not per keystroke — every character would be a server round trip
 * against a rate-limited endpoint, and the user who typed three letters has not yet asked a
 * question. It is for non-friends deliberately: the friends are already on screen, and a second
 * copy of them under a search heading would be the same list wearing a different title.
 */
export function RecipientPicker({
  gift,
  friends,
  results,
  profiles,
  onSearch,
  onPick,
  onCancel,
  busy,
  error,
}: {
  gift: GiftListing;
  friends: RelationshipEntry[];
  results: SuggestedUser[] | null;
  profiles: ReadonlyMap<Id, UserProfile>;
  /** Runs a username search with the submitted query. */
  onSearch: (query: string) => void;
  onPick: (recipient: Id) => void;
  onCancel: () => void;
  busy: boolean;
  /** The send flow's own failure line, so the picker's feedback is beside its controls. */
  error?: string | null;
}): ReactNode {
  const [query, setQuery] = useState('');
  return (
    <div className="recipient-picker" role="dialog" aria-label={`Send ${gift.name}`}>
      <div className="panel-head">
        <h2 className="panel-heading">Send {gift.name}</h2>
        <span className="gift-price">{gift.price} coins</span>
      </div>
      {error != null ? <p className="form-error">{error}</p> : null}
      <form
        className="panel-search"
        role="search"
        onSubmit={(event: FormEvent<HTMLFormElement>) => {
          event.preventDefault();
          onSearch(query.trim());
        }}
      >
        <input
          type="search"
          className="input"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search by username"
          aria-label="Search people by username"
        />
        <button type="submit" className="btn">
          Search
        </button>
      </form>
      <div className="panel-section">
        {friends.length === 0 ? (
          <p className="muted">No friends yet — search for anyone by username.</p>
        ) : (
          friends.map((entry) => (
            <RecipientRow
              key={entry.userId}
              id={entry.userId}
              name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
              username={profiles.get(entry.userId)?.username}
              busy={busy}
              onPick={onPick}
            />
          ))
        )}
      </div>
      {results !== null ? (
        <div className="panel-section">
          <h3 className="panel-heading">Search results</h3>
          {results.length === 0 ? (
            <p className="muted">No one found.</p>
          ) : (
            results.map((person) => (
              <RecipientRow
                key={person.accountId}
                id={person.accountId}
                name={person.displayName}
                username={person.username}
                busy={busy}
                onPick={onPick}
              />
            ))
          )}
        </div>
      ) : null}
      <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={busy}>
        Cancel
      </button>
    </div>
  );
}

/**
 * The Wallet section panel: loads the balance, catalogue, statement, progression, badges, and
 * friends on mount, and refreshes the money-side facts after a send.
 */
export function WalletPanel(): ReactNode {
  const { client, accountId } = useMigo();

  const [balance, setBalance] = useState<WalletView | null>(null);
  const [catalogue, setCatalogue] = useState<GiftListing[] | null>(null);
  const [ledger, setLedger] = useState<LedgerEntryWire[] | null>(null);
  const [progression, setProgression] = useState<ProgressionWire | null>(null);
  const [badges, setBadges] = useState<BadgeWire[] | null>(null);
  const [friends, setFriends] = useState<RelationshipEntry[] | null>(null);
  const [leaderboard, setLeaderboard] = useState<RankWire[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // The send flow: which gift is being addressed (the picker is open), and whether a send is in
  // flight. Its failure line is separate from the panel's load error, so a refused send never
  // reads as a broken panel. Success feedback is a plain line, cleared by the next flow.
  const [picking, setPicking] = useState<GiftListing | null>(null);
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [results, setResults] = useState<SuggestedUser[] | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const [wallet, gifts, statement] = await Promise.all([
        client.economy.getBalance(),
        client.economy.getGiftCatalogue(),
        client.economy.getLedger(LEDGER_ROWS),
      ]);
      setBalance(wallet);
      setCatalogue(gifts);
      setLedger(statement);
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client]);

  // Progression, badges, the friends list, and the XP leaderboard are standing facts, not money:
  // they load once and are not part of the post-send refresh.
  useEffect(() => {
    if (!client || accountId === null) {
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      try {
        const [standing, honours, relationships, top] = await Promise.all([
          client.economy.getProgression(accountId),
          client.economy.getBadges(accountId),
          client.social.listRelationships(),
          client.economy.getLeaderboard('xp', LEADERBOARD_ROWS),
        ]);
        if (!cancelled) {
          setProgression(standing);
          setBadges(honours);
          setFriends(relationships.filter((entry) => entry.kind === KIND_FRIEND));
          setLeaderboard(top);
        }
      } catch (cause) {
        if (!cancelled) {
          setError(friendlyError(cause));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, accountId]);

  useEffect(() => {
    void reload();
  }, [reload]);

  /** Sends the picked gift; on success the picker closes and the money-side facts re-read. */
  const sendTo = useCallback(
    (recipient: Id): void => {
      if (!client || picking === null || sending) {
        return;
      }
      setSending(true);
      setSendError(null);
      setNotice(null);
      client.economy
        .sendGift(picking.sku, recipient)
        .then(() => {
          setNotice(`Sent ${picking.name}.`);
          setPicking(null);
          setResults(null);
          return reload();
        })
        .catch((cause: unknown) => {
          setSendError(friendlyError(cause));
        })
        .finally(() => {
          setSending(false);
        });
    },
    [client, picking, sending, reload],
  );

  const onSearch = useCallback(
    (text: string): void => {
      if (!client || text.length === 0) {
        return;
      }
      client.social
        .search(text, 20)
        .then((found) => {
          setResults(found);
        })
        .catch((cause: unknown) => {
          setSendError(friendlyError(cause));
        });
    },
    [client],
  );

  // The picker resolves names for friends and search results through the shared profile cache.
  // The leaderboard's rows resolve through the same cache, so a ranked account and a friend show
  // the same name and picture for the same person.
  const pickerIds = useMemo(() => {
    const ids: Id[] = (friends ?? []).map((entry) => entry.userId);
    for (const person of results ?? []) {
      ids.push(person.accountId);
    }
    for (const rank of leaderboard ?? []) {
      ids.push(rank.accountId);
    }
    return ids;
  }, [friends, results, leaderboard]);
  const profiles = useProfiles(pickerIds);

  return (
    <div className="panel">
      <header className="panel-head">
        <h1 className="panel-title">Wallet</h1>
        <span className="mig-chip" title="Migo's coin">
          <CoinMark size={14} />
          $MIG
        </span>
      </header>

      {error !== null ? <p className="form-error">{error}</p> : null}
      {notice !== null ? <p className="hint">{notice}</p> : null}

      {balance === null || catalogue === null || ledger === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : (
        <>
          <section className="panel-section" aria-label="Balance">
            <h2 className="panel-heading">Balance</h2>
            <BalanceCard balance={balance} />
          </section>

          {progression !== null ? (
            <section className="panel-section" aria-label="Progression">
              <h2 className="panel-heading">Progress</h2>
              <ProgressionCard progression={progression} />
            </section>
          ) : null}

          {badges !== null ? (
            <section className="panel-section" aria-label="Badges">
              <h2 className="panel-heading">Badges</h2>
              <BadgeShelf badges={badges} />
            </section>
          ) : null}

          <section className="panel-section" aria-label="Gift shop">
            <h2 className="panel-heading">Send a gift</h2>
            {catalogue.length === 0 ? (
              <p className="muted">The gift shop is empty on this server.</p>
            ) : (
              <GiftGrid
                gifts={catalogue}
                onSend={(gift) => {
                  setNotice(null);
                  setPicking(gift);
                }}
                disabled={picking !== null}
              />
            )}
          </section>

          <section className="panel-section" aria-label="Recent activity">
            <h2 className="panel-heading">Recent activity</h2>
            <LedgerList entries={ledger} />
          </section>

          {leaderboard !== null ? (
            <section className="panel-section" aria-label="Leaderboard">
              <h2 className="panel-heading">Leaderboard</h2>
              <LeaderboardList ranks={leaderboard} profiles={profiles} />
            </section>
          ) : null}
        </>
      )}

      {picking !== null ? (
        <RecipientPicker
          gift={picking}
          friends={friends ?? []}
          results={results}
          profiles={profiles}
          onSearch={onSearch}
          onPick={sendTo}
          onCancel={() => {
            setPicking(null);
            setResults(null);
            setSendError(null);
          }}
          busy={sending}
          error={sendError}
        />
      ) : null}
    </div>
  );
}
