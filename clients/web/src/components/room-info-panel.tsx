'use client';

/**
 * A room's details: the roster, the moderation controls, the settings its staff may change, and the
 * way out.
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
 * has left must not linger in the sidebar as a conversation it can no longer open. The room's
 * whole client-side life ends in the same breath ({@link MigoClient.teardownRoom}): both topics,
 * the crypto state, and the bridge, so a re-join starts fresh chains rather than reusing keys
 * the departed members may still hold.
 *
 * # The room's own settings
 *
 * Three more controls answer to the room's rank ladder rather than the moderation one, and the panel
 * gates them on the same defaults the server resolves. A **rename** (the name and the topic) and the
 * **slow-mode interval** — how long a member must wait between messages — need the room's edit
 * permission, which an Administrator and above hold by default; the interval is whole seconds on
 * this screen and milliseconds on the wire, zero turns it off, and the server refuses an hour's
 * worth or more. **Role management** — the per-member "Make …" items in the roster menu — needs the
 * manage permission, a Manager or the Owner, and the server refuses a grant at or above the actor's
 * own rank, so the items offered are the ranks strictly below the viewer's. **Archiving** is the
 * owner's alone: no other rank, however senior, may end the room for everybody in it, and there is
 * no unarchive — the confirmation says exactly that. Each gate is asked in one place,
 * {@link effectivePermission}: a per-member override the roster cannot see would win there the day
 * a wire surface grows one, and until then the role default answers and the server's refusal is the
 * last word, surfaced as the error it returns.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { ReportSubject, RoomRole, SanctionAction } from '@migo/sdk';
import type { AdminStanding, Id, RosterEntry } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { debounce } from '@/lib/debounce.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { applyRoomSettings, useRooms } from '@/lib/migo/rooms-provider.js';
import { closeConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { ReportDialog } from './report-dialog.js';
import type { ReportSubjectRef } from './report-dialog.js';
import { Spinner } from './spinner.js';
import { UserProfileModal } from './user-profile-modal.js';

/** How many roster rows one open reads; the server clamps its own ceiling above this. */
const ROSTER_LIMIT = 100;
/**
 * How long a member-event re-read waits for the movement to stop, so a burst of joins and leaves
 * costs one roster read rather than one per event.
 */
const MEMBER_EVENT_DEBOUNCE_MS = 300;

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

/**
 * The ceiling a slow-mode interval may reach: an hour, in the whole seconds the store keeps. The
 * server refuses anything above it, so the field bound and the parser agree with the wire rather
 * than letting a screen ask for a change the node will only return as an error.
 */
export const SLOW_MODE_MAX_SECONDS = 3600;

/**
 * A slow-mode field's seconds, or `null` when the text names no interval the server would take.
 *
 * Pure, so a test pins it. Whole seconds only — the wire's milliseconds truncate, so a half-second
 * interval is slow mode off rather than a shorter one, and a field that cannot name a whole second
 * of wait is refused here rather than sent as something it is not. Zero is slow mode off and is a
 * valid answer, the one the clear affordance writes; a negative, a decimal, an empty field, or an
 * interval past the server's hour is not.
 */
export function parseSlowModeSeconds(raw: string): number | null {
  const trimmed = raw.trim();
  if (!/^\d+$/.test(trimmed)) {
    return null;
  }
  const seconds = Number(trimmed);
  return seconds > SLOW_MODE_MAX_SECONDS ? null : seconds;
}

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

/**
 * The room-level actions this panel gates, one per question the server asks before admitting them.
 *
 * `edit` and `manage` are the wire's own permission bits (`ROOM_EDIT`, `ROOM_MANAGE`): the settings
 * patch — the rename, the topic, the slow-mode interval — needs the first, and a role change or a
 * permission override needs the second. `archive` is not a bit at all: the server checks the room's
 * owner column itself, and no permission anyone holds can hand the ending of a room to another
 * member, which is why {@link effectivePermission} never consults an override for it.
 */
