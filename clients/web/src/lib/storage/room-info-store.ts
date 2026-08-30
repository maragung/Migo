/**
 * Persistence for the room metadata the chat shell keeps beside its conversation list.
 *
 * A room's conversation summary arrives from the server with no room attached: the conversation
 * list carries neither the room's id nor its name, its topic, nor its counters — the only wire
 * moment both are visible together is the join reply. So the shell remembers the projection
 * itself, and this module is where it survives a reload: a room joined weeks ago would otherwise
 * render as an anonymous "Room" row forever, because nothing on the wire would ever name it
 * again.
 *
 * The record is scoped to the account. Room names and topics are public facts, but they are
 * also a map of where *this* account spends its time, and the next account on the same device
 * profile has no business inheriting it — a stored copy that names a different account is
 * discarded on load rather than merged.
 *
 * IndexedDB, like every other store in this app: the values are plain room metadata (no key
 * material, no credentials), but Web Storage remains forbidden surface by the audit rules this
 * client holds itself to, and one storage discipline is easier to keep than two.
 */

import type { Id } from '@migo/sdk';

import type { RoomInfo } from '@/lib/migo/rooms-provider.js';

import { idbDelete, idbGet, idbSet } from './idb.js';

const KEY = 'room-info';

/** The persisted record: which account's rooms these are, and the rooms themselves. */
export interface PersistedRoomInfo {
  accountId: Id;
  /** Conversation id → the room the conversation is. */
  rooms: Record<string, RoomInfo>;
}

/** Loads the persisted room metadata, or `undefined` on a first visit. */
export function loadRoomInfo(): Promise<PersistedRoomInfo | undefined> {
  return idbGet<PersistedRoomInfo>(KEY);
}

/** Persists the room metadata record for `accountId`. */
export function saveRoomInfo(accountId: Id, rooms: Record<string, RoomInfo>): Promise<void> {
  return idbSet(KEY, { accountId, rooms });
}

/** Removes the persisted room metadata (a different account's copy must not survive). */
export function clearRoomInfo(): Promise<void> {
  return idbDelete(KEY);
}
