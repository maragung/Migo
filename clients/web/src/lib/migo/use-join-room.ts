'use client';

/**
 * The join flow, shared by every surface that offers a room.
 *
 * Joining is one wire call, but its reply is the single moment the wire names both halves of a
 * room — the room record and the conversation handle — so every join must project the reply the
 * same way: a {@link ConversationSummary} into the shared conversation list, the room record
 * into the shell's registry, and the opened conversation back to the caller so the shell can
 * switch to the thread. The Home dashboard, the Rooms directory, and Search all offer rooms;
 * this module is the one implementation of the flow.
 */

import { useCallback, useState } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id, RoomJoinResponse, RoomSummary } from '@migo/sdk';

import { friendlyError } from './errors.js';
import { useConversations } from './conversations-provider.js';
import { roomInfoOf, useRooms } from './rooms-provider.js';
import { useMigo } from './use-migo.js';

/**
 * Projects a room-join handle onto the conversation summary the shared list holds.
 *
 * Pure, so a test can pin the projection. `readSeq` is set to `lastSeq`: the room is joined at
 * its tip, and a freshly joined room showing a phantom unread badge would send the user hunting
 * for history they have not missed.
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

/** The join flow's surface: which rooms are mid-join, and the flow's own failure line. */
export interface JoinRoom {
  /** Joins the room and hands the conversation to the caller; resolves false on failure. */
  join: (room: RoomSummary) => Promise<boolean>;
  /** The room ids with a join in flight, so their rows can disable their buttons. */
  joining: ReadonlySet<Id>;
  /** The flow's failure line; a refused join never reads as a broken panel. */
  error: string | null;
}

/**
 * The one join flow, parameterised by where the opened conversation goes.
 *
 * @param onOpenConversation Called with the join's conversation handle — the caller switches to
 *   the chats section and opens the thread.
 */
export function useJoinRoom(onOpenConversation: (conversationId: Id) => void): JoinRoom {
  const { client } = useMigo();
  const { noteConversation } = useConversations();
  const { noteRoom } = useRooms();

  const [joining, setJoining] = useState<ReadonlySet<Id>>(new Set());
  const [error, setError] = useState<string | null>(null);

  const join = useCallback(
    async (room: RoomSummary): Promise<boolean> => {
      if (!client || joining.has(room.roomId)) {
        return false;
      }
      setJoining((prev) => new Set(prev).add(room.roomId));
      try {
        const joined = await client.rooms.join(room.roomId);
        noteConversation(joinedRoomSummary(joined));
        noteRoom(roomInfoOf(joined));
        onOpenConversation(joined.conversationId);
        return true;
      } catch (cause) {
        setError(friendlyError(cause));
        return false;
      } finally {
        setJoining((prev) => {
          const next = new Set(prev);
          next.delete(room.roomId);
          return next;
        });
      }
    },
    [client, joining, noteConversation, noteRoom, onOpenConversation],
  );

  return { join, joining, error };
}
