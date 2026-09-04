'use client';

/**
 * A room's details: the roster, the moderation controls, and the way out.
 *
 * The roster is paged wire data ({@link RoomsDomain.getRoster}), re-read on open rather than
 * mirrored locally — membership moves with every join and leave, and the room's own events are
 * the freshest statement of it. Roles arrive as plain numbers ({@link RoomRole} values), so the
 * label mapping lives in one pure function the tests pin, comparing number to number the same
 * way the friends panel does.
 *
 * # Two ways to move against a member
 *
 * The panel offers both recourses the wire has. A **kick vote** ({@link RoomsDomain.voteKick}) is
 * every member's own: it appears on every other member's row (never your own, never the owner's),
 * and its running tally arrives on the broadcast {@link RoomsDomain.onVote} stream so every device
 * watches the same count climb — half the room, rounded up, and the target is gone. A **sanction**
 * ({@link RoomsDomain.sanction}) is the staff path: room silence, kick, or ban, shown only to a
 * moderator or above acting strictly below their own rank, and to a global admin acting on any
 * non-owner member. The two never touch the owner. A room silence is deliberately not the same
 * control as a personal mute — it quiets the person for the whole room, not just for you — so it
 * says so, and the destructive pair (kick, ban) confirm before they fire.
 *
 * Leaving is a one-way door and says so: `rooms.leave` is called, the conversation is dropped
 * from the shared list ({@link forgetConversation}), and the thread closes — a room the account
 * has left must not linger in the sidebar as a conversation it can no longer open.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { RoomRole, SanctionAction } from '@migo/sdk';
import type { AdminStanding, Id, RosterEntry } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';
import { closeConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

/** How many roster rows one open reads; the server clamps its own ceiling above this. */
const ROSTER_LIMIT = 100;

/**
 * The label a roster role renders as, from the plain number the wire carries.
 *
 * `RosterEntry.role` is a `number` (a newer server may send a value this build has no name for),
 * so the enum's numeric values are read into number-typed constants once and compared number to
 * number. The hierarchy collapses to the three words a reader needs: Owner, Admin, Member — a
 * Helper or Moderator is staff, "Admin" is the honest umbrella, and an unknown value from a
 * newer node renders as Member rather than a guess.
 */
const ROLE_OWNER: number = RoomRole.Owner;
const ROLE_MANAGER: number = RoomRole.Manager;
const ROLE_ADMIN: number = RoomRole.Admin;
/** The lowest rank the staff controls answer to; compared number to number, like the labels. */
const ROLE_MODERATOR: number = RoomRole.Moderator;

/** The sanction verbs, held as enum-typed constants so the calls read plainly at the sites. */
const ACTION_MUTE: SanctionAction = SanctionAction.Mute;
const ACTION_KICK: SanctionAction = SanctionAction.Kick;
const ACTION_BAN: SanctionAction = SanctionAction.Ban;

export function roleLabel(role: number): string {
  if (role === ROLE_OWNER) {
    return 'Owner';
  }
  if (role === ROLE_MANAGER || role === ROLE_ADMIN) {
    return 'Admin';
  }
  return 'Member';
}

/**
 * A kick vote's tally, as the fraction the row shows: votes cast over the count needed.
 *
 * Pure, so a test pins it. `needed` is half the room rounded up; a non-positive `needed` (a room
 * too small to have stated one yet) shows the bare count rather than dividing by nothing, and a
 * stale negative vote count is clamped so it never renders below zero.
 */
export function voteTally(votes: number, needed: number): string {
  const cast = Math.max(0, votes);
  if (needed <= 0) {
    return `${cast}`;
  }
  return `${cast}/${needed}`;
}

/**
 * Whether the "Vote kick" control belongs on a member's row.
 *
 * The members' own recourse is open to everyone and needs no rank — but never against yourself,
 * and never against the owner, whom a show of hands cannot unseat.
 */
export function canVoteKick(targetRole: number, isSelf: boolean): boolean {
  return !isSelf && targetRole !== ROLE_OWNER;
}

/**
 * Whether the staff controls (silence, kick, ban) belong on a member's row.
 *
 * Two ways to earn them, and one member they never touch. The owner is never sanctioned from this
 * panel, by anyone. A global admin outranks every room role, so they may act on any other member.
 * Otherwise it is the room's own ladder: a moderator or above may act, and only strictly below
 * their own rank — never on a peer of equal standing, never up the ladder. Identity is the
 * caller's to check (rank alone would let a global admin sanction their own row); this answers
 * rank only.
 */
