'use client';

/**
 * The Rooms section: the public directory and the way in.
 *
 * The directory is the server's room catalogue paged through {@link RoomsDomain.list} — the
 * panel adds the browsing shape: a live query box, category chips (the categories the wire
 * actually carries, collected from the results rather than guessed at a server that may not
 * share them), and a Popular/New sort over whatever page is held. Rows state the facts a join
 * decision needs — name, topic, members, live online count, the verified mark — and nothing
 * else.
 *
 * Joining goes through the shared {@link useJoinRoom} flow, which projects the join reply into
 * the conversation list and the room registry and hands the thread to the caller. A room
 * already joined (a Room-kind conversation the shell holds) offers Open instead of Join — the
 * second join would be a round trip to learn what the shell already knows.
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { Id, RoomSummary } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useJoinRoom } from '@/lib/migo/use-join-room.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { EmptyState } from './states.js';
import { Skeleton } from './states.js';
import { Spinner } from './spinner.js';

/** The room page the panel browses; the server owns the ceiling it clamps this to. */
const PAGE_SIZE = 30;

/** The search debounce: typing is not yet asking, but a pause is. */
const SEARCH_DEBOUNCE_MS = 300;

/** The directory's client-side sorts; the wire's own ordering is the default. */
type Sort = 'default' | 'popular' | 'new';

const SORTS: ReadonlyArray<{ id: Sort; label: string }> = [
  { id: 'default', label: 'All' },
  { id: 'popular', label: 'Popular' },
  { id: 'new', label: 'New' },
];

/**
 * The Rooms section panel: query, categories, sort, and the rows.
 *
 * @param onOpenConversation Hands the opened conversation back so the shell can switch to the
 *   thread — the same flow the Home dashboard and Search use.
 */
