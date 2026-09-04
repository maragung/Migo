'use client';

/**
 * Room metadata for the chat shell: the room behind a Room-kind conversation.
 *
 * The server keeps rooms and conversations in separate frames on purpose — a room *is* a large
 * discoverable conversation, joined and left and moderated through its own service — and the
 * cost of that split lands here: a conversation summary for a room carries no room id, no name,
 * no topic, and no counters, and a room listing carries no conversation id. The one wire moment
 * both are visible together is the join reply, so the shell builds its own session map from
 * joins and keeps it fresh from the room service's live state deltas
 * ({@link RoomsDomain.onState} carries exactly the coalesced counters and metadata a header
 * wants: online count, member count, topic).
 *
 * The deltas only arrive on the room's own topic, which the gateway delivers to subscribers
 * alone, so knowing a room and watching it are one step here: {@link noteRoom} subscribes the
 * topic (the SDK re-subscribes it across a session reset), and a restored session re-watches
 * every remembered room. A room the account has since left refuses the watch; its record simply
 * stops updating.
 *
 * The map is persisted (see `lib/storage/room-info-store.ts`): without persistence a reloaded
 * session would render every joined room as an anonymous "Room" row, because the conversation
 * list would never name it again. The stored copy is scoped to the account — room names are a
 * map of where an account spends its time, and the next account on the same device inherits
 * none of it. It is room metadata only — public facts a room publishes about itself — never
 * credentials or key material.
 */

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import type { Id, RoomJoinResponse, RoomStateEvent } from '@migo/sdk';

import { useMigo } from './use-migo.js';
import { clearRoomInfo, loadRoomInfo, saveRoomInfo } from '@/lib/storage/room-info-store.js';

/** The room-side facts a Room-kind conversation's row and header show. */
export interface RoomInfo {
  roomId: Id;
  conversationId: Id;
  name: string;
  topic?: string;
  memberCount?: number;
  onlineCount?: number;
  /** The room's capacity: the ceiling the online/member counts climb toward. */
  maxMembers?: number;
}

export interface RoomsContextValue {
  /** The room behind a conversation, when this shell knows one. */
  infoFor: (conversationId: Id) => RoomInfo | null;
  /**
   * The live record for a room id, when this shell watches one.
   *
   * The directory's rows hold the counts a `ROOM_LIST` page carried — a snapshot from the moment
   * of the read — while this record is the one the state deltas keep current. A row whose room
   * is joined renders these counts instead, so the directory moves with the room rather than
   * with the last refresh.
   */
  liveFor: (roomId: Id) => RoomInfo | null;
  /** Records or refreshes a room from a join reply (the one moment the wire names both). */
  noteRoom: (info: RoomInfo) => void;
  /**
   * Drops a room's record after a leave.
   *
   * The server's leave fan-out deliberately excludes the leaver's own device, so no state delta
   * will ever correct the held counts — without this call a left room's row keeps showing the
   * counts it had while joined ("1 online, 1 member" in a room of nobody). The directory page
   * re-reads on its own triggers and is the honest count after that.
   */
  forgetRoom: (roomId: Id) => void;
}

const RoomsContext = createContext<RoomsContextValue | null>(null);

/**
 * Projects a join reply onto the shell's room record.
 *
 * Pure, so a test can pin it. Only fields the wire actually carries are set — an absent topic is
 * an absent key, not an undefined-valued one — and the join's own counters are the seed the
 * state deltas then keep current.
 */
export function roomInfoOf(joined: RoomJoinResponse): RoomInfo {
  return {
    roomId: joined.room.roomId,
    conversationId: joined.conversationId,
    name: joined.room.name,
    ...(joined.room.topic !== undefined ? { topic: joined.room.topic } : {}),
    memberCount: joined.room.memberCount,
    onlineCount: joined.room.onlineCount,
    ...(joined.room.maxMembers !== undefined ? { maxMembers: joined.room.maxMembers } : {}),
  };
}

/**
 * Applies one room-state delta onto the held record.
 *
 * The event is a delta by protocol: each field it carries replaces the held value, and each
 * field it omits leaves the held value alone. Treating it as a snapshot would blank the topic
 * every time the online count moved — the exact bug the delta shape exists to prevent.
 */
export function applyRoomState(info: RoomInfo, delta: RoomStateEvent): RoomInfo {
  return {
    ...info,
    ...(delta.onlineCount !== undefined ? { onlineCount: delta.onlineCount } : {}),
    ...(delta.memberCount !== undefined ? { memberCount: delta.memberCount } : {}),
    ...(delta.topic !== undefined ? { topic: delta.topic } : {}),
    ...(delta.maxMembers !== undefined ? { maxMembers: delta.maxMembers } : {}),
  };
}

/**
 * The room capacity line: online out of the ceiling, as in "2/33".
 *
 * Pure, so a test can pin it. The ceiling is the honest part — a room with no `maxMembers` on the
 * wire (an older server, or a room the field never rode on) has no known capacity, so the label
 * falls back to the bare online count rather than inventing a denominator or printing "/0". The
 * numerator is the online count, the count a reader cares about when deciding whether to enter.
 */
export function capacityLabel(online: number | undefined, max: number | undefined): string {
  const here = online ?? 0;
  if (max === undefined || max <= 0) {
    return `${here}`;
  }
  return `${here}/${max}`;
}