export function canSanction(myRole: number, targetRole: number, isGlobalAdmin: boolean): boolean {
  if (targetRole === ROLE_OWNER) {
    return false;
  }
  if (isGlobalAdmin) {
    return true;
  }
  return myRole >= ROLE_MODERATOR && myRole > targetRole;
}

/** One roster row: avatar, name, the role badge, and — when offered — the actions on the member. */
export function RosterRow({
  entry,
  name,
  avatarUrl,
  tally,
  canVote = false,
  canModerate = false,
  busy = false,
  onVoteKick,
  onRoomMute,
  onKick,
  onBan,
}: {
  entry: RosterEntry;
  name: string;
  avatarUrl?: string;
  /** The live kick-vote tally against this member ("3/17"), when a vote is open. */
  tally?: string;
  /** Show the "Vote kick" control (every member sees it on others; never self or the owner). */
  canVote?: boolean;
  /** Show the staff controls (Silence, Kick, Ban) — the viewer outranks this member. */
  canModerate?: boolean;
  /** True while an action on this row is in flight, so its controls disable together. */
  busy?: boolean;
  onVoteKick?: () => void;
  onRoomMute?: () => void;
  onKick?: () => void;
  onBan?: () => void;
}): ReactNode {
  const showVote = canVote && onVoteKick !== undefined;
  const showStaff =
    canModerate && (onRoomMute !== undefined || onKick !== undefined || onBan !== undefined);
  return (
    <div className="person-row roster-row">
      <Avatar name={name} id={entry.accountId} size={32} avatarUrl={avatarUrl} />
      <div className="person-main">
        <span className="person-name">{name}</span>
        <span className="person-sub">joined {formatRelative(entry.joinedAt)}</span>
        {tally !== undefined ? (
          <span className="person-note vote-tally">Vote to kick: {tally}</span>
        ) : null}
      </div>
      <span className={`role-badge role-${roleLabel(entry.role).toLowerCase()}`}>
        {roleLabel(entry.role)}
      </span>
      {showVote || showStaff ? (
        <div className="person-actions roster-actions">
          {showVote ? (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={onVoteKick}
              title="Call a vote to remove this person. When half the room agrees, they are kicked."
            >
              Vote kick
            </button>
          ) : null}
          {showStaff && onRoomMute !== undefined ? (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={onRoomMute}
              title="Silences this person for everyone in the room (the server sets the term, around 30 days). Different from muting them just for yourself."
            >
              Silence in room
            </button>
          ) : null}
          {showStaff && onKick !== undefined ? (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={onKick}
              title="Remove this person from the room. They can come back."
            >
              Kick
            </button>
          ) : null}
          {showStaff && onBan !== undefined ? (
            <button
              type="button"
              className="btn btn-danger"
              disabled={busy}
              onClick={onBan}
              title="Remove this person and bar them from returning."
            >
              Ban
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/**
 * The roster list: one row per member, roles labelled, and — for a viewer allowed them — the
 * moderation controls on each other member's row.
 *
 * The viewer's context is optional: with none supplied the list is a plain read (the shape the
 * roster test pins), and the controls appear only as the handlers and the viewer's standing admit
 * them. Each row's eligibility is decided here, from the pure predicates the tests also pin, so the
 * row component stays a presentational surface over booleans.
 */
export function RosterList({
  entries,
  profiles,
  viewerId = null,
  viewerRole = 0,
  isGlobalAdmin = false,
  tallies,
  busyIds,
  onVoteKick,
  onRoomMute,
  onKick,
  onBan,
}: {
  entries: RosterEntry[];
  /** Resolved profiles, for names and avatars; an unresolved member keeps a stable fallback. */
  profiles: ReadonlyMap<Id, { displayName: string; avatarUrl?: string }>;
  /** The viewer, so their own row offers no actions and their rank gates the staff controls. */
  viewerId?: Id | null;
  /** The viewer's room rank as a number; unknown (0) shows no staff controls. */
  viewerRole?: number;
  /** True when the viewer is a global admin, which grants the staff controls room-rank aside. */
  isGlobalAdmin?: boolean;
  /** Open kick-vote tallies by target, pre-formatted through {@link voteTally}. */
  tallies?: ReadonlyMap<Id, string>;
  /** Targets with an action in flight, so their row disables while it settles. */
  busyIds?: ReadonlySet<Id>;
  onVoteKick?: (targetId: Id) => void;
  onRoomMute?: (targetId: Id) => void;
  onKick?: (targetId: Id) => void;
  onBan?: (targetId: Id) => void;
}): ReactNode {
  if (entries.length === 0) {
    return <p className="muted">No one else is here.</p>;
  }
  const hasStaffHandlers = onRoomMute !== undefined || onKick !== undefined || onBan !== undefined;
  return (
    <div className="roster-list">
      {entries.map((entry) => {
        const isSelf = viewerId !== null && entry.accountId === viewerId;
        const canVote = onVoteKick !== undefined && canVoteKick(entry.role, isSelf);
        const canModerate =
          !isSelf && hasStaffHandlers && canSanction(viewerRole, entry.role, isGlobalAdmin);
        return (
          <RosterRow
            key={entry.accountId}
            entry={entry}
            name={profiles.get(entry.accountId)?.displayName ?? 'Someone'}
            avatarUrl={profiles.get(entry.accountId)?.avatarUrl}
            tally={tallies?.get(entry.accountId)}
            canVote={canVote}
            canModerate={canModerate}
            busy={busyIds?.has(entry.accountId) ?? false}
            onVoteKick={onVoteKick ? () => onVoteKick(entry.accountId) : undefined}
            onRoomMute={onRoomMute ? () => onRoomMute(entry.accountId) : undefined}
            onKick={onKick ? () => onKick(entry.accountId) : undefined}
            onBan={onBan ? () => onBan(entry.accountId) : undefined}
          />
        );
      })}
    </div>
  );
}

/** The room details drawer: roster, moderation, plus the leave control. */
export function RoomInfoPanel({
  roomId,
  conversationId,
}: {
  roomId: Id;
  conversationId: Id;
}): ReactNode {
  const { client, accountId } = useMigo();
  const { forgetConversation } = useConversations();
  const { forgetRoom } = useRooms();

  const [roster, setRoster] = useState<RosterEntry[] | null>(null);
  const [standing, setStanding] = useState<AdminStanding | null>(null);
  // Raw kick-vote tallies by target; the labels the rows show derive from these.
  const [tallies, setTallies] = useState<ReadonlyMap<Id, { votes: number; needed: number }>>(
    new Map(),
  );
  // Targets with a vote or sanction in flight, so a row disables while its action settles.
  const [busyIds, setBusyIds] = useState<ReadonlySet<Id>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [leaving, setLeaving] = useState(false);

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      setRoster(await client.rooms.getRoster(roomId, ROSTER_LIMIT));
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client, roomId]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // The viewer's global-admin standing, read once: it lets a server admin moderate a room whose
  // rank they do not hold. A failure (not an admin, or the endpoint unreachable) leaves the safe
  // default — no elevated controls.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client
      .adminStanding()
      .then((value) => {
        if (!cancelled) {
          setStanding(value);
        }
      })
      .catch(() => {
        /* not an admin, or unreachable: the room-rank path is the only one, which is the default */
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  // The live kick-vote tallies: the broadcast stream keeps every member's count in step, and a
  // closed vote drops from the map — and re-reads the roster, since a vote that passed just removed
  // someone the list must stop showing.
  useEffect(() => {
    if (!client) {
      return;
    }
    return client.rooms.onVote((event) => {
      if (event.roomId !== roomId) {
        return;
      }
      setTallies((prev) => {
        const next = new Map(prev);
        if (event.closed) {
          next.delete(event.targetId);
        } else {
          next.set(event.targetId, { votes: event.votes, needed: event.needed });
        }
        return next;
      });
      if (event.closed) {
        void reload();
      }
    });
  }, [client, roomId, reload]);

  // The roster's members resolve through the shared profile cache, so names and avatars match
  // every other surface that shows these people.
  const memberIds = (roster ?? []).map((entry) => entry.accountId);
  const profiles = useProfiles(memberIds);

  // The viewer's own room rank, from their roster row; unknown — or not on the roster at all — is
  // the lowest standing, which shows no staff controls. A global admin's elevation is separate.
  const myRole: number =
    (accountId !== null
      ? (roster ?? []).find((entry) => entry.accountId === accountId)?.role
      : undefined) ?? 0;
  const isGlobalAdmin = standing?.owner === true || standing?.admin === true;

  // The raw tallies become the labels the rows show, through the same pure formatter a test pins.
  const tallyLabels = useMemo<ReadonlyMap<Id, string>>(() => {
    const labels = new Map<Id, string>();
    for (const [targetId, tally] of tallies) {
      labels.set(targetId, voteTally(tally.votes, tally.needed));
    }
    return labels;
  }, [tallies]);

  const nameOf = (id: Id): string => profiles.get(id)?.displayName ?? 'this member';

  /** Marks a target busy, runs the work, re-reads the roster, and clears the busy mark. */
  function withBusy(targetId: Id, work: () => Promise<unknown>): void {
    setBusyIds((prev) => new Set(prev).add(targetId));
    setError(null);
    void (async (): Promise<void> => {
      try {
        await work();
        await reload();
      } catch (cause) {
        setError(friendlyError(cause));
      } finally {
        setBusyIds((prev) => {
          const next = new Set(prev);
          next.delete(targetId);
          return next;
        });
      }
    })();
  }

  // A vote is its own path: the reply carries the fresh tally (seeded at once so the caller does
  // not wait for the broadcast to echo back), and a vote that closed the case re-reads the roster.
  function castVote(targetId: Id): void {
    const active = client;
    if (!active) {
      return;
    }
    setBusyIds((prev) => new Set(prev).add(targetId));
    setError(null);
    active.rooms
      .voteKick(roomId, targetId)
      .then((res) => {
        setTallies((prev) => {
          const next = new Map(prev);
          if (res.open) {
            next.set(targetId, { votes: res.votes, needed: res.needed });
          } else {
            next.delete(targetId);
          }
          return next;
        });
        if (!res.open) {
          void reload();
        }
      })
      .catch((cause: unknown) => setError(friendlyError(cause)))
      .finally(() => {
        setBusyIds((prev) => {
          const next = new Set(prev);
          next.delete(targetId);
          return next;
        });
      });
  }

  // A room silence is not the personal mute: it quiets the person for the whole room. It is not
  // destructive, so it fires without a confirm.
  function silence(targetId: Id): void {
    const active = client;
    if (!active) {
      return;
    }
    withBusy(targetId, () => active.rooms.sanction({ roomId, targetId, action: ACTION_MUTE }));
  }

  // Kick and ban remove a person, so neither is silent: the member is named before the server acts.
  function kick(targetId: Id): void {
    const active = client;
    if (!active) {
      return;
    }
    if (!window.confirm(`Kick ${nameOf(targetId)} from the room? They can come back.`)) {
      return;
    }
    withBusy(targetId, () => active.rooms.sanction({ roomId, targetId, action: ACTION_KICK }));
  }

  function ban(targetId: Id): void {
    const active = client;
    if (!active) {
      return;
    }
    if (!window.confirm(`Ban ${nameOf(targetId)}? They are removed and barred from returning.`)) {
      return;
    }
    withBusy(targetId, () => active.rooms.sanction({ roomId, targetId, action: ACTION_BAN }));
  }

  const leave = useCallback((): void => {
    if (!client || leaving) {
      return;
    }
    setLeaving(true);
    client.rooms
      .leave(roomId)
      .then(() => {
        // The room's record goes before the conversation: the server's member fan-out excludes
        // the leaver's own device, so the held counts would otherwise outlive the membership
        // and the directory row would keep showing a room of one that nobody is in.
        forgetRoom(roomId);
        forgetConversation(conversationId);
        closeConversation();
      })
      .catch((cause: unknown) => {
        setError(friendlyError(cause));
      })
      .finally(() => {
        setLeaving(false);
      });
  }, [client, leaving, roomId, conversationId, forgetConversation, forgetRoom]);

  return (
    <div className="room-info" aria-label="Room details">
      {error ? <p className="form-error">{error}</p> : null}
      {roster === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : (
        <>
          <div className="panel-head">
            <h2 className="panel-heading">Members ({roster.length})</h2>
            <button
              type="button"
              className="btn btn-danger"
              disabled={leaving}
              onClick={leave}
              aria-label="Leave room"
            >
              {leaving ? <Spinner /> : 'Leave Room'}
            </button>
          </div>
          <RosterList
            entries={roster}
            profiles={profiles}
            viewerId={accountId}
            viewerRole={myRole}
            isGlobalAdmin={isGlobalAdmin}
            tallies={tallyLabels}
            busyIds={busyIds}
            onVoteKick={castVote}
            onRoomMute={silence}
            onKick={kick}
            onBan={ban}
          />
        </>
      )}
    </div>
  );
}