export type RoomAction = 'edit' | 'manage' | 'archive';

/**
 * A member's standing in a room, as this client can see it.
 *
 * The roster carries a role and nothing more: `RosterEntry` is the account, the rank, and the join
 * time, and the wire has no surface that reports the per-member permission overrides the server
 * stores (a grant/deny mask per membership row, invisible even to the member events, which carry a
 * role and not a permission set). So the role is the whole answer today, and `overrides` is the
 * seam: the day a wire surface grows one, the overrides land in this record and every gate that
 * asks {@link effectivePermission} moves with them, because this is the one place the question is
 * answered.
 */
export interface MemberStanding {
  /** The member's room rank, as the roster carries it: a plain wire number. */
  role: number;
  /**
   * Per-member overrides of the role's permission defaults, keyed by action. Always absent from
   * real data today — the roster cannot see them — so the role default answers; a test pins the
   * precedence for the day the wire grows a surface.
   */
  overrides?: Readonly<Partial<Record<RoomAction, boolean>>>;
}

/**
 * Whether a member may take a room-level action: the override first, the role's default second.
 *
 * Pure, so a test pins it, including the precedence. An override, when one is visible, is the
 * member's *effective* permission — a grant hands a bit to a rank the defaults would refuse, and a
 * deny takes it from a rank the defaults would give it — so it wins outright rather than being
 * folded into a rank comparison, the same order the server resolves its own masks in (role default,
 * plus the grant, minus the deny). Archive is the exception the server itself makes: the owner
 * column, not a permission bit, decides it, so no override is consulted and the owner's rank is the
 * whole answer. Until the wire carries overrides, the role default is the panel's answer and the
 * server's refusal is the last word, surfaced as the error it returns.
 */
export function effectivePermission(member: MemberStanding, action: RoomAction): boolean {
  if (action === 'archive') {
    return member.role === ROLE_OWNER;
  }
  const override = member.overrides?.[action];
  if (override !== undefined) {
    return override;
  }
  return action === 'manage' ? member.role >= ROLE_MANAGER : member.role >= ROLE_ADMIN;
}

/**
 * Whether the role controls belong on a member's row.
 *
 * Pure, so a test pins it. The server demands the room's manage permission — a Manager or the Owner
 * by default, though an override can hand the bit to a lower rank the roster cannot see — and that
 * the actor strictly outrank the member; the owner's row is beyond everyone's reach and the viewer's
 * own row carries no actions. A global admin's elevation does not help here: unlike a sanction, a
 * role change is resolved from the membership row alone.
 */
export function canSetRole(myRole: number, targetRole: number, isSelf: boolean): boolean {
  return (
    !isSelf &&
    targetRole !== ROLE_OWNER &&
    effectivePermission({ role: myRole }, 'manage') &&
    myRole > targetRole
  );
}

/**
 * The roles this viewer may grant, as the roster menu's "Make …" items.
 *
 * Pure, so a test pins it. The server refuses a grant of Owner outright (ownership moves by transfer)
 * and any role at or above the actor's own, so the ladder stops one rung below the viewer: an Owner
 * may grant Manager on down, a Manager Administrator on down, and a Moderator nothing at all. The
 * roles are the wire's real names — the badge collapses Helper and Moderator into "Admin", but a role
 * change must name the rank the server will store.
 */
export function settableRoles(myRole: number): ReadonlyArray<{ value: number; label: string }> {
  // Held as numbers, like every other rank comparison in this file: the roster's roles arrive as
  // plain numbers, and the gate is a ladder position, not an enum identity.
  const ladder: ReadonlyArray<{ value: number; label: string }> = [
    { value: RoomRole.Member, label: 'Member' },
    { value: RoomRole.Helper, label: 'Helper' },
    { value: RoomRole.Moderator, label: 'Moderator' },
    { value: RoomRole.Admin, label: 'Administrator' },
    { value: RoomRole.Manager, label: 'Manager' },
  ];
  return ladder.filter((role) => role.value < myRole);
}

