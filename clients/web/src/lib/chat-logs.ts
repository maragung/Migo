/**
 * Chat logs: the decrypted transcript as a portable artifact.
 *
 * Migo is end-to-end encrypted, which means the server holds only ciphertext — the one place a
 * readable transcript exists is this device's memory, and it exists only while the app holds it.
 * This module is the honest bridge between that fact and the wish to keep a record: it renders the
 * transcript the app has already decrypted into a plain file the person can save, and nothing here
 * ever sends that file anywhere. A log is plaintext by design, stored only on this device, and the
 * settings group that offers these controls says so in one line.
 *
 * Everything in this file is pure (plus one DOM download helper at the bottom): the transcript
 * shapes, the eviction plan for auto-saved snapshots, and the filename rules are all testable
 * without a client, exactly like the other extracted halves of the panels.
 */

import { ContentType } from '@migo/sdk';
import type { Id, MessageContent } from '@migo/sdk';

import { previewText } from './message-preview.js';

/** One rendered line of the transcript, in send order. */
export interface ChatLogEntry {
  messageId: Id;
  seq: number;
  senderId: Id;
  /** The display name the transcript showed when the line was exported. */
  senderName: string;
  /** When the message was sent, epoch ms. */
  at: number;
  /** What the bubble showed: the text, or the fixed placeholder vocabulary for other kinds. */
  body: string;
  deleted: boolean;
  edited: boolean;
}

/** One conversation's transcript as a portable artifact. */
export interface ConversationLog {
  conversationId: Id;
  /** The title the sidebar showed: a peer's name, a group's, a room's. */
  title: string;
  /** When this log was built, epoch ms — a snapshot is dated, not timeless. */
  exportedAt: number;
  messages: ChatLogEntry[];
}

/** A conversation log with the storage bookkeeping the auto-save adds. */
export interface StoredChatLog extends ConversationLog {
  /** When this snapshot was written, epoch ms — the field eviction orders by. */
  savedAt: number;
}

/** The total byte budget auto-saved snapshots may occupy. */
export const SNAPSHOT_CAP_BYTES = 2 * 1024 * 1024;

/**
 * The body one entry carries: the message-list vocabulary, or the tombstone's exact words.
 *
 * A deleted message keeps its row — the transcript on screen shows "Message deleted", and a saved
 * log that quietly dropped the row would misreport a conversation that had unsends in it. The
 * wording is the tombstone's own so a reader of the file and a reader of the app see the same
 * event named the same way.
 */
export function logBody(content: MessageContent, deleted: boolean): string {
  if (deleted) {
    return 'Message deleted';
  }
  return previewText(content);
}

/** Whether a message is transcript content rather than a protocol signal. */
export function isLoggableContent(content: MessageContent): boolean {
  return content.type !== ContentType.ControlEvent;
}

/**
 * Builds one conversation's log from the messages the app holds.
 *
 * `nameOf` is supplied by the caller because name resolution is a UI concern (the thread's
 * profiles map, "You" for ourselves); the log records whatever the caller says the transcript
 * showed. Entries land in `seq` order regardless of the order they were handed, and control
 * events are skipped — they are protocol traffic the transcript never rendered.
 */
export function buildConversationLog(
  conversationId: Id,
  title: string,
  messages: Iterable<{
    messageId: Id;
    seq: number;
    senderId: Id;
    senderDevice?: Id;
    content: MessageContent;
    createdAt: number;
    deleted?: boolean;
    editedAt?: number;
  }>,
  nameOf: (senderId: Id) => string,
  now: number = Date.now(),
): ConversationLog {
  const entries: ChatLogEntry[] = [];
  for (const message of messages) {
    if (!isLoggableContent(message.content)) {
      continue;
    }
    entries.push({
      messageId: message.messageId,
      seq: message.seq,
      senderId: message.senderId,
      senderName: nameOf(message.senderId),
      at: message.createdAt,
      body: logBody(message.content, message.deleted === true),
      deleted: message.deleted === true,
      edited: message.editedAt !== undefined,
    });
  }
  entries.sort((a, b) => a.seq - b.seq);
  return { conversationId, title, exportedAt: now, messages: entries };
}

/** `YYYY-MM-DD` in the device's own zone, so a day divider in the file matches the app's. */
function dayLabelOf(at: number): string {
  const date = new Date(at);
  const month = `${date.getMonth() + 1}`.padStart(2, '0');
  const day = `${date.getDate()}`.padStart(2, '0');
  return `${date.getFullYear()}-${month}-${day}`;
}

