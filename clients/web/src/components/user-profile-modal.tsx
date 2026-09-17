'use client';

/**
 * Another user's profile, as a modal overlay.
 *
 * The profile arrives through the shared profile cache ({@link refreshProfile}), so the avatar
 * lands resolved and every other surface that knows this person sees the same facts. Level and
 * badges are economy facts ({@link EconomyDomain.getProgression} / {@link getBadges}), fetched
 * beside the profile because the wire keeps them on a different service; a failure of either is
 * degraded, not fatal — the card shows the profile with the standing lines simply absent.
 *
 * Blocking is one-sided and set-only here ({@link SocialDomain.blockUser}); the unblock call
 * ({@link SocialDomain.unblockUser}) belongs to the Friends tab's Blocked section, so this
 * control reads "Block" until it succeeds and "Blocked" (disabled) after.
 * Whether the person is already blocked stays the opener's fact to pass in — the block edges are
 * only half of the graph the modal reads for its social line, and openers that hold the whole
 * graph re-read it on their own block handler anyway.
 *
 * The card itself is an exported presentational component over plain data, so its rules (the
 * blocked control's disabled state, the missing-Message gate, the badge row, the social line)
 * are testable without a live client.
 *
 * The social line and the XP-board rank are the modal's own reads, not the opener's: the
 * relationship comes from the one graph walk ({@link SocialDomain.listAllRelationships}) and the
 * rank from the XP board page that already ranks the whole community — both degrade to a missing
 * line, never a broken card. Friend actions ({@link SocialDomain.friendRequest} /
 * {@link friendRespond}) re-read the graph afterwards so the line states what the wire now says.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';

import { RelationshipKind } from '@migo/sdk';
import type { BadgeWire, Id, ProgressionWire } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { refreshProfile } from '@/lib/migo/use-profiles.js';
import type { ResolvedProfile } from '@/lib/migo/use-profiles.js';
import { presenceLabel } from '@/lib/migo/use-presence.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useMuted } from '@/lib/migo/muted-provider.js';

import { Avatar } from './avatar.js';
import { BotBadge } from './bot-badge.js';
import { Spinner } from './spinner.js';

// The relationship kinds the card files its social line under, as plain numbers the wire may
// extend past the enum's names — the same guard the friends panel uses.
const KIND_FRIEND: number = RelationshipKind.Friend;
const KIND_PENDING_INCOMING: number = RelationshipKind.PendingIncoming;
const KIND_PENDING_OUTGOING: number = RelationshipKind.PendingOutgoing;
const KIND_BLOCK: number = RelationshipKind.Block;

/** The card of facts about one person: profile, standing, the social line, and the actions. */
export function UserProfileCard({
  profile,
  progression,
  badges,
  rank,
  relationship,
  blocked,
  muted = false,
  canMessage,
  busy,
  muteBusy = false,
  friendBusy = false,
  onMessage,
  onBlock,
  onMute,
  onFriendRequest,
  onFriendRespond,
  onGift,
  onReport,
}: {
  profile: ResolvedProfile;
  /** The person's XP standing, when it loaded; absent is a missing line, not a broken card. */
  progression?: ProgressionWire;
  /** The person's badges, when they loaded; an empty row renders nothing. */
  badges?: BadgeWire[];
  /** The person's position on the XP board, when they hold one inside the page read. */
  rank?: number;
  /** The viewer's relationship to this person, as the wire's plain kind number; absent is unknown. */
  relationship?: number;
  /** True when the viewer already blocks this person. */
  blocked: boolean;
  /** True when the viewer has personally muted this person's room chatter. */
  muted?: boolean;
  /** False when the Send Message action is not offered (e.g. the person is blocked). */
  canMessage: boolean;
  /** True while the block request is in flight. */
  busy: boolean;
  /** True while a mute/unmute request is in flight. */
  muteBusy?: boolean;
  /** True while a friend request or response is in flight. */
  friendBusy?: boolean;
  onMessage?: (userId: Id) => void;
  onBlock?: (userId: Id) => void;
  /** Toggles a personal mute; when omitted the control is not offered. */
  onMute?: (userId: Id, on: boolean) => void;
  /** Sends a friend request, on the "no relationship yet" line. */
  onFriendRequest?: () => void;
  /** Answers a pending incoming request; `accept` carries which way. */
  onFriendRespond?: (accept: boolean) => void;
  /** Hands the person to the opener's gift flow; offered only where one exists. */
  onGift?: () => void;
  /**
   * Files a report about this person; offered only by an opener that can host the report dialog.
   *
   * Deliberately not a sibling of the block control's own state machine: a report is not a
   * personal act the card can reflect (the reporter is never told the outcome and nothing about
   * the card changes), so this is a plain hand-off with no busy state and no result to show.
   */
  onReport?: () => void;
}): ReactNode {
  const [copied, setCopied] = useState(false);
  const presence = presenceLabel(profile.presence);
  // The level bar's share, clamped so a stale or future wire value cannot escape its track.
  const intoLevel = progression && progression.xpForNextLevel > 0 ? progression.xpIntoLevel : 0;
  const levelSpan = progression && progression.xpForNextLevel > 0 ? progression.xpForNextLevel : 0;
  const levelShare = levelSpan > 0 ? Math.min(100, Math.round((intoLevel / levelSpan) * 100)) : 0;

  /** Copies the shareable public id — the one identifier a person can hand out freely. */
  const copyId = useCallback((): void => {
    void (async (): Promise<void> => {
      try {
        await navigator.clipboard.writeText(profile.publicId);
        setCopied(true);
      } catch {
        /* the clipboard may be locked; the id stays selectable as the fallback */
      }
    })();
  }, [profile.publicId]);

  // The social line: what the viewer is to this person, and the one act that state admits.
  const socialLine: ReactNode = (() => {
    if (blocked || relationship === KIND_BLOCK) {
      return null; // the Block control below already states this state its own way.
    }
    if (relationship === KIND_FRIEND) {
      return (
        <span className="profile-rel profile-rel-friend" title="You are friends">
          ✓ Friends
        </span>
      );
    }
    if (relationship === KIND_PENDING_OUTGOING) {
      return (
        <span className="profile-rel" title="Waiting on their answer">
          Request sent
        </span>
      );
    }
    if (relationship === KIND_PENDING_INCOMING) {
      return (
        <div className="profile-rel-row">
          <span className="profile-rel">wants to be your friend</span>
          {onFriendRespond ? (
            <>
              <button
                type="button"
                className="btn btn-primary"
                disabled={friendBusy}
                onClick={() => onFriendRespond(true)}
              >
                Accept
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                disabled={friendBusy}
                onClick={() => onFriendRespond(false)}
              >
                Decline
              </button>
            </>
          ) : null}
        </div>
      );
    }
    if (relationship !== undefined && onFriendRequest !== undefined) {
      return (
        <button
          type="button"
          className="btn btn-ghost"
          disabled={friendBusy}
          onClick={onFriendRequest}
        >
          Add friend
        </button>
      );
    }
    return null;
  })();

  return (
    <div className="profile-card">
      <div className="profile-head">
        <Avatar
          name={profile.displayName}
          id={profile.userId}
          size={56}
          avatarUrl={profile.avatarUrl}
          presence={profile.presence}
        />
        <div className="profile-id">
          <span className="person-name">
            {profile.displayName}
            {profile.verified ? (
              <span className="profile-verified" title="Verified account">
                ✔
              </span>
            ) : null}
            <BotBadge botId={profile.botId} />
          </span>
          {profile.username ? <span className="person-sub">@{profile.username}</span> : null}
          {presence ? <span className="person-sub profile-presence">{presence}</span> : null}
          {progression ? <span className="person-sub">Level {progression.level}</span> : null}
          {profile.customStatus ? (
            <span className="person-sub profile-status">“{profile.customStatus}”</span>
          ) : null}
        </div>
      </div>

      {profile.bio ? <p className="profile-bio">{profile.bio}</p> : null}

      <div className="profile-facts">
        {profile.country ? <span className="profile-fact">🌍 {profile.country}</span> : null}
        {profile.language ? <span className="profile-fact">🗣 {profile.language}</span> : null}
        {progression ? <span className="profile-fact">⭐ {progression.xp} XP</span> : null}
        {rank !== undefined ? (
          <span className="profile-fact" title="Their position on the XP board">
            🏆 #{rank} on the XP board
          </span>
        ) : null}
        <span className="profile-fact profile-fact-id">
          🪪 {profile.publicId}
          <button
            type="button"
            className="profile-copy-id"
            onClick={copyId}
            aria-label={copied ? 'Copied' : `Copy ${profile.publicId}`}
            title="Copy the shareable id"
          >
            {copied ? '✓' : '📋'}
          </button>
        </span>
      </div>

      {progression && levelSpan > 0 ? (
        <div
          className="profile-progress"
          role="img"
          aria-label={`Level ${progression.level}, ${intoLevel} of ${levelSpan} XP towards the next`}
        >
          <div className="profile-progress-track">
            <div className="profile-progress-fill" style={{ width: `${levelShare}%` }} />
          </div>
          <span className="profile-progress-note">
            {intoLevel} / {levelSpan} XP to level {progression.level + 1}
          </span>
        </div>
      ) : null}

      {badges && badges.length > 0 ? (
        <div className="badge-row" aria-label="Badges">
          {badges.map((badge) => (
            <span
              key={badge.badgeCode}
              className="badge-chip"
              title={`Earned ${formatRelative(badge.awardedAt)}`}
            >
              🏅 {badge.badgeCode}
            </span>
          ))}
        </div>
      ) : null}

      {socialLine}

      <div className="modal-actions">
        {canMessage && onMessage ? (
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => onMessage(profile.userId)}
          >
            Send Message
          </button>
        ) : null}
        {onGift ? (
          <button type="button" className="btn btn-ghost" onClick={onGift}>
            Gift
          </button>
        ) : null}
        {onMute ? (
          <button
            type="button"
            className="btn btn-ghost"
            disabled={muteBusy}
            onClick={() => onMute(profile.userId, !muted)}
            aria-label={muted ? `Unmute ${profile.displayName}` : `Mute ${profile.displayName}`}
            title="Hides this person’s room messages for you. Direct messages are never muted."
          >
            {muteBusy ? <Spinner /> : muted ? 'Unmute' : 'Mute for me'}
          </button>
        ) : null}
        {onBlock ? (
          <button
            type="button"
            className="btn btn-danger"
            disabled={blocked || busy}
            onClick={() => onBlock(profile.userId)}
            aria-label={blocked ? 'Blocked' : `Block ${profile.displayName}`}
          >
            {busy ? <Spinner /> : blocked ? 'Blocked' : 'Block'}
          </button>
        ) : null}
        {onReport ? (
          <button
            type="button"
            className="btn btn-ghost profile-report-btn"
            onClick={onReport}
            aria-label={`Report ${profile.displayName}`}
            title="Sends a report to this node’s moderators. They are the only ones who read it."
          >
            ⚑ Report
          </button>
        ) : null}
      </div>
    </div>
  );
}