/**
 * One roster row: avatar, name, the role badge — and, on a click, the member menu.
 *
 * The row itself is the entry: tapping it opens the actions this viewer may take against the
 * member (view profile, gift, vote kick, and the staff sanctions when rank admits them) instead
 * of laying them out beside every name. The menu is rendered into the row and hidden until the
 * click, so its contents are part of the row's own markup — the roster tests read exactly what a
 * viewer would be offered, not an empty shell.
 */
export function RosterRow({
  entry,
  name,
  avatarUrl,
  tally,
  canVote = false,
  canModerate = false,
  busy = false,
  onViewProfile,
  onGift,
  onVoteKick,
  onSetRole,
  settableRoles: settable,
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
  /**
   * Grant this member a role. Supplied only with {@link settableRoles} and only when the viewer may
   * manage roles at all; the member's current role is not among the items.
   */
  onSetRole?: (role: number) => void;
  /** The roles the viewer may grant on this row, as the "Make …" menu items. */
  settableRoles?: ReadonlyArray<{ value: number; label: string }>;
  onRoomMute?: () => void;
  onKick?: () => void;
  onBan?: () => void;
  /** Open this member's profile; supplied by openers that can show one. */
  onViewProfile?: () => void;
  /** Hand this member to the gift flow; never supplied for the viewer's own row. */
  onGift?: () => void;
}): ReactNode {
  const [open, setOpen] = useState(false);
  const showVote = canVote && onVoteKick !== undefined;
  const showStaff =
    canModerate && (onRoomMute !== undefined || onKick !== undefined || onBan !== undefined);
  const showRoles = onSetRole !== undefined && (settable?.length ?? 0) > 0;
  const hasMenu =
    onViewProfile !== undefined || onGift !== undefined || showVote || showRoles || showStaff;
  const close = (): void => setOpen(false);
  return (
    <div className="roster-row-wrap">
      <button
        type="button"
        className="person-row roster-row"
        onClick={() => setOpen(!open)}
        disabled={!hasMenu}
        aria-haspopup="menu"
        aria-expanded={open}
        title={hasMenu ? `Options for ${name}` : undefined}
      >
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
        {hasMenu ? <Icon name="chevron-right" size={14} className="roster-menu-cue" /> : null}
      </button>
      {hasMenu ? (
        <>
          {open ? (
            <button
              type="button"
              className="menu-backdrop"
              onClick={close}
              aria-label="Close the member menu"
            />
          ) : null}
          {/* The menu rides in the row's markup and is hidden until the click, so what the roster
              offers a viewer is part of the row itself — one render, one place to read it. */}
          <div className="roster-menu" role="menu" hidden={!open}>
            {onViewProfile !== undefined ? (
              <button
                type="button"
                role="menuitem"
                className="roster-menu-item"
                onClick={() => {
                  close();
                  onViewProfile();
                }}
              >
                View profile
              </button>
            ) : null}
            {onGift !== undefined ? (
              <button
                type="button"
                role="menuitem"
                className="roster-menu-item"
                onClick={() => {
                  close();
                  onGift();
                }}
                title="Send this person a gift from the shop."
              >
                Gift
              </button>
            ) : null}
            {showVote ? (
              <button
                type="button"
                role="menuitem"
                className="roster-menu-item"
                disabled={busy}
                onClick={() => {
                  close();
                  onVoteKick?.();
                }}
                title="Call a vote to remove this person. When half the room agrees, they are kicked."
              >
                Vote kick
              </button>
            ) : null}
            {/* The role items name the wire's real ranks, not the badge's collapsed labels: a grant
                must name the rank the server will store. */}
            {showRoles && settable !== undefined
              ? settable.map((role) => (
                  <button
                    key={role.value}
                    type="button"
                    role="menuitem"
                    className="roster-menu-item"
                    disabled={busy}
                    onClick={() => {
                      close();
                      onSetRole?.(role.value);
                    }}
                    title={`Make this member ${role.label} of the room.`}
                  >
                    Make {role.label}
                  </button>
                ))
              : null}
            {showStaff && onRoomMute !== undefined ? (
              <button
                type="button"
                role="menuitem"
                className="roster-menu-item"
                disabled={busy}
                onClick={() => {
                  close();
                  onRoomMute();
                }}
                title="Silences this person for everyone in the room (the server sets the term, around 30 days). Different from muting them just for yourself."
              >
                Silence in room
              </button>
            ) : null}
            {showStaff && onKick !== undefined ? (
              <button
                type="button"
                role="menuitem"
                className="roster-menu-item"
                disabled={busy}
                onClick={() => {
                  close();
                  onKick();
                }}
                title="Remove this person from the room. They can come back."
              >
                Kick
              </button>
            ) : null}
            {showStaff && onBan !== undefined ? (
              <button
                type="button"
                role="menuitem"
                className="roster-menu-item roster-menu-danger"
                disabled={busy}
                onClick={() => {
                  close();
                  onBan();
                }}
                title="Remove this person and bar them from returning."
              >
                Ban
              </button>
            ) : null}
          </div>
        </>
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
  onViewProfile,
  onGift,
  onVoteKick,
  onSetRole,
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
  /** Open a member's profile; every row offers it when the opener can show one. */
  onViewProfile?: (targetId: Id) => void;
  /** Hand a member to the gift flow; a viewer never gifts themselves. */
  onGift?: (targetId: Id) => void;
  onVoteKick?: (targetId: Id) => void;
  /**
   * Grant a member a role, offered where the viewer holds the manage permission and outranks the
   * member — the items themselves are the ranks strictly below the viewer's own, as plain wire
   * numbers like the roster carries.
   */
  onSetRole?: (targetId: Id, role: number) => void;
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
        const roles =
          onSetRole !== undefined && canSetRole(viewerRole, entry.role, isSelf)
            ? settableRoles(viewerRole).filter((role) => role.value !== entry.role)
            : undefined;
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
            onViewProfile={onViewProfile ? () => onViewProfile(entry.accountId) : undefined}
            onGift={onGift && !isSelf ? () => onGift(entry.accountId) : undefined}
            onVoteKick={onVoteKick ? () => onVoteKick(entry.accountId) : undefined}
            onSetRole={onSetRole ? (role) => onSetRole(entry.accountId, role) : undefined}
            settableRoles={roles}
            onRoomMute={onRoomMute ? () => onRoomMute(entry.accountId) : undefined}
            onKick={onKick ? () => onKick(entry.accountId) : undefined}
            onBan={onBan ? () => onBan(entry.accountId) : undefined}
          />
        );
      })}
    </div>
  );
}