/**
 * Renders a log as the plain-text transcript a person can open anywhere.
 *
 * The timestamps are ISO 8601 in UTC rather than the app's clock-only format: a saved file is an
 * archival artifact, read years later and possibly far away, and the one thing an archival
 * timestamp must carry that a clock on a bubble may omit is a zone that never depended on where
 * or when it is opened again. (The day dividers below stay in the device's own zone, so the
 * grouping matches what the reader's transcript showed.) Day dividers group the lines the way the
 * transcript's do.
 */
export function formatTranscriptText(log: ConversationLog): string {
  const lines: string[] = [];
  lines.push(`# ${log.title}`);
  lines.push(
    `# Exported ${new Date(log.exportedAt).toISOString()} — ${log.messages.length} messages`,
  );
  lines.push('');
  let lastDay = '';
  for (const entry of log.messages) {
    const day = dayLabelOf(entry.at);
    if (day !== lastDay) {
      lines.push(`— ${day} —`);
      lastDay = day;
    }
    const time = new Date(entry.at).toISOString();
    const suffix = entry.edited ? ' (edited)' : '';
    lines.push(`[${time}] ${entry.senderName}: ${entry.body}${suffix}`);
  }
  return `${lines.join('\n')}\n`;
}

/** Lowercase alphanumerics and hyphens are what a filename can carry without surprise. */
const SAFE_NAME = /[^a-z0-9-]+/g;

/**
 * The name a conversation's log downloads as: `migo-log-<title>.txt` / `.json`.
 *
 * The same rule the account file uses (lib/account-file.js): the title is lowercased and
 * everything else becomes a hyphen, so no title can produce a path separator or an unclickable
 * filename; a title that sanitises to nothing (an emoji, another script) still gets a name.
 */
export function logFileName(title: string, kind: 'txt' | 'json'): string {
  const sanitized = title
    .toLowerCase()
    .replace(SAFE_NAME, '-')
    .replace(/^-+|-+$/g, '');
  return `migo-log-${sanitized === '' ? 'chat' : sanitized}.${kind}`;
}

/** The snapshot's storage footprint, in bytes as the eviction plan counts them. */
export function snapshotByteSize(snapshot: StoredChatLog): number {
  // `stringify` measures in UTF-16 code units; for the mixed text a transcript holds that is the
  // same order of magnitude as bytes, and the cap is a budget against unbounded growth, not a
  // quota the store enforces — an approximation that is stable and cheap beats an exact count
  // that would have to walk every string twice.
  return JSON.stringify(snapshot).length;
}

/**
 * Where a new snapshot leaves the stored set: replacing the same conversation's, then keeping as
 * much of the rest as the byte budget allows, oldest first out.
 *
 * The incoming snapshot is always kept — even when it alone exceeds the cap — because dropping
 * it would make auto-save silently stop for exactly the long conversations that most need it;
 * the cap bounds how *many* conversations accumulate, not whether the newest one exists. This is
 * the pure half of the store; the IndexedDB read-modify-write around it lives in
 * lib/storage/chat-log-store.js.
 */
export function planSnapshots(
  existing: readonly StoredChatLog[],
  incoming: StoredChatLog,
  capBytes: number = SNAPSHOT_CAP_BYTES,
): StoredChatLog[] {
  const kept = existing.filter((snapshot) => snapshot.conversationId !== incoming.conversationId);
  kept.push(incoming);
  kept.sort((a, b) => b.savedAt - a.savedAt);
  const out: StoredChatLog[] = [];
  let used = 0;
  for (const snapshot of kept) {
    const size = snapshotByteSize(snapshot);
    if (out.length > 0 && used + size > capBytes) {
      break;
    }
    out.push(snapshot);
    used += size;
  }
  return out;
}

/**
 * Offers text to the browser as a download.
 *
 * A no-op outside a browser (the test renderer has no `document`), and best-effort inside one,
 * the same contract the account-file download carries: a refused download is re-pressable, so it
 * is never treated as a failure of the export itself.
 */
export function downloadTextFile(text: string, filename: string, mime: string): void {
  if (typeof document === 'undefined') {
    return;
  }
  const blob = new Blob([text], { type: mime });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}
