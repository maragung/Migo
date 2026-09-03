'use client';

/**
 * A group's details: the roster, the invite path, the founder controls, the members' vote, and the
 * way out.
 *
 * # Who may do what
 *
 * A group is built by two founders — the creator and the first person they named — and the roster is
 * the only statement of who they are, so the panel reads it before offering anyone a control:
 *
 * - **Invite** is every member's right. The invite section offers both paths the user asked for: the
 *   friends quick-pick (the relationship graph is the shortest path to a person) and a debounced
 *   username search for everyone else — the same two sources a new conversation starts from, so a
 *   person is found the same way everywhere. People already seated read as "In group".
 * - **Rename** and **mute** and a straight **kick** are the founders' controls. A founder cannot
 *   touch the other founder — a group built by two cannot be halved by one of them — and cannot
 *   mute or kick themselves either. A group mute silences the member for the whole group while it
 *   runs (they keep every other right, including the vote); the roster is the record of it, so the
 *   row says so until it expires or a founder lifts it.
 * - **Vote kick** is the members' own recourse, open to everyone, never against yourself and never
 *   against a founder. Half the group rounded up carries it, and the running tally arrives on the
 *   broadcast vote stream so every member watches the same count climb.
 * - **Leave** is nobody's to gate. When the last founder walks out the server quietly hands the
 *   role to the longest-standing member, so the panel never needs to ask anyone to appoint an heir.
 *
 * The roster is wire data re-read on every membership move rather than mirrored locally — the
 * group's own events are the freshest statement of who belongs.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { ConversationRole, RelationshipKind } from '@migo/sdk';
import type {
  ConversationRosterEntry,
  ConversationSummary,
  Id,
  RelationshipEntry,
  SuggestedUser,
} from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { closeConversation } from '@/lib/migo/use-open-conversation.js';
import { SEARCH_DEBOUNCE_MS, searchPeople } from './new-conversation-dialog.js';
import { PersonPickRow } from './new-conversation-dialog.js';
import { voteTally } from './room-info-panel.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

/** The founder role as a plain number, so the gates compare number to number like the room panel's. */
const ROLE_FOUNDER: number = ConversationRole.Founder;

const KIND_FRIEND: number = RelationshipKind.Friend;

/** The mute terms a founder may set, as the row's control labels read them. */
export const MUTE_TERMS: ReadonlyArray<{ label: string; ms: number }> = [
  { label: '1 hour', ms: 60 * 60 * 1000 },
  { label: '1 day', ms: 24 * 60 * 60 * 1000 },
  { label: '7 days', ms: 7 * 24 * 60 * 60 * 1000 },
];

/**
 * The label a group role renders as, from the plain number the wire carries.
 *
 * Pure, so a test pins it. An unknown value from a newer node renders as Member rather than a guess.
 */
export function groupRoleLabel(role: number): string {
  return role === ROLE_FOUNDER ? 'Founder' : 'Member';
}

/**
 * Whether the founder controls (mute, kick) belong on a member's row for this viewer.
 *
 * Pure, so a test pins it. The viewer must be a founder, the target must not be (the two builders
 * are beyond each other's reach), and nobody acts on their own row.
 */
export function canFounderAct(viewerRole: number, targetRole: number, isSelf: boolean): boolean {
  return !isSelf && viewerRole === ROLE_FOUNDER && targetRole !== ROLE_FOUNDER;
}

/**
 * Whether the "Vote kick" control belongs on a member's row.
 *
 * Pure, so a test pins it. The vote is every member's own recourse — but never against yourself,
 * and never against a founder, whom a show of hands cannot unseat.
 */
export function canVoteKickGroup(targetRole: number, isSelf: boolean): boolean {
  return !isSelf && targetRole !== ROLE_FOUNDER;
}

