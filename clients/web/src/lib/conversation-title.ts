'use client';

/**
 * A conversation's display title, the one way.
 *
 * The wire's `title` is optional and meaningless for direct chats (the server ignores it there),
 * so every list that names a conversation must resolve the title the same way: a 1:1 is its
 * peer's display name, a room is the room registry's name (the join or a state delta named it),
 * and a group is its own title. The conversation list, the Home digests, and Search all render
 * rows through this helper — one conversation, one name, everywhere it appears.
 */

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id, UserProfile } from '@migo/sdk';

import { roomRowTitle } from '@/components/conversation-list.js';
import type { RoomInfo } from '@/lib/migo/rooms-provider.js';

/**
 * The title a conversation renders under.
 *
 * @param conversation The conversation summary the row holds.
 * @param selfId The signed-in account, to find the 1:1's peer.
 * @param profiles Resolved profiles, for the peer's display name.
 * @param room The room registry record for a Room-kind conversation, when the shell holds one.
 */
export function conversationTitle(
  conversation: ConversationSummary,
  selfId: Id | null,
  profiles: ReadonlyMap<Id, UserProfile>,
  room: RoomInfo | null = null,
): string {
  if (conversation.kind === ConversationKind.Direct) {
    const other = conversation.members?.find((member) => member !== selfId);
    return (other !== undefined ? profiles.get(other)?.displayName : undefined) ?? 'Direct message';
  }
  if (conversation.kind === ConversationKind.Room) {
    return roomRowTitle(conversation, room);
  }
  return conversation.title ?? 'Group';
}
