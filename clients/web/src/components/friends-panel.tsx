'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, KeyboardEvent, ReactNode } from 'react';

import { ConversationKind, PresenceState, RelationshipKind } from '@migo/sdk';
import type {
  Id,
  PresenceState as PresenceStateValue,
  RelationshipEntry,
  SuggestedUser,
} from '@migo/sdk';

import { useConversations } from '@/lib/migo/conversations-provider.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { presenceLabel, usePresenceOf } from '@/lib/migo/use-presence.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { ConnectionBadge } from './connection-badge.js';
import { ContextMenu } from './context-menu.js';
import { useContextMenu } from './context-menu.js';
import type { ContextAction } from './context-menu.js';
import { Icon } from './icons.js';
import { NewConversationDialog } from './new-conversation-dialog.js';
import { PresencePicker } from './presence-picker.js';
import { Spinner } from './spinner.js';
import { UserProfileModal } from './user-profile-modal.js';

/**
 * The relationship kinds this panel files people under, as the plain numbers the wire carries.
 *
 * `RelationshipEntry.kind` is a `number` (a newer server may send a value this build has no name
 * for), and comparing a number against an enum member directly trips the workspace's
 * unsafe-enum-comparison rule — so the enum's numeric values are read into number-typed constants
 * once, and the section filters compare number to number. A kind that matches none of them is
 * simply not rendered, never misfiled.
 */
const KIND_FRIEND: number = RelationshipKind.Friend;
const KIND_PENDING_INCOMING: number = RelationshipKind.PendingIncoming;
const KIND_PENDING_OUTGOING: number = RelationshipKind.PendingOutgoing;
const KIND_BLOCK: number = RelationshipKind.Block;

/**
 * The Friends tab: the relationship graph, pending requests, suggestions, people search, and the
 * block list.
 *
 * The graph is server-owned — every mutation here asks the server and re-reads the result, because a
 * local mirror would drift the moment either party acted from another device. {@link
 * SocialDomain.onFriendEvent} is the signal to re-read: it says the graph moved, not how, so the
 * panel refreshes both the relationships and the suggestions (a new friend changes what is
 * suggested) rather than patching local state.
 *
 * The full graph ({@link SocialDomain.listAllRelationships}) is what feeds the Blocked section:
 * the bounded read is the panel's working list, but blocks live outside its default page, so the
 * two reads happen together on every refresh.
 *
 * A friend row is a door: clicking it opens that person's profile modal, where blocking (and
 * messaging) live — the list rows stay clean of per-row block controls on purpose.
 */
