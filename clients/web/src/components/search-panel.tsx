'use client';

/**
 * The Search section: one box, everything it can honestly find.
 *
 * The wire's search surfaces are people ({@link SocialDomain.search}) and rooms
 * ({@link RoomsDomain.list} with a query); conversations are held client-side in the shared
 * list, so a conversation match is a local filter over what the session already holds. The
 * panel runs all three at once, debounced — typing is not yet asking, but a pause is — and
 * groups the answers under the headings a user thinks in (People, Rooms, Chats).
 *
 * Before the first query the box is not empty: the social graph's own suggestions and the room
 * catalogue's liveliest page fill the space as "Try these", and the account's recent searches
 * persist locally so the second visit starts where the first left off. Recent searches are the
 * user's own words kept on their own device — never a server-side history.
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, ContentType } from '@migo/sdk';
import type { Id, RoomSummary, SuggestedUser } from '@migo/sdk';

import { conversationTitle } from '@/lib/conversation-title.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { useJoinRoom } from '@/lib/migo/use-join-room.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { EmptyState } from './states.js';
import { Skeleton } from './states.js';
import { Spinner } from './spinner.js';
import { UserProfileModal } from './user-profile-modal.js';

/** The debounce: a pause in typing is the question; a keystroke is not. */
const SEARCH_DEBOUNCE_MS = 300;

/** The page sizes per surface. */
const PEOPLE_LIMIT = 10;
const ROOMS_LIMIT = 10;

/** How many recent searches the box keeps. */
const RECENT_LIMIT = 6;

/** The local-storage key for the account's own recent searches (device-local by design). */
const RECENT_KEY = 'migo:recent-searches';

/**
 * The Search section panel.
 *
 * @param onOpenConversation Hands an opened conversation to the shell — a conversation result,
 *   a joined room's Open, and a fresh join all land in the thread.
 */