/**
 * The modal overlay for one person's profile.
 *
 * Opens over whatever surface named the person (a friend row, a chat header); closing is the
 * backdrop click or the header's ✕, and `onMessage` hands the person back to the opener to start
 * a conversation — the modal never navigates on its own.
 */
export function UserProfileModal({
  userId,
  blocked = false,
  onClose,
  onMessage,
  onBlock,
  onGift,
  onReport,
}: {
  userId: Id;
  /** The opener's current block state for this person, so the control starts honest. */
  blocked?: boolean;
  onClose: () => void;
  onMessage?: (userId: Id) => void;
  /**
   * Requests a block; when omitted the modal performs it itself and reports the change through
   * its own state. Supplied by openers that hold the relationship graph and must re-read it.
   */
  onBlock?: (userId: Id) => Promise<void> | void;
  /** Hands the person to the opener's gift flow; offered only where one exists. */
  onGift?: () => void;
  /**
   * Opens the opener's report dialog for this person, named as the card names them.
   *
   * The whole card travels rather than a pair of fields, because a card of a bot carries two ids —
   * the account and the bot — and it is not the opener's job to know which one a report takes. The
   * display name is wanted for the header ("Report Ada?") and the acknowledgement sentence, and the
   * opener has no profile of its own to read it from.
   */
  onReport?: (profile: ResolvedProfile) => void;
}): ReactNode {
  const { client } = useMigo();
  const { isMuted, setMuted } = useMuted();

  const [profile, setProfile] = useState<ResolvedProfile | null>(null);
  const [progression, setProgression] = useState<ProgressionWire | null>(null);
  const [badges, setBadges] = useState<BadgeWire[] | null>(null);
  const [relationship, setRelationship] = useState<number | undefined>(undefined);
  const [rank, setRank] = useState<number | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [isBlocked, setIsBlocked] = useState(blocked);
  const [blocking, setBlocking] = useState(false);
  const [muting, setMuting] = useState(false);
  const [friending, setFriending] = useState(false);

  useEffect(() => {
    setIsBlocked(blocked);
  }, [blocked]);

  // Re-reads the one relationship that matters — the viewer's edge to this person — so a friend
  // act can land and be reflected without the opener re-rendering anything.
  const rereadRelationship = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const entries = await client.social.listAllRelationships();
      const edge = entries.find((entry) => entry.userId === userId);
      setRelationship(edge ? edge.kind : undefined);
    } catch {
      setRelationship(undefined); // unknown renders no line, which is honest.
    }
  }, [client, userId]);

  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      try {
        const resolved = await refreshProfile(client, userId);
        if (!cancelled) {
          setProfile(resolved);
        }
      } catch (cause) {
        if (!cancelled) {
          setError(friendlyError(cause));
        }
        return;
      }
      // Standing facts degrade quietly: a profile without its level or badges is still a profile.
      try {
        const standing = await client.economy.getProgression(userId);
        if (!cancelled) {
          setProgression(standing);
        }
      } catch {
        /* absent, not fatal */
      }
      try {
        const earned = await client.economy.getBadges(userId);
        if (!cancelled) {
          setBadges(earned);
        }
      } catch {
        /* absent, not fatal */
      }
      // The social line: the viewer's edge to this person, from the one graph walk.
      try {
        const entries = await client.social.listAllRelationships();
        if (!cancelled) {
          const edge = entries.find((entry) => entry.userId === userId);
          setRelationship(edge ? edge.kind : undefined);
        }
      } catch {
        /* no line, not a broken card */
      }
      // The XP-board rank: only the first page is read — a person off it simply has no rank line.
      try {
        const board = await client.economy.getLeaderboard('xp', 100);
        if (!cancelled) {
          const row = board.find((entry) => entry.accountId === userId);
          setRank(row ? row.position : undefined);
        }
      } catch {
        /* off the board, not fatal */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, userId]);

  /**
   * Runs one block request: the opener's handler when it supplied one, the social domain
   * otherwise, then reflects the set state. The wrapper is sync and the work is async so the
   * card's control contract (a void return) stays clean.
   */
  const runBlock = useCallback(
    (target: Id): void => {
      if (blocking) {
        return;
      }
      setBlocking(true);
      void (async (): Promise<void> => {
        try {
          if (onBlock) {
            await onBlock(target);
          } else if (client) {
            await client.social.blockUser(target);
          }
          setIsBlocked(true);
        } catch (cause) {
          setError(friendlyError(cause));
        } finally {
          setBlocking(false);
        }
      })();
    },
    [blocking, onBlock, client],
  );

  /**
   * Toggles a personal mute through the muted provider (the one owner of the set), then lets the
   * provider's state re-render the control. Sync wrapper over async work, like {@link runBlock}.
   */
  const runMute = useCallback(
    (target: Id, on: boolean): void => {
      if (muting) {
        return;
      }
      setMuting(true);
      void (async (): Promise<void> => {
        try {
          await setMuted(target, on);
        } catch (cause) {
          setError(friendlyError(cause));
        } finally {
          setMuting(false);
        }
      })();
    },
    [muting, setMuted],
  );

  /**
   * Sends a friend request, then re-reads the edge so the line says "Request sent" because the
   * wire says so, not because the button was clicked.
   */
  const runFriendRequest = useCallback((): void => {
    if (friending || !client) {
      return;
    }
    setFriending(true);
    void (async (): Promise<void> => {
      try {
        await client.social.friendRequest(userId);
        await rereadRelationship();
      } catch (cause) {
        setError(friendlyError(cause));
      } finally {
        setFriending(false);
      }
    })();
  }, [friending, client, userId, rereadRelationship]);

  /** Answers a pending incoming request either way, then re-reads the edge like the request does. */
  const runFriendRespond = useCallback(
    (accept: boolean): void => {
      if (friending || !client) {
        return;
      }
      setFriending(true);
      void (async (): Promise<void> => {
        try {
          await client.social.friendRespond(userId, accept);
          await rereadRelationship();
        } catch (cause) {
          setError(friendlyError(cause));
        } finally {
          setFriending(false);
        }
      })();
    },
    [friending, client, userId, rereadRelationship],
  );

  // Portaled to the body: a modal opened from inside a window must be the topmost surface —
  // the desk stacks windows by an unbounded counter, so a dialog rendered in place could stay
  // below another window.
  return createPortal(
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="User profile"
      onClick={onClose}
    >
      <div className="modal" onClick={(event) => event.stopPropagation()}>
        <header className="modal-header">
          <h2>Profile</h2>
          <button type="button" className="icon-btn" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </header>
        <div className="modal-body">
          {error ? <p className="form-error">{error}</p> : null}
          {profile === null && error === null ? (
            <div className="center-fill">
              <Spinner />
            </div>
          ) : profile !== null ? (
            <UserProfileCard
              profile={profile}
              progression={progression ?? undefined}
              badges={badges ?? undefined}
              rank={rank}
              relationship={relationship}
              blocked={isBlocked}
              muted={isMuted(profile.userId)}
              canMessage={!isBlocked}
              busy={blocking}
              muteBusy={muting}
              friendBusy={friending}
              onMessage={onMessage ? () => onMessage(profile.userId) : undefined}
              onBlock={runBlock}
              onMute={runMute}
              onFriendRequest={runFriendRequest}
              onFriendRespond={runFriendRespond}
              onGift={onGift}
              onReport={onReport ? () => onReport(profile) : undefined}
            />
          ) : null}
        </div>
      </div>
    </div>,
    document.body,
  );
}
