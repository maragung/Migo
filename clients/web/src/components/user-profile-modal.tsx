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
 * Blocking is one-sided and set-only on the wire ({@link SocialDomain.blockUser}; there is no
 * unblock call), so the control reads "Block" until it succeeds and "Blocked" (disabled) after.
 * Whether the person is already blocked is the opener's fact to pass in — the modal does not
 * re-read the relationship graph, because every opener already holds it.
 *
 * The card itself is an exported presentational component over plain data, so its rules (the
 * blocked control's disabled state, the missing-Message gate, the badge row) are testable
 * without a live client.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { BadgeWire, Id, ProgressionWire } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { refreshProfile } from '@/lib/migo/use-profiles.js';
import type { ResolvedProfile } from '@/lib/migo/use-profiles.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useMuted } from '@/lib/migo/muted-provider.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

/** The card of facts about one person: profile, standing, and the actions on them. */
export function UserProfileCard({
  profile,
  progression,
  badges,
  blocked,
  muted = false,
  canMessage,
  busy,
  muteBusy = false,
  onMessage,
  onBlock,
  onMute,
}: {
  profile: ResolvedProfile;
  /** The person's XP standing, when it loaded; absent is a missing line, not a broken card. */
  progression?: ProgressionWire;
  /** The person's badges, when they loaded; an empty row renders nothing. */
  badges?: BadgeWire[];
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
  onMessage?: (userId: Id) => void;
  onBlock?: (userId: Id) => void;
  /** Toggles a personal mute; when omitted the control is not offered. */
  onMute?: (userId: Id, on: boolean) => void;
}): ReactNode {
  return (
    <div className="profile-card">
      <div className="profile-head">
        <Avatar
          name={profile.displayName}
          id={profile.userId}
          size={56}
          avatarUrl={profile.avatarUrl}
        />
        <div className="profile-id">
          <span className="person-name">{profile.displayName}</span>
          {profile.username ? <span className="person-sub">@{profile.username}</span> : null}
          {progression ? <span className="person-sub">Level {progression.level}</span> : null}
        </div>
      </div>

      {profile.bio ? <p className="profile-bio">{profile.bio}</p> : null}

      <div className="profile-facts">
        {profile.country ? <span className="profile-fact">🌍 {profile.country}</span> : null}
        {progression ? <span className="profile-fact">⭐ {progression.xp} XP</span> : null}
        <span className="profile-fact">🪪 {profile.publicId}</span>
      </div>

      {badges && badges.length > 0 ? (
        <div className="badge-row" aria-label="Badges">
          {badges.map((badge) => (
            <span key={badge.badgeCode} className="badge-chip" title={badge.badgeCode}>
              🏅 {badge.badgeCode}
            </span>
          ))}
        </div>
      ) : null}

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
}): ReactNode {
  const { client } = useMigo();
  const { isMuted, setMuted } = useMuted();

  const [profile, setProfile] = useState<ResolvedProfile | null>(null);
  const [progression, setProgression] = useState<ProgressionWire | null>(null);
  const [badges, setBadges] = useState<BadgeWire[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isBlocked, setIsBlocked] = useState(blocked);
  const [blocking, setBlocking] = useState(false);
  const [muting, setMuting] = useState(false);

  useEffect(() => {
    setIsBlocked(blocked);
  }, [blocked]);

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

  return (
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
              blocked={isBlocked}
              muted={isMuted(profile.userId)}
              canMessage={!isBlocked}
              busy={blocking}
              muteBusy={muting}
              onMessage={onMessage ? () => onMessage(profile.userId) : undefined}
              onBlock={runBlock}
              onMute={runMute}
            />
          ) : null}
        </div>
      </div>
    </div>
  );
}
