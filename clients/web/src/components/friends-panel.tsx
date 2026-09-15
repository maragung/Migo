'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, KeyboardEvent, ReactNode } from 'react';

import { ConversationKind, PresenceState, RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry, SuggestedUser } from '@migo/sdk';

import { debounce } from '@/lib/debounce.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMuted } from '@/lib/migo/muted-provider.js';
import { presenceLabel, usePresenceOf } from '@/lib/migo/use-presence.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { ContextMenu } from './context-menu.js';
import { useContextMenu } from './context-menu.js';
import type { ContextAction } from './context-menu.js';
import { Icon } from './icons.js';
import { NewConversationDialog } from './new-conversation-dialog.js';
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
const KIND_MUTE: number = RelationshipKind.Mute;
/**
 * How long a friend-event re-read waits for the events to stop arriving — the same quiet window
 * the search's debounce uses, sized for a burst of echoes rather than a typing rhythm.
 */
const FRIEND_EVENT_DEBOUNCE_MS = 300;

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
 * messaging) live — the list rows stay clean of per-row block controls on purpose. Un-friending
 * is the friendship's own affair, not the person's, so it is the one control the row itself
 * carries — behind a confirm, since one tap ends the friendship on both sides at once — and the
 * Blocked section's rows carry their own Unblock beside the door, the same bargain the Muted
 * section's Unmute makes.
 */
