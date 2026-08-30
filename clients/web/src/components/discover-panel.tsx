'use client';

import { useCallback, useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id, RoomJoinResponse, RoomSummary } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { roomInfoOf, useRooms } from '@/lib/migo/rooms-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

/** The room page the panel browses; the server owns the ceiling it clamps this to. */
const PAGE_SIZE = 20;

/**
 * The Discover tab: browse public rooms and join one.
 *
 * Joining hands the conversation back to the chat shell — the join reply carries the conversation
 * handle (`conversationId`, `encryption`, `lastSeq`), which this panel projects onto a
 * {@link ConversationSummary}, inserts into the shared conversation list, and opens through the
 * caller's {@link onOpenConversation} so the user lands in the thread. The thread's own hook then
 * subscribes and replays history exactly as it would for any other conversation.
 */
export function DiscoverPanel({
  onOpenConversation,
}: {
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client } = useMigo();
  const { noteConversation } = useConversations();
  const { noteRoom } = useRooms();

  const [rooms, setRooms] = useState<RoomSummary[] | null>(null);
  const [query, setQuery] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [joining, setJoining] = useState<ReadonlySet<Id>>(new Set());

  const reload = useCallback(
    async (text: string): Promise<void> => {
      if (!client) {
        return;
      }
      try {
        const response = await client.rooms.list(PAGE_SIZE, text.length > 0 ? { query: text } : {});
        setRooms(response.rooms);
        setError(null);
      } catch (cause) {
        setError(friendlyError(cause));
      }
    },
    [client],
  );

  useEffect(() => {
    void reload('');
  }, [reload]);

  async function onSearch(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    await reload(query.trim());
  }

  async function onJoin(room: RoomSummary): Promise<void> {
    if (!client || joining.has(room.roomId)) {
      return;
    }
    setJoining((prev) => new Set(prev).add(room.roomId));
    try {
      const joined = await client.rooms.join(room.roomId);
      // Project the join handle into the shared list so the shell can open it like any
      // conversation, and into the room registry so the row and header keep the room's name,
      // topic, and counters — the join reply is the one moment the wire names both halves.
      noteConversation(joinedRoomSummary(joined));
      noteRoom(roomInfoOf(joined));
      onOpenConversation(joined.conversationId);
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setJoining((prev) => {
        const next = new Set(prev);
        next.delete(room.roomId);
        return next;
      });
    }
  }

  return (
    <div className="panel">
      <h1 className="panel-title">Discover</h1>

      <form className="panel-search" role="search" onSubmit={(event) => void onSearch(event)}>
        <input
          type="search"
          className="input"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search public rooms"
          aria-label="Search public rooms"
        />
        <button type="submit" className="btn">
          Search
        </button>
      </form>

      {error ? <p className="form-error">{error}</p> : null}

      {rooms === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : rooms.length === 0 ? (
        <div className="center-fill">
          <div>
            <div className="emoji">🧭</div>
            {query.trim().length > 0 ? 'No rooms matched your search.' : 'No public rooms yet.'}
          </div>
        </div>
      ) : (
        <section className="panel-section" aria-label="Public rooms">
          <h2 className="panel-heading">Rooms</h2>
          {rooms.map((room) => (
            <div className="person-row" key={room.roomId}>
              <Avatar name={room.name} id={room.roomId} size={36} avatarUrl={room.avatarUrl} />
              <div className="person-main">
                <span className="person-name">{room.name}</span>
                <span className="person-sub">
                  {room.memberCount} members · {room.onlineCount} online
                </span>
                {room.topic ? <span className="person-note">{room.topic}</span> : null}
              </div>
              <div className="person-actions">
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={joining.has(room.roomId)}
                  onClick={() => void onJoin(room)}
                >
                  {joining.has(room.roomId) ? <Spinner /> : 'Join'}
                </button>
              </div>
            </div>
          ))}
        </section>
      )}
    </div>
  );
}

/**
 * Projects a room-join handle onto the conversation summary the shared list holds.
 *
 * Pure, so a test can pin the projection. `readSeq` is set to `lastSeq`: the room is joined at its
 * tip, and a freshly joined room showing a phantom unread badge would send the user hunting for
 * history they have not missed.
 */
export function joinedRoomSummary(joined: RoomJoinResponse): ConversationSummary {
  return {
    conversationId: joined.conversationId,
    kind: ConversationKind.Room,
    encryption: joined.encryption,
    lastSeq: joined.lastSeq,
    readSeq: joined.lastSeq,
    title: joined.room.name,
    ...(joined.room.avatarUrl !== undefined ? { avatarUrl: joined.room.avatarUrl } : {}),
  };
}
