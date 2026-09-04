'use client';

/**
 * One membership pill — "Ana joined the room", "Bekti left" — for the open room or group.
 *
 * The pill renders *inside* the transcript, as an {@link MessageList} interleaved row placed at
 * the moment the change happened, so the thread reads in time order: a join that preceded a
 * message sits above it, not below every message that follows. The component resolves its own
 * name through the profile facility, so a name that arrives after the pill does still lands on
 * it; an unresolved member reads as "Someone".
 */

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { useProfiles } from '@/lib/migo/use-profiles.js';
import { memberNotice } from '@/lib/migo/use-room-notices.js';
import type { RoomNotice } from '@/lib/migo/use-room-notices.js';

export function RoomNoticeLine({
  notice,
  place = 'room',
}: {
  notice: RoomNotice;
  /** Where the change happened — "room" or "group" — so the line says the right one. */
  place?: string;
}): ReactNode {
  const ids: Id[] = [notice.userId];
  const profiles = useProfiles(ids);
  return (
    <div className="room-notice">
      {memberNotice(
        notice.change,
        profiles.get(notice.userId)?.displayName ?? '',
        notice.joined,
        place,
      )}
    </div>
  );
}
