'use client';

/**
 * The open room's membership log: who came, who went, who was shown the door.
 *
 * A room's {@link RoomsDomain.onMember} stream carries a per-member change every time someone
 * joins, leaves, drops, returns, or is removed. This hook keeps a short tail of those changes for
 * the *one* room the user is reading, so the thread can surface them as they happen — a chat's
 * ambient "X joined the room" line, not a durable record. The tail is capped ({@link MAX_NOTICES})
 * and reset whenever the open room changes or the session resets: it is a live view, never history
 * to page back through.
 *
 * The stream names a room, so the hook filters to the room it was given and drops every event for
 * another room the same connection also watches. The name a notice reads out is resolved at render
 * time from the profile facility, not stored here — a display name that arrives late still lands on
 * the pill.
 */

import { useEffect, useRef, useState } from 'react';

import { MemberChange } from '@migo/sdk';
import type { Id, RoomMemberEvent } from '@migo/sdk';

import { useMigo } from './use-migo.js';

/** How many recent membership changes the open room keeps on screen at once. */
export const MAX_NOTICES = 50;

/** One remembered membership change for the open room. */
export interface RoomNotice {
  /** When the client saw the change (wall-clock ms), for ordering and a relative label. */
  at: number;
  /** The account the change is about; resolved to a name at render time. */
  userId: Id;
  /** What happened, when the event carried it; absent on a legacy event that only set `joined`. */
  change?: MemberChange;
  /** The legacy join/leave flag, always present, so a notice renders even without a `change`. */
  joined: boolean;
  /** A monotonic key for the rendered row — arrival order, stable across re-renders. */
  seq: number;
}

/**
 * The line a membership change reads out, e.g. `"Ana joined the room"`.
 *
 * Pure, so a test can pin every branch. `name` is the resolved display name; an empty one (a member
 * whose profile has not resolved) falls back to `"Someone"` rather than printing a blank or an id.
 * `change` drives the verb; when it is absent — a legacy event, or the `Unknown` sentinel — the
 * `joined` flag is the only signal left, so the line falls back to join/leave from it. `place` names
 * where it happened — "room" or "group" — so the same projection serves both streams.
 */
export function memberNotice(
  change: MemberChange | undefined,
  name: string,
  joined = true,
  place = 'room',
): string {
  const who = name.trim().length > 0 ? name : 'Someone';
  switch (change) {
    case MemberChange.Joined:
      return `${who} joined the ${place}`;
    case MemberChange.Left:
      return `${who} left`;
    case MemberChange.Disconnected:
      return `${who} disconnected`;
    case MemberChange.Reconnected:
      return `${who} came back`;
    case MemberChange.Kicked:
      return `${who} was kicked`;
    case MemberChange.Banned:
      return `${who} was banned`;
    default:
      // No change on the wire (a legacy event) or the Unknown sentinel: the joined flag is all
      // that is left to go on.
      return joined ? `${who} joined the ${place}` : `${who} left`;
  }
}

/**
 * Keeps the open room's most recent membership changes, newest last.
 *
 * Pass the *room* id (not the conversation id); pass `null` when the open conversation is not a
 * room, and the hook holds nothing and subscribes to nothing. The tail resets when the room changes
 * or the session resets, so a reader never sees another room's churn or a stale log after a
 * reconnect.
 */
export function useRoomNotices(roomId: Id | null): RoomNotice[] {
  const { client, resetNonce } = useMigo();
  const [notices, setNotices] = useState<RoomNotice[]>([]);
  const seqRef = useRef(0);

  useEffect(() => {
    // A fresh room (or a reconnect) starts the log over: these are live arrivals, not history.
    setNotices([]);
    if (!client || roomId === null) {
      return;
    }
    const off = client.rooms.onMember((event: RoomMemberEvent) => {
      if (event.roomId !== roomId) {
        return;
      }
      setNotices((prev) => {
        const notice: RoomNotice = {
          at: Date.now(),
          userId: event.userId,
          joined: event.joined,
          seq: seqRef.current++,
          ...(event.change !== undefined ? { change: event.change } : {}),
        };
        const next = [...prev, notice];
        return next.length > MAX_NOTICES ? next.slice(next.length - MAX_NOTICES) : next;
      });
    });
    return off;
  }, [client, resetNonce, roomId]);

  return notices;
}