/**
 * The room's settings section: the rename and the slow-mode interval a staff member may apply, and
 * the archive that ends the room for everybody in it.
 *
 * Presentational, so a test pins what each rank sees: a viewer allowed neither control gets nothing
 * at all, not a disabled husk. The rename covers exactly the fields the settings patch carries that
 * this panel can seed honestly — the name and the topic, both trimmed server-side, the name required,
 * the topic clearable to nothing. The slow-mode interval is the patch's third field, whole seconds on
 * this screen and milliseconds on the wire, zero meaning off; the field is bounded by the server's
 * own ceiling ({@link SLOW_MODE_MAX_SECONDS}) and offered a clear affordance so turning the interval
 * off is a button rather than a remembered convention. The archive confirmation states what the
 * server does and no more: the room stops admitting joins and its settings lock, history stays
 * readable and links keep resolving, and there is no unarchive.
 */
export function RoomSettings({
  name,
  topic,
  slowModeSec,
  canEdit = false,
  canArchive = false,
  saving = false,
  archiving = false,
  onApply,
  onArchive,
}: {
  /** The room's current name, for the field's starting value. */
  name: string;
  /** The room's current topic, when it carries one; the field starts empty without it. */
  topic?: string;
  /** The room's current slow-mode interval in seconds, when one is set; the field starts at zero. */
  slowModeSec?: number;
  /** Show the rename fields — the viewer holds the room's edit permission. */
  canEdit?: boolean;
  /** Show the archive control — the viewer is the room's owner. */
  canArchive?: boolean;
  /** True while the settings patch is in flight. */
  saving?: boolean;
  /** True while the archive is in flight. */
  archiving?: boolean;
  /** Apply the settings; the trimmed name is never empty, the seconds parse, and something moved. */
  onApply: (settings: { name: string; topic: string; slowModeSec: number }) => void;
  /** Archive the room; the owner's confirmation is the opener's to ask for. */
  onArchive: () => void;
}): ReactNode {
  const [nameValue, setNameValue] = useState(name);
  const [topicValue, setTopicValue] = useState(topic ?? '');
  const [slowValue, setSlowValue] = useState(String(slowModeSec ?? 0));
  if (!canEdit && !canArchive) {
    return null;
  }
  const trimmedName = nameValue.trim();
  const trimmedTopic = topicValue.trim();
  // The interval the field names, or null when it names nothing the server would take; the save
  // stays disabled on null rather than sending the field's text to be refused.
  const slowSeconds = parseSlowModeSeconds(slowValue);
  const moved =
    trimmedName !== name || trimmedTopic !== (topic ?? '') || slowSeconds !== (slowModeSec ?? 0);

  function submit(event: FormEvent): void {
    event.preventDefault();
    if (saving || !moved || trimmedName.length === 0 || slowSeconds === null) {
      return;
    }
    onApply({ name: trimmedName, topic: trimmedTopic, slowModeSec: slowSeconds });
  }

  return (
    <>
      {canEdit ? (
        <form className="panel-section" onSubmit={submit} aria-label="Room settings">
          <h3 className="panel-heading">Room Settings</h3>
          <label className="field-label">
            Room name
            <input
              type="text"
              className="input"
              value={nameValue}
              onChange={(event) => setNameValue(event.target.value)}
              maxLength={64}
              aria-label="Room name"
            />
          </label>
          <label className="field-label">
            Topic
            <input
              type="text"
              className="input"
              value={topicValue}
              onChange={(event) => setTopicValue(event.target.value)}
              maxLength={256}
              aria-label="Room topic"
              placeholder="none"
            />
          </label>
          <label className="field-label">
            Slow mode
            <input
              type="number"
              className="input"
              value={slowValue}
              onChange={(event) => setSlowValue(event.target.value)}
              min={0}
              max={SLOW_MODE_MAX_SECONDS}
              step={1}
              aria-label="Slow mode seconds"
              placeholder="0"
              title="How long a member must wait between messages, in seconds. Zero turns slow mode off; the server refuses an interval longer than an hour."
            />
          </label>
          <div className="inline-field">
            <button
              type="button"
              className="btn"
              disabled={saving || slowSeconds === 0}
              onClick={() => setSlowValue('0')}
              title="Clears the interval: slow mode off, the whole room free to answer at its own pace."
            >
              Turn Off
            </button>
            <button
              type="submit"
              className="btn"
              disabled={saving || !moved || trimmedName.length === 0 || slowSeconds === null}
            >
              {saving ? <Spinner /> : 'Save'}
            </button>
          </div>
        </form>
      ) : null}
      {canArchive ? (
        <div className="panel-section">
          <button
            type="button"
            className="btn btn-danger"
            disabled={archiving}
            onClick={onArchive}
            aria-label="Archive room"
            title="Archives the room for everybody: no new joins, and the settings lock. History stays readable and links keep resolving. This cannot be undone."
          >
            {archiving ? <Spinner /> : 'Archive Room'}
          </button>
        </div>
      ) : null}
    </>
  );
}