export function FriendsPanel({
  onOpenConversation,
}: {
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client } = useMigo();
  const { noteConversation } = useConversations();
  // The muted set is the provider's to own, so every surface that mutes (a roster, a profile
  // modal, this panel) shares one source of truth; the panel renders it and offers Unmute.
  const { muted: mutedSet, setMuted } = useMuted();

  const [entries, setEntries] = useState<RelationshipEntry[] | null>(null);
  const [blocked, setBlocked] = useState<RelationshipEntry[]>([]);
  const [suggestions, setSuggestions] = useState<SuggestedUser[]>([]);
  const [results, setResults] = useState<SuggestedUser[] | null>(null);
  const [query, setQuery] = useState('');
  // The header's search is collapsed until its icon is tapped: the field takes the icon's place
  // only while a search is actually being made, and leaves again when it is finished.
  const [searchOpen, setSearchOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<ReadonlySet<Id>>(new Set());
  // The person whose profile modal is open, if any.
  const [selected, setSelected] = useState<Id | null>(null);
  // Which list the panel is showing: the friends themselves, the username search, or one of
  // the three the header's right-aligned icons switch to. The counts on those icons come
  // from the same reads.
  const [view, setView] = useState<'friends' | 'search' | 'requests' | 'blocked' | 'suggestions'>(
    'friends',
  );
  // The New-conversation dialog, formerly the sidebar header's plus button.
  const [dialogOpen, setDialogOpen] = useState(false);

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

  // A friend event means the graph changed under us; re-read rather than guess the shape of
  // change — debounced, because one acceptance can arrive as several events (the request's
  // removal and the friendship's arrival, each echoed per device), and the graph read the burst
  // would have triggered per event answers the same question the last event asks. The last event
  // wins the read: it is the freshest word on what moved.
  useEffect(() => {
    if (!client) {
      return;
    }
    const reRead = debounce(() => void reload(), FRIEND_EVENT_DEBOUNCE_MS);
    const off = client.social.onFriendEvent(reRead);
    return () => {
      off();
      reRead.cancel();
    };
  }, [client, reload]);

  // The Message action: an existing direct conversation opens; otherwise one is created. The
  // created summary is noted into the shared list so the chats shell can open it like any other.
  const startDirect = useCallback(
    async (userId: Id): Promise<void> => {
      if (!client) {
        return;
      }
      try {
        // startConversation, not a bare create: it caches the membership the first send needs
        // and subscribes the topic so the peer's replies arrive.
        const summary = await client.startConversation(ConversationKind.Direct, [userId]);
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
    if (!client) {
      return;
    }
    // An emptied field is a return to the list, not a search for nothing: clearing the words and
    // submitting is how a person says "show me my friends again" from the revealed field.
    if (text.length === 0) {
      setResults(null);
      setView('friends');
      return;
    }
    try {
      setResults(await client.social.search(text, 20));
      setView('search');
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }

  // The search is finished when its field is dismissed — the field's own close control, or a
  // blur while the query is empty — and finishing puts the list back: a dismissed search leaves
  // no results on screen with no field left to change them.
  const dismissSearch = useCallback((): void => {
    setSearchOpen(false);
    setQuery('');
    if (view === 'search') {
      setResults(null);
      setView('friends');
    }
  }, [view]);

  const { friends, incoming, outgoing } = useMemo(() => {
    const list = entries ?? [];
    return {
      friends: list.filter((entry) => entry.kind === KIND_FRIEND),
      incoming: list.filter((entry) => entry.kind === KIND_PENDING_INCOMING),
      outgoing: list.filter((entry) => entry.kind === KIND_PENDING_OUTGOING),
    };
  }, [entries]);

  // The Muted rows come from the provider's set, not the graph read, so a mute made anywhere else
  // shows here at once and an unmute here clears it everywhere.
  const mutedEntries = useMemo<RelationshipEntry[]>(
    () => [...mutedSet].map((userId) => ({ userId, kind: KIND_MUTE })),
    [mutedSet],
  );

  // Resolve the relationship rows to names once, through the shared profile cache.
  const relatedIds = useMemo(
    () =>
      [...friends, ...incoming, ...outgoing, ...blocked, ...mutedEntries].map(
        (entry) => entry.userId,
      ),
    [friends, incoming, outgoing, blocked, mutedEntries],
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

  // Unmute from the Muted section: the provider performs the call and drops the id from its set,
  // which is what re-renders this list; `act` only wraps it in the row's busy state.
  const unmute = useCallback(
    async (userId: Id): Promise<void> => {
      await act(userId, () => setMuted(userId, false));
    },
    // `act` closes over state setters only; `setMuted` is the live dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [setMuted],
  );

  // Unfriend from a friend row: a quiet exit on the wire (the other party is told nothing beyond
  // the bare "removed" hint), so the confirm says so — the friendship ends on both sides at once
  // and rebuilding it means asking all over again.
  const removeFriend = useCallback(
    async (userId: Id): Promise<void> => {
      if (!client) {
        return;
      }
      const name = profiles.get(userId)?.displayName ?? 'Someone';
      if (!window.confirm(`Remove ${name} from your friends? They will not be told.`)) {
        return;
      }
      await act(userId, () => client.social.removeFriend(userId));
    },
    // `act` is a stable-shape closure over state setters only; the client and the profile cache
    // (the confirm's name for the person) are the live dependencies.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [client, profiles],
  );

  // Unblock from the Blocked section: the wire lifts the block and restores nothing it tore down
  // (the friendship and the follows stay gone), so the row's button is the whole act — no confirm,
  // because unblocking takes nothing away from the person pressing it.
  const unblock = useCallback(
    async (userId: Id): Promise<void> => {
      if (!client) {
        return;
      }
      await act(userId, () => client.social.unblockUser(userId));
    },
    // `act` is a stable-shape closure over state setters only; the client is the live dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [client],
  );

  return (
    <div className="panel panel-flush">
      {/* One title, not two: the panel is "Friends" and the lists beneath it are its views —
          the search hides behind its icon until the icon is tapped (the field then takes the
          icon's place in the header, focused, with the new-conversation control staying
          visible beside it), the right-aligned icons switch between the views, and each view
          icon carries its count when there is something to count, so a pending request is
          visible without visiting it. */}
      <div className="panel-head">
        <h1 className="panel-title">Friends</h1>
        <div className="panel-head-icons" role="group" aria-label="Friend lists">
          <FriendsSearch
            open={searchOpen}
            active={view === 'search'}
            query={query}
            onQueryChange={setQuery}
            onSubmit={(event) => void onSearch(event)}
            onReveal={() => setSearchOpen(true)}
            onDismiss={dismissSearch}
          />
          <button
            type="button"
            className={`panel-head-icon${view === 'requests' ? ' chosen' : ''}`}
            aria-pressed={view === 'requests'}
            title="Requests"
            onClick={() => setView('requests')}
          >
            <Icon name="user-plus" size={16} />
            {incoming.length > 0 ? (
              <span className="panel-head-count">{incoming.length}</span>
            ) : null}
          </button>
          <button
            type="button"
            className={`panel-head-icon${view === 'blocked' ? ' chosen' : ''}`}
            aria-pressed={view === 'blocked'}
            title="Blocked"
            onClick={() => setView('blocked')}
          >
            <Icon name="block" size={16} />
            {blocked.length > 0 ? <span className="panel-head-count">{blocked.length}</span> : null}
          </button>
          <button
            type="button"
            className={`panel-head-icon${view === 'suggestions' ? ' chosen' : ''}`}
            aria-pressed={view === 'suggestions'}
            title="Suggestions"
            onClick={() => setView('suggestions')}
          >
            <Icon name="sparkle" size={16} />
            {suggestions.length > 0 ? (
              <span className="panel-head-count">{suggestions.length}</span>
            ) : null}
          </button>
          {/* New chat is an action, not a view, so it takes no chosen state — it opens the
              dialog and leaves whichever list is on screen alone. */}
          <button
            type="button"
            className="panel-head-icon"
            onClick={() => setDialogOpen(true)}
            aria-label="New conversation"
            title="New conversation"
          >
            <Icon name="plus" size={16} />
          </button>
        </div>
      </div>

      {error ? <p className="form-error">{error}</p> : null}

      {entries === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : view === 'friends' ? (
        <>
          {/* The contact list leads: friends, presence-first, before anything administrative.
              No heading of its own — the panel title is the list's name. */}
          <FriendsSection
            entries={friends}
            profiles={profiles}
            presence={presence}
            busy={busy}
            onSelect={(userId) => setSelected(userId)}
            onMessage={(userId) => void startDirect(userId)}
            onRemove={(userId) => void removeFriend(userId)}
          />

          <MutedSection
            entries={mutedEntries}
            profiles={profiles}
            busy={busy}
            onSelect={(userId) => setSelected(userId)}
            onUnmute={(userId) => void unmute(userId)}
          />
        </>
      ) : view === 'search' ? (
        <>
          {/* The results of the header's search field: the field reveals from its icon and
              lives in the panel head, so this view is what it finds, not where it lives. */}
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
          ) : (
            <p className="muted">Find people by username to add them as friends.</p>
          )}
        </>
      ) : view === 'requests' ? (
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
      ) : view === 'blocked' ? (
        <BlockedSection
          entries={blocked}
          profiles={profiles}
          busy={busy}
          onSelect={(userId) => setSelected(userId)}
          onUnblock={(userId) => void unblock(userId)}
        />
      ) : (
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
 * The friends list: the graph's Friend-kind rows, presence-first, each row a door to the person's
 * profile with the friendship's own exit beside it.
 *
 * Exported presentational over plain data, so the list's rules — an honest empty state, one row
 * per friend, every row opening the profile, the Remove control on every row and disabled only
 * while its own wire call flies — are testable without a live client, the same bargain the other
 * sections make.
 */
export function FriendsSection({
  entries,
  profiles,
  presence,
  busy,
  onSelect,
  onMessage,
  onRemove,
}: {
  entries: RelationshipEntry[];
  /** Resolved profiles through the shared cache; an unresolved account keeps a stable fallback. */
  profiles: ReadonlyMap<
    Id,
    { displayName: string; username?: string; avatarUrl?: string; customStatus?: string }
  >;
  /** Live presence by id, when the caller watches it; absent leaves the rows ambient-free. */
  presence?: ReadonlyMap<Id, PresenceState>;
  /** The ids with a removal in flight, so a row's button can disable itself. */
  busy?: ReadonlySet<Id>;
  onSelect: (userId: Id) => void;
  onMessage: (userId: Id) => void;
  /** Ends the friendship; the caller owns the confirm, since it owns the person's name. */
  onRemove: (userId: Id) => void;
}): ReactNode {
  return (
    <section className="panel-section" aria-label="Your friends">
      {entries.length === 0 ? (
        <p className="muted">No friends yet. Add someone from the suggestions below.</p>
      ) : (
        entries.map((entry) => (
          <PersonRow
            key={entry.userId}
            id={entry.userId}
            name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
            username={profiles.get(entry.userId)?.username}
            avatarUrl={profiles.get(entry.userId)?.avatarUrl}
            note={
              profiles.get(entry.userId)?.customStatus ?? presenceLabel(presence?.get(entry.userId))
            }
            presence={presence?.get(entry.userId)}
            onSelect={() => onSelect(entry.userId)}
            onMessage={() => onMessage(entry.userId)}
            actions={
              <button
                type="button"
                className="btn btn-ghost"
                disabled={busy?.has(entry.userId) ?? false}
                onClick={(event) => {
                  // The row is a door to the profile; the button is not — stop the row's open.
                  event.stopPropagation();
                  onRemove(entry.userId);
                }}
              >
                Remove friend
              </button>
            }
          />
        ))
      )}
    </section>
  );
}

/**
 * The Friends header's search, collapsed to an icon until it is asked for.
 *
 * The header's right-hand row is narrow and the field is idle most of the time, so the icon
 * stands in for it: a tap reveals the field in the icon's place, focused and ready, and the
 * field leaves again the moment the search is finished — its own close control, or a blur
 * while the query is empty, collapses it back to the icon. What the query *does* (the search
 * itself, the results view) is the panel's and is untouched by the field's visibility.
 *
 * Exported presentational over plain props, so the two states — the icon that offers the
 * search, the focused field with its way out — are testable without a live client, the same
 * bargain {@link BlockedSection} and {@link MutedSection} make.
 *
 * Two surfaces share this control: the panel's own head (`tone="panel"`, the default) and the
 * phone's home view header (`tone="home"`). The behaviour is one bargain — the icon offers, the
 * field arrives focused, the close or an empty blur ends it — so it lives here once; only the ink
 * differs, because the panel head is a light strip and the home view header is the teal band whose
 * controls are white glass.
 */
export function FriendsSearch({
  tone = 'panel',
  open,
  active,
  query,
  onQueryChange,
  onSubmit,
  onReveal,
  onDismiss,
}: {
  /** Which surface's ink the control wears: the panel head's, or the phone header's teal band. */
  tone?: 'panel' | 'home';
  /** Whether the field is revealed; the icon stands in for it until someone wants it. */
  open: boolean;
  /** Whether the search results are the view on screen — the icon's chosen state when closed. */
  active: boolean;
  query: string;
  onQueryChange: (query: string) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  /** The icon's tap: the field takes the icon's place, focused. */
  onReveal: () => void;
  /** The search is finished: the field leaves, empty, and the icon returns. */
  onDismiss: () => void;
}): ReactNode {
  // The home tone rides the teal view header, whose controls are the white-glass `tbtn` set —
  // the panel classes would paint a near-invisible ink there, and the panel classes are the
  // default everywhere else.
  const iconClass =
    tone === 'home'
      ? `tbtn tbtn-sm${active ? ' tbtn-on' : ''}`
      : `panel-head-icon${active ? ' chosen' : ''}`;
  if (!open) {
    return (
      <button
        type="button"
        className={iconClass}
        aria-pressed={active}
        onClick={onReveal}
        aria-label="Search people by username"
        title="Search people by username"
      >
        <Icon name="search" size={16} />
      </button>
    );
  }
  return (
    <form
      className={tone === 'home' ? 'mhome-head-search' : 'panel-head-search'}
      role="search"
      onSubmit={onSubmit}
    >
      <input
        type="search"
        className={tone === 'home' ? 'mhome-viewhead-search' : 'input'}
        value={query}
        onChange={(event) => onQueryChange(event.target.value)}
        /* The field arrived because someone asked for it, so it arrives ready to type in. */
        autoFocus
        onBlur={() => {
          // A blur on an empty field is the search ending without starting: collapse quietly.
          // A blur on words worth searching keeps the field — the search is still being made.
          if (query.trim().length === 0) {
            onDismiss();
          }
        }}
        placeholder="Search by username"
        aria-label="Search people by username"
      />
      {/* The explicit way out: the close finishes the search whatever the query holds. */}
      <button
        type="button"
        className={tone === 'home' ? 'mhome-viewhead-search-x' : 'panel-head-search-x'}
        onClick={onDismiss}
        aria-label="Close search"
        title="Close search"
      >
        <Icon name="close" size={13} />
      </button>
    </form>
  );
}

/**
 * The Blocked section: the block list the whole-graph read surfaced, each row a door to the
 * person's profile with an Unblock button beside it.
 *
 * Exported presentational over plain data, so the section's rules — an honest empty state, one
 * row per blocked account, every row opening the profile, the unblock disabled only while its
 * own wire call flies — are testable without a live client.
 */
export function BlockedSection({
  entries,
  profiles,
  busy,
  onSelect,
  onUnblock,
}: {
  entries: RelationshipEntry[];
  /** Resolved profiles through the shared cache; an unresolved account keeps a stable fallback. */
  profiles: ReadonlyMap<Id, { displayName: string; username?: string; avatarUrl?: string }>;
  /** The ids with an unblock in flight, so a row's button can disable itself. */
  busy?: ReadonlySet<Id>;
  onSelect: (userId: Id) => void;
  onUnblock: (userId: Id) => void;
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
            actions={
              <button
                type="button"
                className="btn btn-ghost"
                disabled={busy?.has(entry.userId) ?? false}
                onClick={(event) => {
                  // The row is a door to the profile; the button is not — stop the row's open.
                  event.stopPropagation();
                  onUnblock(entry.userId);
                }}
              >
                Unblock
              </button>
            }
          />
        ))
      )}
    </section>
  );
}

/**
 * The Muted section: the personal-mute set the provider owns, each row a door to the person's
 * profile and an Unmute button beside it.
 *
 * Mirrors {@link BlockedSection} — exported presentational over plain data — and carries its
 * edge's one-tap undo the same way the Blocked rows now carry Unblock. The note names what a mute
 * does and does not do, since it is the gentler cousin of a block: room chatter hidden, direct
 * messages left alone.
 */
export function MutedSection({
  entries,
  profiles,
  busy,
  onSelect,
  onUnmute,
}: {
  entries: RelationshipEntry[];
  /** Resolved profiles through the shared cache; an unresolved account keeps a stable fallback. */
  profiles: ReadonlyMap<Id, { displayName: string; username?: string; avatarUrl?: string }>;
  /** The ids with an unmute in flight, so a row's button can disable itself. */
  busy?: ReadonlySet<Id>;
  onSelect: (userId: Id) => void;
  onUnmute: (userId: Id) => void;
}): ReactNode {
  return (
    <section className="panel-section" aria-label="Muted accounts">
      <h2 className="panel-heading">Muted</h2>
      {entries.length === 0 ? (
        <p className="muted">
          No muted accounts. Mute someone to hide their room messages for you.
        </p>
      ) : (
        entries.map((entry) => (
          <PersonRow
            key={entry.userId}
            id={entry.userId}
            name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
            username={profiles.get(entry.userId)?.username}
            avatarUrl={profiles.get(entry.userId)?.avatarUrl}
            note="muted · room messages hidden"
            onSelect={() => onSelect(entry.userId)}
            actions={
              <button
                type="button"
                className="btn btn-ghost"
                disabled={busy?.has(entry.userId) ?? false}
                onClick={(event) => {
                  // The row is a door to the profile; the button is not — stop the row's open.
                  event.stopPropagation();
                  onUnmute(entry.userId);
                }}
              >
                Unmute
              </button>
            }
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
