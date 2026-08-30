'use client';

/**
 * A room's details: the roster and the way out.
 *
 * The roster is paged wire data ({@link RoomsDomain.getRoster}), re-read on open rather than
 * mirrored locally — membership moves with every join and leave, and the room's own events are
 * the freshest statement of it. Roles arrive as plain numbers ({@link RoomRole} values), so the
 * label mapping lives in one pure function the tests pin, comparing number to number the same
 * way the friends panel does.
 *
 * Leaving is a one-way door and says so: `rooms.leave` is called, the conversation is dropped
 * from the shared list ({@link forgetConversation}), and the thread closes — a room the account
 * has left must not linger in the sidebar as a conversation it can no longer open.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { RoomRole } from '@migo/sdk';
import type { Id, RosterEntry } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
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

export function roleLabel(role: number): string {
  if (role === ROLE_OWNER) {
    return 'Owner';
  }
  if (role === ROLE_MANAGER || role === ROLE_ADMIN) {
    return 'Admin';
  }
  return 'Member';
}

/** One roster row: avatar, name, and the role badge. */
export function RosterRow({
  entry,
  name,
  avatarUrl,
}: {
  entry: RosterEntry;
  name: string;
  avatarUrl?: string;
}): ReactNode {
  return (
    <div className="person-row roster-row">
      <Avatar name={name} id={entry.accountId} size={32} avatarUrl={avatarUrl} />
      <div className="person-main">
        <span className="person-name">{name}</span>
        <span className="person-sub">joined {formatRelative(entry.joinedAt)}</span>
      </div>
      <span className={`role-badge role-${roleLabel(entry.role).toLowerCase()}`}>
        {roleLabel(entry.role)}
      </span>
    </div>
  );
}

/** The roster list: one row per member, roles labelled. */
export function RosterList({
  entries,
  profiles,
}: {
  entries: RosterEntry[];
  /** Resolved profiles, for names and avatars; an unresolved member keeps a stable fallback. */
  profiles: ReadonlyMap<Id, { displayName: string; avatarUrl?: string }>;
}): ReactNode {
  if (entries.length === 0) {
    return <p className="muted">No one else is here.</p>;
  }
  return (
    <div className="roster-list">
      {entries.map((entry) => (
        <RosterRow
          key={entry.accountId}
          entry={entry}
          name={profiles.get(entry.accountId)?.displayName ?? 'Someone'}
          avatarUrl={profiles.get(entry.accountId)?.avatarUrl}
        />
      ))}
    </div>
  );
}

/** The room details drawer: roster plus the leave control. */
export function RoomInfoPanel({
  roomId,
  conversationId,
}: {
  roomId: Id;
  conversationId: Id;
}): ReactNode {
  const { client } = useMigo();
  const { forgetConversation } = useConversations();

  const [roster, setRoster] = useState<RosterEntry[] | null>(null);
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

  // The roster's members resolve through the shared profile cache, so names and avatars match
  // every other surface that shows these people.
  const memberIds = (roster ?? []).map((entry) => entry.accountId);
  const profiles = useProfiles(memberIds);

  const leave = useCallback((): void => {
    if (!client || leaving) {
      return;
    }
    setLeaving(true);
    client.rooms
      .leave(roomId)
      .then(() => {
        forgetConversation(conversationId);
        closeConversation();
      })
      .catch((cause: unknown) => {
        setError(friendlyError(cause));
      })
      .finally(() => {
        setLeaving(false);
      });
  }, [client, leaving, roomId, conversationId, forgetConversation]);

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
          <RosterList entries={roster} profiles={profiles} />
        </>
      )}
    </div>
  );
}