/** The room details drawer: roster, moderation, the settings, plus the leave control. */
export function RoomInfoPanel({
  roomId,
  conversationId,
  onGift,
}: {
  roomId: Id;
  conversationId: Id;
  /** Hand a member to the opener's gift flow (the chat's picker, pre-aimed at the member). */
  onGift?: (targetId: Id) => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const { forgetConversation } = useConversations();
  const { forgetRoom, liveFor, noteRoom } = useRooms();

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
  // The settings patch and the archive are their own flights, not row actions, so they disable the
  // section's own controls rather than a roster row.
  const [savingSettings, setSavingSettings] = useState(false);
  const [archiving, setArchiving] = useState(false);
  // The member whose profile a row's "View profile" opened, until the modal closes.
  const [profileId, setProfileId] = useState<Id | null>(null);
  // What the report dialog points at, when it is open. The panel reports the room itself and any
  // member it shows a profile for, so one piece of state serves both — only one dialog is open.
  const [reportSubject, setReportSubject] = useState<ReportSubjectRef | null>(null);

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

  // The roster is live membership, not a snapshot the open froze: a member event for this room
  // re-reads the list, debounced so a burst of movement costs one read. Without this a departed
  // member keeps their row — and the panel keeps presenting them as present — for as long as it
  // stays open, the purely-visual twin of the delivery the server already stopped. The read, not
  // a patch, is the honest reaction: a joiner's row needs the role and join time only the roster
  // carries, so even a patching implementation would end in a read for half the events.
  useEffect(() => {
    if (!client) {
      return;
    }
    const refresh = debounce(() => void reload(), MEMBER_EVENT_DEBOUNCE_MS);
    const off = client.rooms.onMember((event) => {
      if (event.roomId !== roomId) {
        return;
      }
      refresh();
    });
    return () => {
      off();
      refresh.cancel();
    };
  }, [client, roomId, reload]);

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

  // The room's held record — the join's own summary plus every state delta since — seeds the rename
  // fields. A room this shell does not know (never joined here, never restored) still offers the
  // controls to a viewer the roster says holds them; the fields start empty and the server remains
  // the authority on what they say.
  const roomRecord = liveFor(roomId);
  // The settings gates, all through the one place the permission question is answered: the
  // viewer's standing, resolved override-first. The roster the standing is built from carries a
  // role and nothing more — the wire has no surface for the per-member overrides the server
  // stores — so the role default is the whole answer today, and a member holding a bit by an
  // override is the server's refusal to tell; it surfaces as the error it returns.
  const myStanding: MemberStanding = { role: myRole };
  const canEdit = effectivePermission(myStanding, 'edit');
  const canArchive = effectivePermission(myStanding, 'archive');

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

  // A settings change is a flight of its own, and its echo is this side's to record: the fan-out
  // excludes the acting socket, and a rename reaches no frame at all — the state event carries a
  // topic and an interval, nothing else — so the record the shell keeps is moved here, at the one
  // place that knows the server accepted the change. The slow-mode interval is the one settings
  // field whose change the state event does carry, so the rest of the room learns it from the
  // delta; the actor's own record still moves here, because the fan-out's exclusion is the actor.
  function applySettings(settings: { name: string; topic: string; slowModeSec: number }): void {
    const active = client;
    if (!active) {
      return;
    }
    setSavingSettings(true);
    setError(null);
    active.rooms
      .update(roomId, {
        name: settings.name,
        topic: settings.topic,
        // The wire carries milliseconds; the record and the screen keep the store's whole seconds.
        slowModeMs: settings.slowModeSec * 1000,
      })
      .then(() => {
        const held = liveFor(roomId);
        if (held !== null) {
          noteRoom(applyRoomSettings(held, settings));
        }
      })
      .catch((cause: unknown) => setError(friendlyError(cause)))
      .finally(() => setSavingSettings(false));
  }

  // Archiving is the room's end for everybody in it, said plainly before the server acts: joins are
  // refused and the settings lock from then on, while history stays readable and links keep
  // resolving — and there is no unarchive, which is the one fact a hesitant owner most needs.
  function archiveRoom(): void {
    const active = client;
    if (!active || archiving) {
      return;
    }
    if (
      !window.confirm(
        'Archive this room for everybody? It stops admitting joins and its settings lock; history stays readable and links keep resolving. There is no unarchive.',
      )
    ) {
      return;
    }
    setArchiving(true);
    setError(null);
    active.rooms
      .archive(roomId)
      .catch((cause: unknown) => setError(friendlyError(cause)))
      .finally(() => setArchiving(false));
  }

  // A role change is not destructive — whoever may set it may set it back — so it fires without a
  // confirm, like the room silence. The roster re-read that follows is what moves the badge. The
  // ladder's numbers are the wire's own discriminants, so they hand straight to the domain.
  function setRole(targetId: Id, role: number): void {
    const active = client;
    if (!active) {
      return;
    }
    withBusy(targetId, () => active.rooms.roleSet(roomId, targetId, role));
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
        // The room's whole client-side life ends in one call: both topics unsubscribed in a
        // single frame (the server revoked them the moment the leave landed; this is the tracked
        // set a later session reset would otherwise re-ask, and be refused), the conversation's
        // crypto state forgotten — a re-join must start fresh chains rather than reuse keys the
        // departed members may still hold, §163 — and the SDK's room-to-conversation bridge
        // dropped. Fire-and-forget: a refusal here is tidying a set the server has already
        // cleaned, not a failure the leaver needs to read.
        void client.teardownRoom(roomId, conversationId).catch(() => {});
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
    // The members variant of the details panel: the chat window stands the transcript and the
    // composer down while this is open, and the panel fills the whole column below the header —
    // the roster is the panel's scrollable body, the head with the leave control stays pinned.
    <div className="room-info room-info-members" aria-label="Room details">
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
              className="btn btn-ghost"
              onClick={() =>
                setReportSubject({
                  kind: ReportSubject.Room,
                  id: roomId,
                  label: roomRecord?.name ? `“${roomRecord.name}”` : 'this room',
                })
              }
              aria-label="Report room"
              title="Reports this room to the node’s moderators. Leaving is a separate control: reporting does not remove you from it."
            >
              ⚑ Report
            </button>
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
          <RoomSettings
            name={roomRecord?.name ?? ''}
            topic={roomRecord?.topic}
            slowModeSec={roomRecord?.slowModeSec}
            canEdit={canEdit}
            canArchive={canArchive}
            saving={savingSettings}
            archiving={archiving}
            onApply={applySettings}
            onArchive={archiveRoom}
          />
          <RosterList
            entries={roster}
            profiles={profiles}
            viewerId={accountId}
            viewerRole={myRole}
            isGlobalAdmin={isGlobalAdmin}
            tallies={tallyLabels}
            busyIds={busyIds}
            onViewProfile={setProfileId}
            onGift={onGift}
            onVoteKick={castVote}
            onSetRole={setRole}
            onRoomMute={silence}
            onKick={kick}
            onBan={ban}
          />
        </>
      )}
      <ReportDialog subject={reportSubject} onClose={() => setReportSubject(null)} />

      {profileId !== null ? (
        <UserProfileModal
          userId={profileId}
          onClose={() => setProfileId(null)}
          onGift={onGift && profileId !== accountId ? () => onGift(profileId) : undefined}
          onReport={(userId, displayName) =>
            setReportSubject({ kind: ReportSubject.User, id: userId, label: displayName })
          }
        />
      ) : null}
    </div>
  );
}
