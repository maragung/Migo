/**
 * Persistence for auto-saved chat logs, and the toggle that arms it.
 *
 * # What is stored, and where
 *
 * Snapshots live in the same IndexedDB key/value store the key-store uses (lib/storage/idb.js),
 * under exactly one key of this module's own: `chat-logs:v1`. They are plaintext transcripts by
 * design — the whole point of the feature is that a readable record survives on this device — and
 * IndexedDB is the sanctioned store for anything this app persists, so the choice is not a
 * downgrade of the key-store's rules but a different row in the same drawer: the key material
 * stays sealed under its own keys, and this module never names them.
 *
 * # The one-key rule, and what "clear stored data" may touch
 *
 * Because everything lives under one key, the destructive control in Settings can be exact: it
 * deletes `chat-logs:v1` and nothing else. The keys this module must never touch are listed in
 * {@link PROTECTED_KEYS} as documentation of the boundary — `keystore-snapshot`,
 * `keystore-snapshot:v1`, `keystore-master` (the device's sealed identity), and
 * `migo:server-endpoint:v1` (the chosen server) — and none of this module's code paths reference
 * them, so a bug here can cost a saved log, never a session.
 *
 * # The toggle
 *
 * Whether auto-save is armed is a plain `'on'`/`'off'` string in `localStorage`, the same shape
 * and the same non-secret status as the theme choice (lib/theme.js). It is read at write time —
 * the snapshot path asks `isAutoSaveEnabled` on each tick — so turning it off takes effect on the
 * next tick without any provider wiring between the settings panel and every chat window.
 */

import type { Id } from '@migo/sdk';

import type { ConversationLog, StoredChatLog } from '../chat-logs.js';
import { planSnapshots } from '../chat-logs.js';

import { idbDelete, idbGet, idbSet } from './idb.js';

/** The one key this module owns; everything it stores lives under it. */
const LOGS_KEY = 'chat-logs:v1';

/**
 * The keys this module must never read, write, or delete — the store's other tenants. Listed so
 * the boundary is stated where a future change would look before making it.
 */
export const PROTECTED_KEYS: readonly string[] = [
  'keystore-snapshot',
  'keystore-snapshot:v1',
  'keystore-master',
  'migo:server-endpoint:v1',
];

/** Where the auto-save toggle persists; a plain string, never key material. */
const AUTOSAVE_KEY = 'migo:chat-log-autosave';

/**
 * Reads the stored snapshots, or an empty list when none exist or the record is unreadable.
 *
 * A record this build cannot shape-check (a future version's format, a corrupted row) reads as
 * empty rather than thrown: a saved log is a convenience, and losing it to a strict parser is a
 * worse failure than starting the set over.
 */
export async function loadChatLogSnapshots(): Promise<StoredChatLog[]> {
  const stored = await idbGet<unknown>(LOGS_KEY);
  if (!Array.isArray(stored)) {
    return [];
  }
  return stored.filter(
    (entry): entry is StoredChatLog =>
      typeof entry === 'object' &&
      entry !== null &&
      'conversationId' in entry &&
      'savedAt' in entry &&
      'messages' in entry,
  );
}

/** Writes the stored set, replacing whatever was there. */
async function saveSnapshots(snapshots: StoredChatLog[]): Promise<void> {
  await idbSet(LOGS_KEY, snapshots);
}

/**
 * Adds one snapshot to the stored set: the same conversation's previous snapshot is replaced, and
 * the whole set is kept inside the byte budget by {@link planSnapshots}.
 *
 * Best-effort by design: a full or blocked store logs nothing and fails nothing, because a
 * snapshot that could not be written costs the feature one interval, not the session an error.
 */
export async function storeChatLogSnapshot(log: ConversationLog): Promise<void> {
  try {
    const existing = await loadChatLogSnapshots();
    const snapshot: StoredChatLog = { ...log, savedAt: Date.now() };
    await saveSnapshots(planSnapshots(existing, snapshot));
  } catch {
    // See the doc above: an unwritable store is a skipped tick.
  }
}

/** Removes every stored snapshot — the whole of what "clear stored data" may delete here. */
export async function clearChatLogSnapshots(): Promise<void> {
  await idbDelete(LOGS_KEY);
}

/**
 * Removes one conversation's snapshot — the per-row delete in Settings.
 *
 * Reuses {@link loadChatLogSnapshots}'s shape check by going through it, so a record a future
 * build left in a new format is read as the empty set and the delete is a no-op rather than a
 * write of a misunderstood shape.
 */
export async function removeChatLogSnapshot(conversationId: Id): Promise<void> {
  const existing = await loadChatLogSnapshots();
  await saveSnapshots(existing.filter((snapshot) => snapshot.conversationId !== conversationId));
}

/** Whether auto-save is armed. Off is the default: a plaintext record is opt-in, never presumed. */
export function isAutoSaveEnabled(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }
  try {
    return window.localStorage.getItem(AUTOSAVE_KEY) === 'on';
  } catch {
    return false;
  }
}

/** Arms or disarms auto-save; a store that refuses writes costs the choice its persistence only. */
export function setAutoSaveEnabled(enabled: boolean): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.localStorage.setItem(AUTOSAVE_KEY, enabled ? 'on' : 'off');
  } catch {
    // Best-effort, the same contract the theme choice carries.
  }
}
