'use client';

/**
 * The open room's live membership pills — "Ana joined the room", "Bekti left" — shown at the foot
 * of the transcript.
 *
 * These render in the message list's live region (its {@link MessageList} `liveSlot`, the same
 * non-message-traffic slot game activity uses), not interleaved among the message bubbles: a
 * membership change is ambient room traffic, and the live region is where the transcript already
 * puts traffic that is not a message. They read as centered system lines, newest last, and scroll
 * with the transcript.
 *
 * The component resolves each notice's name itself through the profile facility, so a name that
 * arrives after the pill does still lands on it; an unresolved member reads as "Someone".
 */

import { useMemo } from 'react';
import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { useProfiles } from '@/lib/migo/use-profiles.js';
import { memberNotice } from '@/lib/migo/use-room-notices.js';
import type { RoomNotice } from '@/lib/migo/use-room-notices.js';

export function RoomNoticeList({ notices }: { notices: RoomNotice[] }): ReactNode {
  // The distinct members named across the current tail — one profile lookup each, not one per pill.
  const ids = useMemo(() => {
    const seen = new Set<Id>();
    const out: Id[] = [];
    for (const notice of notices) {
      if (!seen.has(notice.userId)) {
        seen.add(notice.userId);
        out.push(notice.userId);
      }
    }
    return out;
  }, [notices]);
  const profiles = useProfiles(ids);

  if (notices.length === 0) {
    return null;
  }
  return (
    <ul className="room-notices" aria-label="Room activity">
      {notices.map((notice) => (
        <li key={notice.seq} className="room-notice">
          {memberNotice(
            notice.change,
            profiles.get(notice.userId)?.displayName ?? '',
            notice.joined,
          )}
        </li>
      ))}
    </ul>
  );
}