export function RoomsPanel({
  onOpenConversation,
}: {
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client } = useMigo();
  const { items: conversations } = useConversations();
  const { infoFor } = useRooms();
  const { join, joining, error } = useJoinRoom(onOpenConversation);

  const [rooms, setRooms] = useState<RoomSummary[] | null>(null);
  const [query, setQuery] = useState('');
  const [category, setCategory] = useState<string | null>(null);
  const [sort, setSort] = useState<Sort>('default');
  const [loadError, setLoadError] = useState<string | null>(null);
  const [reloadNonce, setReloadNonce] = useState(0);
  // The debounced query is what the wire sees; the input's live text is what the user sees.
  const [liveQuery, setLiveQuery] = useState('');
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // The live text becomes the query after a pause in typing — one round trip per intent, not
  // per keystroke, against a rate-limited endpoint.
  function onSearchInput(value: string): void {
    setLiveQuery(value);
    if (debounceRef.current !== null) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = setTimeout(() => setQuery(value), SEARCH_DEBOUNCE_MS);
  }

  // The directory read: the debounced query and the chosen category on the wire, the sort kept
  // client-side over the held page.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    const text = query.trim();
    void (async (): Promise<void> => {
      try {
        const response = await client.rooms.list(PAGE_SIZE, {
          ...(text.length > 0 ? { query: text } : {}),
          ...(category !== null ? { category } : {}),
        });
        if (!cancelled) {
          setRooms(response.rooms);
          setLoadError(null);
        }
      } catch (cause) {
        if (!cancelled) {
          setLoadError(friendlyError(cause));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
    // The reload nonce lets the error state's Try again re-run the read.
  }, [client, query, category, reloadNonce]);

  // The chips are the categories the wire actually carried — a server's vocabulary, discovered
  // from the data rather than hardcoded against a catalogue this build cannot see.
  const categories = useMemo(() => {
    const seen = new Set<string>();
    for (const room of rooms ?? []) {
      if (room.category !== undefined && room.category.length > 0) {
        seen.add(room.category);
      }
    }
    return [...seen].sort();
  }, [rooms]);

  // The rooms the shell has already joined, keyed by room id → conversation id: those rows
  // offer Open (the conversation is already held), not a second Join.
  const joinedByRoomId = useMemo(() => {
    const byRoom = new Map<Id, Id>();
    for (const conversation of conversations) {
      if (conversation.kind === ConversationKind.Room) {
        const info = infoFor(conversation.conversationId);
        if (info !== null) {
          byRoom.set(info.roomId, conversation.conversationId);
        }
      }
    }
    return byRoom;
  }, [conversations, infoFor]);

  const sorted = useMemo(() => {
    const list = [...(rooms ?? [])];
    if (sort === 'popular') {
      list.sort((left, right) => (right.onlineCount ?? 0) - (left.onlineCount ?? 0));
    } else if (sort === 'new') {
      // No created-at on the wire: "New" is the catalogue's own order read back-to-front — the
      // closest the directory can honestly offer.
      list.reverse();
    }
    return list;
  }, [rooms, sort]);

  function onSubmit(event: FormEvent<HTMLFormElement>): void {
    // The debounce already carries the query; submit only stops a mid-pause round trip by
    // flushing now.
    event.preventDefault();
    if (debounceRef.current !== null) {
      clearTimeout(debounceRef.current);
    }
    setQuery(liveQuery.trim());
  }

  return (
    <div className="panel panel-wide">
      <header className="panel-head">
        <h1 className="panel-title">Rooms</h1>
        <button
          type="button"
          className="icon-btn"
          aria-label="Refresh rooms"
          title="Refresh rooms"
          onClick={() => setReloadNonce((nonce) => nonce + 1)}
        >
          <Icon name="refresh" size={20} />
        </button>
      </header>

      <form className="panel-search" role="search" onSubmit={onSubmit}>
        <input
          type="search"
          className="input"
          value={liveQuery}
          onChange={(event) => onSearchInput(event.target.value)}
          placeholder="Search rooms"
          aria-label="Search rooms"
        />
      </form>

      <div className="chip-row" role="group" aria-label="Sort rooms">
        {SORTS.map((option) => (
          <button
            key={option.id}
            type="button"
            className={`chip ${sort === option.id ? 'chip-active' : ''}`}
            aria-pressed={sort === option.id}
            onClick={() => setSort(option.id)}
          >
            {option.label}
          </button>
        ))}
      </div>

      {categories.length > 0 ? (
        <div className="chip-row" role="group" aria-label="Filter by category">
          <button
            type="button"
            className={`chip ${category === null ? 'chip-active' : ''}`}
            aria-pressed={category === null}
            onClick={() => setCategory(null)}
          >
            Everything
          </button>
          {categories.map((name) => (
            <button
              key={name}
              type="button"
              className={`chip ${category === name ? 'chip-active' : ''}`}
              aria-pressed={category === name}
              onClick={() => setCategory(name)}
            >
              {name}
            </button>
          ))}
        </div>
      ) : null}

      {error !== null ? <p className="form-error">{error}</p> : null}

      {loadError !== null ? (
        <EmptyState
          icon="rooms"
          title={loadError}
          action={
            <button
              type="button"
              className="btn"
              onClick={() => setReloadNonce((nonce) => nonce + 1)}
            >
              Try again
            </button>
          }
        />
      ) : rooms === null ? (
        <Skeleton rows={4} />
      ) : sorted.length === 0 ? (
        <EmptyState
          icon="rooms"
          title={query.trim().length > 0 ? 'No rooms matched your search.' : 'No public rooms yet.'}
          hint="Rooms others open will appear here."
        />
      ) : (
        <ul className="room-list" aria-label="Public rooms">
          {sorted.map((room) => {
            const conversationId = joinedByRoomId.get(room.roomId);
            return (
              <RoomRow
                key={room.roomId}
                room={room}
                joined={conversationId !== undefined}
                joining={joining.has(room.roomId)}
                onJoin={() => void join(room)}
                onOpen={() => {
                  if (conversationId !== undefined) {
                    onOpenConversation(conversationId);
                  }
                }}
              />
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** One directory row: the facts of a join decision, and the way in. */
function RoomRow({
  room,
  joined,
  joining,
  onJoin,
  onOpen,
}: {
  room: RoomSummary;
  joined: boolean;
  joining: boolean;
  onJoin: () => void;
  onOpen: () => void;
}): ReactNode {
  return (
    <li className="room-row">
      <Avatar name={room.name} id={room.roomId} size={36} avatarUrl={room.avatarUrl} />
      <div className="person-main">
        <span className="person-name room-name">
          {room.name}
          {room.verified ? (
            <span className="verified-mark" title="Verified room" aria-label="Verified room">
              <Icon name="verified" size={16} />
            </span>
          ) : null}
        </span>
        <span className="person-sub">
          {(room.memberCount ?? 0).toLocaleString()} members ·{' '}
          {(room.onlineCount ?? 0).toLocaleString()} online
          {room.category ? ` · ${room.category}` : ''}
        </span>
        {room.topic || room.description ? (
          <span className="person-note">{room.topic ?? room.description}</span>
        ) : null}
      </div>
      <div className="person-actions">
        {joined ? (
          <button type="button" className="btn btn-ghost" onClick={onOpen}>
            Open
          </button>
        ) : (
          <button type="button" className="btn btn-primary" disabled={joining} onClick={onJoin}>
            {joining ? <Spinner /> : 'Join'}
          </button>
        )}
      </div>
    </li>
  );
}