/** One roster row: avatar, name, the role badge, any running mute, and the actions on the member. */
export function GroupRosterRow({
  entry,
  name,
  avatarUrl,
  now,
  tally,
  canVote = false,
  canFound = false,
  busy = false,
  onVoteKick,
  onMute,
  onUnmute,
  onKick,
}: {
  entry: ConversationRosterEntry;
  name: string;
  avatarUrl?: string;
  /** The clock the mute line reads against, passed in so a test can pin the label. */
  now: number;
  /** The live kick-vote tally against this member ("3/5"), when a vote is open. */
  tally?: string;
  /** Show the "Vote kick" control. */
  canVote?: boolean;
  /** Show the founder controls (mute terms, unmute, kick). */
  canFound?: boolean;
  /** True while an action on this row is in flight, so its controls disable together. */
  busy?: boolean;
  onVoteKick?: () => void;
  onMute?: (ms: number) => void;
  onUnmute?: () => void;
  onKick?: () => void;
}): ReactNode {
  const mutedUntil = entry.mutedUntil;
  const muted = mutedUntil !== undefined && mutedUntil > now;
  const departed = entry.leftAt !== undefined;
  return (
    <div className={`person-row roster-row${departed ? ' departed' : ''}`}>
      <Avatar name={name} id={entry.accountId} size={32} avatarUrl={avatarUrl} />
      <div className="person-main">
        <span className="person-name">{name}</span>
        <span className="person-sub">joined {formatRelative(entry.joinedAt)}</span>
        {muted && mutedUntil !== undefined ? (
          <span className="person-note">Muted until {formatRelative(mutedUntil)}</span>
        ) : null}
        {tally !== undefined ? (
          <span className="person-note vote-tally">Vote to kick: {tally}</span>
        ) : null}
      </div>
      <span className={`role-badge role-${groupRoleLabel(entry.role).toLowerCase()}`}>
        {groupRoleLabel(entry.role)}
      </span>
      {!departed && (canVote || canFound) ? (
        <div className="person-actions roster-actions">
          {canVote && onVoteKick !== undefined ? (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={onVoteKick}
              title="Call a vote to remove this person. When half the group agrees, they are kicked."
            >
              Vote kick
            </button>
          ) : null}
          {canFound && muted && onUnmute !== undefined ? (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={onUnmute}
              title="Lift this group mute now."
            >
              Unmute
            </button>
          ) : null}
          {canFound && !muted && onMute !== undefined
            ? MUTE_TERMS.map((term) => (
                <button
                  key={term.label}
                  type="button"
                  className="btn btn-ghost"
                  disabled={busy}
                  onClick={() => onMute(term.ms)}
                  title={`Silence this person for the whole group for ${term.label}. They keep every other right, including the vote.`}
                >
                  Mute {term.label}
                </button>
              ))
            : null}
          {canFound && onKick !== undefined ? (
            <button
              type="button"
              className="btn btn-danger"
              disabled={busy}
              onClick={onKick}
              title="Remove this person outright, no vote — a founder's call."
            >
              Kick
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/**
 * The group details drawer: invite, roster with the moderation controls, the rename, and the leave.
 */
export function GroupInfoPanel({
  conversationId,
  title,
}: {
  conversationId: Id;
  /** The group's current title, for the rename field's starting value. */
  title: string;
}): ReactNode {
  const { client, accountId } = useMigo();
  const { forgetConversation, noteConversation } = useConversations();

  const [roster, setRoster] = useState<ConversationRosterEntry[] | null>(null);
  const [tallies, setTallies] = useState<ReadonlyMap<Id, { votes: number; needed: number }>>(
    new Map(),
  );
  const [busyIds, setBusyIds] = useState<ReadonlySet<Id>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [leaving, setLeaving] = useState(false);
  const [renameValue, setRenameValue] = useState(title);
  const [renaming, setRenaming] = useState(false);

  // The invite section's own state: the friends quick-pick, the debounced username search, and the
  // seats already taken.
  const [friends, setFriends] = useState<RelationshipEntry[]>([]);
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SuggestedUser[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [invitingIds, setInvitingIds] = useState<ReadonlySet<Id>>(new Set());

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      setRoster(await client.conversations.getRoster(conversationId));
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client, conversationId]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // The friends quick-pick loads once; it is the graph the friends panel already knows.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.social
      .listRelationships()
      .then((entries) => {
        if (!cancelled) {
          setFriends(entries.filter((entry) => entry.kind === KIND_FRIEND));
        }
      })
      .catch(() => {
        // A failed quick-pick leaves the search as the working path.
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  // The debounced username search: 300ms of quiet, then one query — never a request per keystroke.
  useEffect(() => {
    const text = query.trim();
    if (!client || text.length === 0) {
      setResults(null);
      setSearching(false);
      return;
    }
    setSearching(true);
    const timer = setTimeout(() => {
      searchPeople(client, text)
        .then((found) => {
          setResults(found);
          setError(null);
        })
        .catch((cause: unknown) => {
          setError(friendlyError(cause));
        })
        .finally(() => {
          setSearching(false);
        });
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [client, query]);

  // The live kick-vote tallies: the broadcast stream keeps every member's count in step. A closed
  // vote drops from the map and re-reads the roster, since a vote that passed just removed someone
  // the list must stop showing as seated.
  useEffect(() => {
    if (!client) {
      return;
    }
    return client.conversations.onVote((event) => {
      if (event.conversationId !== conversationId) {
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
  }, [client, conversationId, reload]);

  // Membership moves: the roster is re-read, because the wire's member event is the freshest
  // statement of who belongs. (The thread window owns closing on this account's own removal.)
  useEffect(() => {
    if (!client) {
      return;
    }
    return client.conversations.onMember((event) => {
      if (event.conversationId !== conversationId) {
        return;
      }
      void reload();
    });
  }, [client, conversationId, reload]);

  // The roster's members, the friends, and any search results all resolve through the shared
  // profile cache, so names and avatars match every other surface that shows these people.
  const profileIds = useMemo(() => {
    const ids: Id[] = [];
    const seen = new Set<Id>();
    const push = (id: Id): void => {
      if (!seen.has(id)) {
        seen.add(id);
        ids.push(id);
      }
    };
    for (const entry of roster ?? []) {
      push(entry.accountId);
    }
    for (const entry of friends) {
      push(entry.userId);
    }
    for (const person of results ?? []) {
      push(person.accountId);
    }
    return ids;
  }, [roster, friends, results]);
  const profiles = useProfiles(profileIds);

  const seated = useMemo(
    () =>
      new Set(
        (roster ?? [])
          .filter((entry) => entry.leftAt === undefined)
          .map((entry) => entry.accountId),
      ),
    [roster],
  );

  // The viewer's own role, from their roster row; unknown — or not on the roster at all — is a
  // plain member, who sees no founder controls.
  const myRole: number =
    (accountId !== null
      ? (roster ?? []).find((entry) => entry.accountId === accountId)?.role
      : undefined) ?? 0;
  const isFounder = myRole === ROLE_FOUNDER;

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
    active.conversations
      .voteKick(conversationId, targetId)
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

  // A group mute quiets the person for the whole group — deliberately not the personal mute, so the
  // control says so in its tooltip. It is not destructive, so it fires without a confirm.
  function muteFor(targetId: Id, ms: number): void {
    const active = client;
    if (!active) {
      return;
    }
    withBusy(targetId, () => active.conversations.mute(conversationId, targetId, Date.now() + ms));
  }

  function unmute(targetId: Id): void {
    const active = client;
    if (!active) {
      return;
    }
    withBusy(targetId, () => active.conversations.mute(conversationId, targetId));
  }

  // A founder's kick removes a person outright, so the member is named before the server acts.
  function kickPerson(targetId: Id): void {
    const active = client;
    if (!active) {
      return;
    }
    if (!window.confirm(`Kick ${nameOf(targetId)} from the group? A founder's call, no vote.`)) {
      return;
    }
    withBusy(targetId, () => active.conversations.kick(conversationId, targetId));
  }

  /** Invites one person, then records the reply's summary so the member list is true at once. */
  function invite(personId: Id): void {
    const active = client;
    if (!active) {
      return;
    }
    setInvitingIds((prev) => new Set(prev).add(personId));
    setError(null);
    active.conversations
      .invite(conversationId, [personId])
      .then((summary: ConversationSummary) => {
        noteConversation(summary);
        setItemsFromSummary(summary);
      })
      .catch((cause: unknown) => setError(friendlyError(cause)))
      .finally(() => {
        setInvitingIds((prev) => {
          const next = new Set(prev);
          next.delete(personId);
          return next;
        });
      });
  }

  // The invite reply returns the group's summary; the roster re-read follows, but the roster rows
  // the user is looking at should name the newcomer at once, so the reply seeds them.
  function setItemsFromSummary(summary: ConversationSummary): void {
    setRoster((prev) => {
      if (prev === null || summary.members === undefined) {
        return prev;
      }
      const byId = new Map(prev.map((entry) => [entry.accountId, entry]));
      const next: ConversationRosterEntry[] = [];
      for (const id of summary.members) {
        const held = byId.get(id);
        next.push(
          held ?? {
            accountId: id,
            role: ConversationRole.Member,
            joinedAt: Date.now(),
          },
        );
      }
      return next;
    });
  }

  // Leaving is a one-way door and says so: the conversation is dropped from the shared list and the
  // thread closes — a group the account has left must not linger as a conversation it can open.
  const leave = useCallback((): void => {
    if (!client || leaving) {
      return;
    }
    setLeaving(true);
    client.conversations
      .leave(conversationId)
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
  }, [client, leaving, conversationId, forgetConversation]);

  function submitRename(event: FormEvent): void {
    event.preventDefault();
    const active = client;
    const trimmed = renameValue.trim();
    if (!active || renaming || trimmed.length === 0 || trimmed === title) {
      return;
    }
    setRenaming(true);
    setError(null);
    active.conversations
      .rename(conversationId, trimmed)
      .then((summary: ConversationSummary) => {
        noteConversation(summary);
        // The coalesced state event also carries the title onto every member's row; this simply
        // keeps the field in step with what the server just accepted.
        setRenameValue(summary.title ?? trimmed);
      })
      .catch((cause: unknown) => setError(friendlyError(cause)))
      .finally(() => setRenaming(false));
  }

  const now = Date.now();

  return (
    <div className="room-info" aria-label="Group details">
      {error ? <p className="form-error">{error}</p> : null}

      <div className="panel-head">
        <h2 className="panel-heading">Group</h2>
        <button
          type="button"
          className="btn btn-danger"
          disabled={leaving}
          onClick={leave}
          aria-label="Leave group"
        >
          {leaving ? <Spinner /> : 'Leave Group'}
        </button>
      </div>

      {isFounder ? (
        <form className="panel-section" onSubmit={submitRename}>
          <h3 className="panel-heading">Rename</h3>
          <div className="inline-field">
            <input
              type="text"
              className="input"
              value={renameValue}
              onChange={(event) => setRenameValue(event.target.value)}
              maxLength={120}
              aria-label="Group title"
              placeholder={title}
            />
            <button
              type="submit"
              className="btn"
              disabled={renaming || renameValue.trim().length === 0 || renameValue.trim() === title}
            >
              {renaming ? <Spinner /> : 'Save'}
            </button>
          </div>
        </form>
      ) : null}

      <div className="panel-section" aria-label="Invite to group">
        <h3 className="panel-heading">Invite</h3>
        <label className="field-label">
          Search by username
          <input
            type="search"
            className="input"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="ada"
            aria-label="Search people by username"
          />
        </label>
        {searching ? (
          <p className="hint">
            <Spinner /> Searching…
          </p>
        ) : null}
        {friends.filter((entry) => !seated.has(entry.userId)).length > 0 ? (
          <div aria-label="Friends">
            {friends
              .filter((entry) => !seated.has(entry.userId))
              .map((entry) => (
                <PersonPickRow
                  key={entry.userId}
                  accountId={entry.userId}
                  displayName={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                  username={profiles.get(entry.userId)?.username}
                  note="Friend"
                  picked={invitingIds.has(entry.userId)}
                  onPick={invite}
                />
              ))}
          </div>
        ) : null}
        {results !== null ? (
          <div className="panel-section" aria-label="Search results">
            <h3 className="panel-heading">Search results</h3>
            {results.length === 0 ? (
              <p className="muted">No one found for “{query.trim()}”.</p>
            ) : (
              results.map((person) => (
                <PersonPickRow
                  key={person.accountId}
                  accountId={person.accountId}
                  displayName={person.displayName}
                  username={person.username}
                  note={
                    seated.has(person.accountId)
                      ? 'In group'
                      : person.mutualFriends > 0
                        ? `${person.mutualFriends} mutual friends`
                        : undefined
                  }
                  picked={seated.has(person.accountId) || invitingIds.has(person.accountId)}
                  onPick={invite}
                />
              ))
            )}
          </div>
        ) : null}
      </div>

      {roster === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : (
        <div className="panel-section" aria-label="Members">
          <h3 className="panel-heading">
            Members ({roster.filter((entry) => entry.leftAt === undefined).length})
          </h3>
          {roster.length === 0 ? (
            <p className="muted">No one is here.</p>
          ) : (
            <div className="roster-list">
              {roster.map((entry) => {
                const isSelf = accountId !== null && entry.accountId === accountId;
                const canVote = canVoteKickGroup(entry.role, isSelf);
                const canFound = canFounderAct(myRole, entry.role, isSelf);
                return (
                  <GroupRosterRow
                    key={entry.accountId}
                    entry={entry}
                    name={profiles.get(entry.accountId)?.displayName ?? 'Someone'}
                    avatarUrl={profiles.get(entry.accountId)?.avatarUrl}
                    now={now}
                    tally={tallyLabels.get(entry.accountId)}
                    canVote={canVote}
                    canFound={canFound}
                    busy={busyIds.has(entry.accountId)}
                    onVoteKick={canVote ? () => castVote(entry.accountId) : undefined}
                    onMute={canFound ? (ms) => muteFor(entry.accountId, ms) : undefined}
                    onUnmute={canFound ? () => unmute(entry.accountId) : undefined}
                    onKick={canFound ? () => kickPerson(entry.accountId) : undefined}
                  />
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