export function FriendsPanel({
  onOpenConversation,
}: {
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client } = useMigo();
  const { noteConversation } = useConversations();

  const [entries, setEntries] = useState<RelationshipEntry[] | null>(null);
  const [blocked, setBlocked] = useState<RelationshipEntry[]>([]);
  const [suggestions, setSuggestions] = useState<SuggestedUser[]>([]);
  const [results, setResults] = useState<SuggestedUser[] | null>(null);
  const [query, setQuery] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<ReadonlySet<Id>>(new Set());
  // The person whose profile modal is open, if any.
  const [selected, setSelected] = useState<Id | null>(null);
  // The account's own presence and mood, published from the picker the sidebar used to own.
  const [myPresence, setMyPresence] = useState<PresenceStateValue>(PresenceState.Online);
  const [myStatus, setMyStatus] = useState('');
  // The New-conversation dialog, formerly the sidebar header's plus button.
  const [dialogOpen, setDialogOpen] = useState(false);

  // The account's own presence is a publish, not a read: the picker holds the state and the
  // panel performs the call, exactly the posture the sidebar kept.
  const onPresenceChange = useCallback(
    (state: PresenceStateValue, nextStatus: string): void => {
      if (!client) {
        return;
      }
      setMyPresence(state);
      setMyStatus(nextStatus);
      void client.presence
        .setPresence(state, nextStatus.trim().length > 0 ? { customStatus: nextStatus } : {})
        .catch(() => {});
    },
    [client],
  );

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const [relationships, all, suggested] = await Promise.all([
        client.social.listRelationships(),
        client.social.listAllRelationships(),
        client.social.suggestions(),
      ]);
      setEntries(relationships);
      setBlocked(all.filter((entry) => entry.kind === KIND_BLOCK));
      setSuggestions(suggested);
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // A friend event means the graph changed under us; re-read rather than guess the shape of change.
  useEffect(() => {
    if (!client) {
      return;
    }
    return client.social.onFriendEvent(() => {
      void reload();
    });
  }, [client, reload]);

  // The Message action: an existing direct conversation opens; otherwise one is created. The
  // created summary is noted into the shared list so the chats shell can open it like any other.
  const startDirect = useCallback(
    async (userId: Id): Promise<void> => {
      if (!client) {
        return;
      }
      try {
        const summary = await client.conversations.create(ConversationKind.Direct, [userId]);
        noteConversation(summary);
        onOpenConversation(summary.conversationId);
      } catch (cause) {
        setError(friendlyError(cause));
      }
    },
    [client, noteConversation, onOpenConversation],
  );

  // One stable action per button, so `act` can disable a single person's row while it is in flight.
  const request = useCallback(
    (userId: Id): Promise<void> =>
      client ? client.social.friendRequest(userId) : Promise.resolve(),
    [client],
  );
  const respond = useCallback(
    (userId: Id, accept: boolean): Promise<void> =>
      client ? client.social.friendRespond(userId, accept) : Promise.resolve(),
    [client],
  );

  /** Runs one social action for a user, disabling that user's buttons until it settles. */
  async function act(userId: Id, action: () => Promise<void>): Promise<void> {
    setBusy((prev) => new Set(prev).add(userId));
    try {
      await action();
      await reload();
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setBusy((prev) => {
        const next = new Set(prev);
        next.delete(userId);
        return next;
      });
    }
  }

  async function onSearch(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const text = query.trim();
    if (!client || text.length === 0) {
      return;
    }
    try {
      setResults(await client.social.search(text, 20));
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }

  const { friends, incoming, outgoing } = useMemo(() => {
    const list = entries ?? [];
    return {
      friends: list.filter((entry) => entry.kind === KIND_FRIEND),
      incoming: list.filter((entry) => entry.kind === KIND_PENDING_INCOMING),
      outgoing: list.filter((entry) => entry.kind === KIND_PENDING_OUTGOING),
    };
  }, [entries]);

  // Resolve the relationship rows to names once, through the shared profile cache.
  const relatedIds = useMemo(
    () => [...friends, ...incoming, ...outgoing, ...blocked].map((entry) => entry.userId),
    [friends, incoming, outgoing, blocked],
  );
  const profiles = useProfiles(relatedIds);

  // Presence everywhere the spec asks for it: seeded from the fetched profiles, live through
  // each friend's user topic. Only the friends are watched — a pending request has no presence
  // worth showing, and a block is exactly the account whose whereabouts this client must stop
  // asking about.
  const presence = usePresenceOf(
    useMemo(() => friends.map((entry) => entry.userId), [friends]),
    profiles,
  );

  // A block from the open modal is the panel's graph moving: run it as a busy action, then close.
  const blockFromModal = useCallback(
    async (userId: Id): Promise<void> => {
      if (!client) {
        return;
      }
      await act(userId, () => client.social.blockUser(userId));
      setSelected(null);
    },
    // `act` is a stable-shape closure over state setters only; the client is the live dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [client],
  );

  return (
    <div className="panel">
      {/* The account's ambient controls lead the panel, directly under the profile banner and
          above the lists: the connection line, the presence/status picker (the account's own
          state, published not read), and the people-search + New-conversation affordances that
          every section below them starts from. The headings then name what follows. */}
      <ConnectionBadge />
      <PresencePicker state={myPresence} status={myStatus} onChange={onPresenceChange} />

      <form className="panel-search" role="search" onSubmit={(event) => void onSearch(event)}>
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
        <button
          type="button"
          className="btn btn-ghost"
          onClick={() => setDialogOpen(true)}
          aria-label="New conversation"
          title="New conversation"
        >
          <Icon name="plus" size={16} />
          <span>New chat</span>
        </button>
      </form>

      <h1 className="panel-title">Friends</h1>

      {error ? <p className="form-error">{error}</p> : null}

      {entries === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : (
        <>
          {/* The contact list leads: friends, presence-first, before anything administrative. */}
          <section className="panel-section" aria-label="Your friends">
            <h2 className="panel-heading">Friends</h2>
            {friends.length === 0 ? (
              <p className="muted">No friends yet. Add someone from the suggestions below.</p>
            ) : (
              friends.map((entry) => (
                <PersonRow
                  key={entry.userId}
                  id={entry.userId}
                  name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                  username={profiles.get(entry.userId)?.username}
                  avatarUrl={profiles.get(entry.userId)?.avatarUrl}
                  note={
                    profiles.get(entry.userId)?.customStatus ??
                    presenceLabel(presence.get(entry.userId))
                  }
                  presence={presence.get(entry.userId)}
                  onSelect={() => setSelected(entry.userId)}
                  onMessage={() => void startDirect(entry.userId)}
                />
              ))
            )}
          </section>

          <section className="panel-section" aria-label="Friend requests">
            <h2 className="panel-heading">Requests</h2>
            {incoming.length === 0 && outgoing.length === 0 ? (
              <p className="muted">No pending requests.</p>
            ) : (
              <>
                {incoming.map((entry) => (
                  <PersonRow
                    key={entry.userId}
                    id={entry.userId}
                    name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                    username={profiles.get(entry.userId)?.username}
                    avatarUrl={profiles.get(entry.userId)?.avatarUrl}
                    note="wants to be friends"
                    actions={
                      <>
                        <button
                          type="button"
                          className="btn btn-primary"
                          disabled={busy.has(entry.userId)}
                          onClick={() => void act(entry.userId, () => respond(entry.userId, true))}
                        >
                          Accept
                        </button>
                        <button
                          type="button"
                          className="btn btn-ghost"
                          disabled={busy.has(entry.userId)}
                          onClick={() => void act(entry.userId, () => respond(entry.userId, false))}
                        >
                          Decline
                        </button>
                      </>
                    }
                  />
                ))}
                {outgoing.map((entry) => (
                  <PersonRow
                    key={entry.userId}
                    id={entry.userId}
                    name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                    username={profiles.get(entry.userId)?.username}
                    avatarUrl={profiles.get(entry.userId)?.avatarUrl}
                    note="request sent"
                  />
                ))}
              </>
            )}
          </section>

          <BlockedSection
            entries={blocked}
            profiles={profiles}
            onSelect={(userId) => setSelected(userId)}
          />

          {results !== null ? (
            <section className="panel-section" aria-label="Search results">
              <h2 className="panel-heading">Search results</h2>
              {results.length === 0 ? (
                <p className="muted">No one found for “{query.trim()}”.</p>
              ) : (
                results.map((person) => (
                  <PersonRow
                    key={person.accountId}
                    id={person.accountId}
                    name={person.displayName}
                    username={person.username}
                    note={mutualNote(person)}
                    actions={
                      <button
                        type="button"
                        className="btn btn-primary"
                        disabled={busy.has(person.accountId)}
                        onClick={() => void act(person.accountId, () => request(person.accountId))}
                      >
                        Add friend
                      </button>
                    }
                  />
                ))
              )}
            </section>
          ) : null}

          <section className="panel-section" aria-label="Suggested friends">
            <h2 className="panel-heading">Suggestions</h2>
            {suggestions.length === 0 ? (
              <p className="muted">No suggestions right now.</p>
            ) : (
              suggestions.map((person) => (
                <PersonRow
                  key={person.accountId}
                  id={person.accountId}
                  name={person.displayName}
                  username={person.username}
                  note={mutualNote(person)}
                  actions={
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={busy.has(person.accountId)}
                      onClick={() => void act(person.accountId, () => request(person.accountId))}
                    >
                      Add friend
                    </button>
                  }
                />
              ))
            )}
          </section>
        </>
      )}

      {selected !== null ? (
        <UserProfileModal
          userId={selected}
          blocked={blocked.some((entry) => entry.userId === selected)}
          onClose={() => setSelected(null)}
          onBlock={blockFromModal}
          onMessage={(userId) => {
            setSelected(null);
            void startDirect(userId);
          }}
        />
      ) : null}

      {dialogOpen ? <NewConversationDialog onClose={() => setDialogOpen(false)} /> : null}
    </div>
  );
}

/** The mutual-friends line under a suggested person, omitted when the count is zero. */
function mutualNote(person: SuggestedUser): string | undefined {
  return person.mutualFriends > 0 ? `${person.mutualFriends} mutual friends` : undefined;
}

/**
 * The Blocked section: the block list the whole-graph read surfaced, each row a door to the
 * person's profile (where the block state is stated).
 *
 * Exported presentational over plain data, so the section's rules — an honest empty state, one
 * row per blocked account, every row opening the profile — are testable without a live client.
 */
export function BlockedSection({
  entries,
  profiles,
  onSelect,
}: {
  entries: RelationshipEntry[];
  /** Resolved profiles through the shared cache; an unresolved account keeps a stable fallback. */
  profiles: ReadonlyMap<Id, { displayName: string; username?: string; avatarUrl?: string }>;
  onSelect: (userId: Id) => void;
}): ReactNode {
  return (
    <section className="panel-section" aria-label="Blocked accounts">
      <h2 className="panel-heading">Blocked</h2>
      {entries.length === 0 ? (
        <p className="muted">No blocked accounts.</p>
      ) : (
        entries.map((entry) => (
          <PersonRow
            key={entry.userId}
            id={entry.userId}
            name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
            username={profiles.get(entry.userId)?.username}
            avatarUrl={profiles.get(entry.userId)?.avatarUrl}
            note="blocked"
            onSelect={() => onSelect(entry.userId)}
          />
        ))
      )}
    </section>
  );
}

interface PersonRowProps {
  id: Id;
  name: string;
  username?: string;
  note?: string;
  /**
   * The person's avatar URL, when a resolved profile is available. Only the relationship rows
   * have one — the wire's suggestions and search results carry no avatar, so those rows keep
   * their initials.
   */
  avatarUrl?: string;
  actions?: ReactNode;
  /** Opens this person's profile; rows without it (requests, results) are not doors. */
  onSelect?: () => void;
  /** Starts (or opens) a direct conversation with the person; offered where a DM makes sense. */
  onMessage?: () => void;
  /** The person's presence, drawn on the avatar — the messenger's ambient information. */
  presence?: PresenceState;
}

/**
 * One person in a list: avatar, name, @username, an optional note, and optional actions.
 *
 * A row with both a profile and a message affordance also carries the context menu — right-click
 * on desktop, long-press on touch — with the same actions the row's own controls offer. A tap
 * still opens the profile; the long-press that opens the menu suppresses the tap that follows it.
 */
function PersonRow({
  id,
  name,
  username,
  note,
  avatarUrl,
  actions,
  onSelect,
  onMessage,
  presence,
}: PersonRowProps): ReactNode {
  const [menu, setMenu] = useState<{ x: number; y: number; touch: boolean } | null>(null);
  const suppressClick = useRef(false);
  const gestures = useContextMenu((at) => {
    suppressClick.current = at.touch;
    setMenu(at);
  });

  const contextActions: ContextAction[] = [];
  if (onSelect !== undefined) {
    contextActions.push({ id: 'profile', label: 'View profile', icon: 'user', onRun: onSelect });
  }
  if (onMessage !== undefined) {
    contextActions.push({ id: 'message', label: 'Message', icon: 'chats', onRun: onMessage });
  }

  return (
    <div
      className={`person-row ${onSelect ? 'person-row-clickable' : ''}`}
      {...(onSelect
        ? {
            role: 'button',
            tabIndex: 0,
            'aria-label': `View ${name}'s profile`,
            onClick: () => {
              if (suppressClick.current) {
                suppressClick.current = false;
                return;
              }
              onSelect();
            },
            onKeyDown: (event: KeyboardEvent<HTMLDivElement>) => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                onSelect();
              }
            },
          }
        : {})}
      {...(contextActions.length > 0
        ? {
            onPointerDown: gestures.onPointerDown,
            onPointerMove: gestures.onPointerMove,
            onPointerUp: gestures.onPointerUp,
            onPointerCancel: gestures.onPointerCancel,
            onContextMenu: gestures.onContextMenu,
          }
        : {})}
    >
      <Avatar name={name} id={id} size={36} avatarUrl={avatarUrl} presence={presence} />
      <div className="person-main">
        <span className="person-name">{name}</span>
        {username ? <span className="person-sub">@{username}</span> : null}
        {note ? <span className="person-note">{note}</span> : null}
      </div>
      {actions ? <div className="person-actions">{actions}</div> : null}
      {menu !== null && contextActions.length > 0 ? (
        <ContextMenu
          at={menu}
          title={name}
          actions={contextActions}
          onClose={() => setMenu(null)}
        />
      ) : null}
    </div>
  );
}
