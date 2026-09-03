'use client';

/**
 * The open group's membership log: who came, who went, who was shown the door.
 *
 * The conversations domain's member stream carries a per-member change every time someone joins,
 * leaves, or is removed from a group. This hook keeps a short tail of those changes for the *one*
 * group the user is reading, so the thread can surface them as they happen — the same ambient
 * "X joined the group" line a room gets, never a durable record. The tail is capped and reset
 * whenever the open group changes or the session resets, exactly as the room log does.
 */

import { useEffect, useRef, useState } from 'react';

import { MemberChange } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { useMigo } from './use-migo.js';
import type { RoomNotice } from './use-room-notices.js';

/** How many recent membership changes the open group keeps on screen at once. */
export const MAX_GROUP_NOTICES = 50;

/**
 * Keeps the open group's most recent membership changes, newest last.
 *
 * Pass the *conversation* id; pass `null` when the open conversation is not a group, and the hook
 * holds nothing and subscribes to nothing.
 */
export function useGroupNotices(conversationId: Id | null): RoomNotice[] {
  const { client, resetNonce } = useMigo();
  const [notices, setNotices] = useState<RoomNotice[]>([]);
  const seqRef = useRef(0);

  useEffect(() => {
    // A fresh group (or a reconnect) starts the log over: these are live arrivals, not history.
    setNotices([]);
    if (!client || conversationId === null) {
      return;
    }
    const off = client.conversations.onMember((event) => {
      if (event.conversationId !== conversationId) {
        return;
      }
      setNotices((prev) => {
        const notice: RoomNotice = {
          at: Date.now(),
          userId: event.userId,
          joined: event.change === undefined || event.change === MemberChange.Joined,
          seq: seqRef.current++,
          ...(event.change !== undefined ? { change: event.change } : {}),
        };
        const next = [...prev, notice];
        return next.length > MAX_GROUP_NOTICES ? next.slice(next.length - MAX_GROUP_NOTICES) : next;
      });
    });
    return off;
  }, [client, resetNonce, conversationId]);

  return notices;
}