export function RoomsProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, accountId, resetNonce } = useMigo();

  // The state exists for its re-render: `infoFor` reads the ref (always current), and each
  // commit swaps the context value so every consumer re-renders and re-reads.
  const [, setRooms] = useState<Map<Id, RoomInfo>>(new Map());
  // A mirror for the event handler (async-stable) and the reverse index the state deltas need:
  // they name a room, and the map is keyed by conversation.
  const roomsRef = useRef(new Map<Id, RoomInfo>());
  const byRoomId = useRef(new Map<Id, Id>());

  /**
   * Subscribes the room's topic, the one gate the state deltas pass through. A refusal (a room
   * the account has since left) leaves the record in place, merely static.
   */
  const watch = useCallback(
    (roomId: Id): void => {
      if (!client) {
        return;
      }
      void client.watchRoom(roomId).catch(() => {});
    },
    [client],
  );

  const commit = useCallback(
    (next: Map<Id, RoomInfo>): void => {
      roomsRef.current = next;
      setRooms(next);
      if (accountId === null) {
        return;
      }
      // Persisted as a plain record: the map is small (the rooms this account joined), and a
      // write per change keeps the stored copy exactly as fresh as the rendered one.
      const persisted: Record<string, RoomInfo> = {};
      for (const [conversationId, info] of next) {
        persisted[conversationId] = info;
      }
      void saveRoomInfo(accountId, persisted).catch(() => {
        // A failed write costs a stale name after the next reload, never a wrong screen now.
      });
    },
    [accountId],
  );

  const noteRoom = useCallback(
    (info: RoomInfo): void => {
      const next = new Map(roomsRef.current);
      next.set(info.conversationId, info);
      byRoomId.current.set(info.roomId, info.conversationId);
      commit(next);
      watch(info.roomId);
    },
    [commit, watch],
  );

  // The leave's other half. The server's member event names the leaver as the fan-out's
  // excluded device, so nothing on the wire will ever bring the held record down to the room's
  // real counts — the drop happens here, at the one place that knows the leave succeeded.
  const forgetRoom = useCallback(
    (roomId: Id): void => {
      const conversationId = byRoomId.current.get(roomId);
      if (conversationId === undefined) {
        return;
      }
      const next = new Map(roomsRef.current);
      next.delete(conversationId);
      byRoomId.current.delete(roomId);
      commit(next);
    },
    [commit],
  );

  // Restore the remembered rooms once per session: their names back on the rows, their topics
  // back under watch, so the counters live again without a re-join. The stored copy is only a
  // floor — a join in this session re-notes the room with fresher counters over it. A copy
  // stored under another account is discarded outright: room names are a map of where an
  // account spends its time, and the next account on this device inherits nothing of it.
  useEffect(() => {
    if (accountId === null) {
      return;
    }
    let cancelled = false;
    loadRoomInfo()
      .then((persisted) => {
        if (cancelled) {
          return;
        }
        if (persisted === undefined || persisted.accountId !== accountId) {
          if (persisted !== undefined) {
            void clearRoomInfo().catch(() => {});
          }
          return;
        }
        const next = new Map<Id, RoomInfo>();
        for (const info of Object.values(persisted.rooms)) {
          next.set(info.conversationId, info);
          byRoomId.current.set(info.roomId, info.conversationId);
        }
        roomsRef.current = next;
        setRooms(next);
        for (const info of next.values()) {
          watch(info.roomId);
        }
      })
      .catch(() => {
        // Unreadable or absent storage: the session simply starts without remembered names.
      });
    return () => {
      cancelled = true;
    };
  }, [accountId, watch]);

  // The live state deltas: counters and topic for rooms this shell knows. A delta for an
  // unknown room is dropped — without the join there is no conversation to attach it to.
  useEffect(() => {
    if (!client) {
      return;
    }
    const off = client.rooms.onState((delta) => {
      const conversationId = byRoomId.current.get(delta.roomId);
      if (conversationId === undefined) {
        return;
      }
      const held = roomsRef.current.get(conversationId);
      if (held === undefined) {
        return;
      }
      const next = new Map(roomsRef.current);
      next.set(conversationId, applyRoomState(held, delta));
      commit(next);
    });
    return off;
  }, [client, resetNonce, commit]);

  const infoFor = useCallback(
    (conversationId: Id): RoomInfo | null => roomsRef.current.get(conversationId) ?? null,
    [],
  );

  // The reverse index the context exposes: the same records, addressed by room id. The map is
  // small (the rooms this account joined) and the commit that updates it always runs before the
  // re-render a consumer's read drives, so a lookup here is never behind the rendered state.
  const liveFor = useCallback((roomId: Id): RoomInfo | null => {
    const conversationId = byRoomId.current.get(roomId);
    if (conversationId === undefined) {
      return null;
    }
    return roomsRef.current.get(conversationId) ?? null;
  }, []);

  const value: RoomsContextValue = { infoFor, liveFor, noteRoom, forgetRoom };
  return <RoomsContext.Provider value={value}>{children}</RoomsContext.Provider>;
}

export function useRooms(): RoomsContextValue {
  const value = useContext(RoomsContext);
  if (value === null) {
    throw new Error('useRooms must be used within a RoomsProvider');
  }
  return value;
}