export function SearchPanel({
  onOpenConversation,
}: {
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const { items: conversations, lastPreviews } = useConversations();
  const { join, joining } = useJoinRoom(onOpenConversation);

  // The profile map the chat-match titles resolve through — a 1:1's title is its peer's name.
  const peerIds = useMemo(
    () =>
      [
        ...new Set(
          conversations.flatMap((item) =>
            item.kind === ConversationKind.Direct ? (item.members ?? []) : [],
          ),
        ),
      ].filter((id) => id !== accountId),
    [conversations, accountId],
  );
  const profiles = useProfiles(peerIds);

  const [query, setQuery] = useState('');
  const [liveQuery, setLiveQuery] = useState('');
  const [people, setPeople] = useState<SuggestedUser[] | null>(null);
  const [rooms, setRooms] = useState<RoomSummary[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Id | null>(null);
  const [recent, setRecent] = useState<string[]>([]);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // The persisted recent searches, read once on mount — the user's own words, their own device.
  useEffect(() => {
    try {
      const stored = window.localStorage.getItem(RECENT_KEY);
      if (stored !== null) {
        const parsed: unknown = JSON.parse(stored);
        if (Array.isArray(parsed)) {
          setRecent(
            parsed
              .filter((item): item is string => typeof item === 'string')
              .slice(0, RECENT_LIMIT),
          );
        }
      }
    } catch {
      // Unreadable storage: the box starts without history, never broken.
    }
  }, []);

  function noteRecent(text: string): void {
    const trimmed = text.trim();
    if (trimmed.length === 0) {
      return;
    }
    setRecent((prev) => {
      const next = [trimmed, ...prev.filter((item) => item !== trimmed)].slice(0, RECENT_LIMIT);
      try {
        window.localStorage.setItem(RECENT_KEY, JSON.stringify(next));
      } catch {
        // A failed write costs a missing history line, never a broken search.
      }
      return next;
    });
  }
  // Stable so the search effect can depend on it without re-running per render.
  const noteRecentRef = useRef(noteRecent);
  noteRecentRef.current = noteRecent;

  function onSearchInput(value: string): void {
    setLiveQuery(value);
    if (debounceRef.current !== null) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = setTimeout(() => setQuery(value.trim()), SEARCH_DEBOUNCE_MS);
  }

  // The search itself: people and rooms on the wire, conversations locally. Only a non-empty
  // query asks the server; the empty box returns to the pre-query state (suggestions and
  // recents), not to a blank.
  useEffect(() => {
    if (!client) {
      return;
    }
    if (query.length === 0) {
      setPeople(null);
      setRooms(null);
      return;
    }
    let cancelled = false;
    setSearching(true);
    void (async (): Promise<void> => {
      try {
        const [foundPeople, foundRooms] = await Promise.all([
          client.social.search(query, PEOPLE_LIMIT).catch(() => [] as SuggestedUser[]),
          client.rooms
            .list(ROOMS_LIMIT, { query })
            .then((page) => page.rooms)
            .catch(() => [] as RoomSummary[]),
        ]);
        if (!cancelled) {
          setPeople(foundPeople);
          setRooms(foundRooms);
          setError(null);
          noteRecentRef.current(query);
        }
      } catch (cause) {
        if (!cancelled) {
          setError(friendlyError(cause));
        }
      } finally {
        if (!cancelled) {
          setSearching(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, query]);

  // The local half: conversations whose title or newest message matches the query.
  const chatMatches = useMemo(() => {
    const text = query.trim().toLowerCase();
    if (text.length === 0) {
      return [];
    }
    return conversations.filter((conversation) => {
      if (conversationTitle(conversation, accountId, profiles).toLowerCase().includes(text)) {
        return true;
      }
      const preview = lastPreviews.get(conversation.conversationId);
      const content = preview?.content;
      const previewText =
        content !== undefined && content.type === ContentType.Text && 'text' in content
          ? content.text
          : '';
      return previewText.toLowerCase().includes(text);
    });
  }, [conversations, lastPreviews, query, accountId, profiles]);

  const hasQuery = query.length > 0;
  const noResults =
    hasQuery &&
    people !== null &&
    rooms !== null &&
    people.length === 0 &&
    rooms.length === 0 &&
    chatMatches.length === 0;

  return (
    <div className="panel panel-wide">
      <h1 className="panel-title">Search</h1>

      <div className="search-box" role="search">
        <span className="search-box-icon" aria-hidden="true">
          <Icon name="search" size={20} />
        </span>
        <input
          type="search"
          className="input search-box-input"
          value={liveQuery}
          onChange={(event) => onSearchInput(event.target.value)}
          placeholder="Search people, rooms, chats"
          aria-label="Search people, rooms, chats"
          autoFocus
        />
        {searching ? <Spinner /> : null}
      </div>

      {error !== null ? <p className="form-error">{error}</p> : null}

      {!hasQuery ? (
        <>
          {recent.length > 0 ? (
            <section className="panel-section" aria-label="Recent searches">
              <h2 className="panel-heading">Recent</h2>
              <div className="chip-row">
                {recent.map((text) => (
                  <button
                    key={text}
                    type="button"
                    className="chip"
                    onClick={() => {
                      setLiveQuery(text);
                      setQuery(text);
                    }}
                  >
                    {text}
                  </button>
                ))}
              </div>
            </section>
          ) : null}
          <RecentPeople client={client} onSelect={setSelected} />
        </>
      ) : noResults ? (
        <EmptyState
          icon="search"
          title={`Nothing found for “${query}”.`}
          hint="Try a username, a room name, or a word from a conversation."
        />
      ) : (
        <>
          {chatMatches.length > 0 ? (
            <section className="panel-section" aria-label="Chat results">
              <h2 className="panel-heading">Chats</h2>
              <ul className="digest-list">
                {chatMatches.map((conversation) => (
                  <li key={conversation.conversationId}>
                    <button
                      type="button"
                      className="digest-row"
                      onClick={() => onOpenConversation(conversation.conversationId)}
                    >
                      <Avatar
                        name={conversationTitle(conversation, accountId, profiles)}
                        id={conversation.conversationId}
                        size={32}
                      />
                      <span className="digest-main">
                        <span className="person-name">
                          {conversationTitle(conversation, accountId, profiles)}
                        </span>
                        <span className="person-sub">Open conversation</span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          ) : null}

          {people === null && rooms === null && !searching ? null : people === null ? (
            <Skeleton rows={3} />
          ) : (
            <section className="panel-section" aria-label="People results">
              <h2 className="panel-heading">People</h2>
              {people.length === 0 ? (
                <p className="muted">No people matched.</p>
              ) : (
                <ul className="digest-list">
                  {people.map((person) => (
                    <li key={person.accountId}>
                      <button
                        type="button"
                        className="digest-row"
                        onClick={() => setSelected(person.accountId)}
                      >
                        <Avatar name={person.displayName} id={person.accountId} size={32} />
                        <span className="digest-main">
                          <span className="person-name">{person.displayName}</span>
                          <span className="person-sub">
                            @{person.username}
                            {person.mutualFriends > 0 ? ` · ${person.mutualFriends} mutual` : ''}
                          </span>
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          )}

          {rooms !== null ? (
            <section className="panel-section" aria-label="Room results">
              <h2 className="panel-heading">Rooms</h2>
              {rooms.length === 0 ? (
                <p className="muted">No rooms matched.</p>
              ) : (
                <ul className="digest-list">
                  {rooms.map((room) => (
                    <li key={room.roomId}>
                      <div className="digest-row digest-row-static">
                        <Avatar
                          name={room.name}
                          id={room.roomId}
                          size={32}
                          avatarUrl={room.avatarUrl}
                        />
                        <span className="digest-main">
                          <span className="person-name">{room.name}</span>
                          <span className="person-sub">
                            {(room.onlineCount ?? 0).toLocaleString()} online
                            {room.category ? ` · ${room.category}` : ''}
                          </span>
                        </span>
                        <button
                          type="button"
                          className="btn btn-primary btn-sm"
                          disabled={joining.has(room.roomId)}
                          onClick={() => void join(room)}
                        >
                          {joining.has(room.roomId) ? <Spinner /> : 'Join'}
                        </button>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          ) : null}
        </>
      )}

      {selected !== null ? (
        <UserProfileModal userId={selected} blocked={false} onClose={() => setSelected(null)} />
      ) : null}
    </div>
  );
}

/** The pre-query people block: the social graph's own suggestions, offered as doors. */
function RecentPeople({
  client,
  onSelect,
}: {
  client: ReturnType<typeof useMigo>['client'];
  onSelect: (userId: Id) => void;
}): ReactNode {
  const [suggestions, setSuggestions] = useState<SuggestedUser[] | null>(null);

  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.social
      .suggestions(PEOPLE_LIMIT)
      .then((people) => {
        if (!cancelled) {
          setSuggestions(people);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setSuggestions([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  if (suggestions === null) {
    return <Skeleton rows={3} />;
  }
  if (suggestions.length === 0) {
    return null;
  }
  return (
    <section className="panel-section" aria-label="Suggested people">
      <h2 className="panel-heading">People to meet</h2>
      <ul className="digest-list">
        {suggestions.map((person) => (
          <li key={person.accountId}>
            <button type="button" className="digest-row" onClick={() => onSelect(person.accountId)}>
              <Avatar name={person.displayName} id={person.accountId} size={32} />
              <span className="digest-main">
                <span className="person-name">{person.displayName}</span>
                <span className="person-sub">
                  @{person.username}
                  {person.mutualFriends > 0 ? ` · ${person.mutualFriends} mutual` : ''}
                </span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
