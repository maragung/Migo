/**
 * What a saved chat log claims, pinned.
 *
 * A log is a plaintext artifact of an encrypted conversation — the one place the transcript
 * exists outside the app's memory — so the rules here are mostly rules about *honesty* rather
 * than mechanics:
 *
 *   1. **The log's vocabulary is the transcript's.** A media message, a voice note, a reaction,
 *      and an unsent message must appear in the file exactly as the bubble on screen named them
 *      (the 📎 / 🎤 / "Reacted" / "Message deleted" vocabulary `message-preview` owns). A log
 *      with its own dialect would describe a conversation differently than the app that
 *      supposedly witnessed it.
 *   2. **Order and completeness.** Entries land in `seq` order however they were handed, a
 *      tombstone keeps its row, and a control event — protocol traffic the transcript never
 *      rendered — never appears in a file a person will read as "what was said".
 *   3. **The eviction plan keeps the newest snapshot no matter what.** A cap that dropped the
 *      incoming snapshot for being large would make auto-save silently stop for exactly the long
 *      conversations that most need it; the cap bounds how many snapshots accumulate, not
 *      whether the newest one exists.
 *   4. **Filenames cannot surprise the filesystem.** The account file's rule (lowercase
 *      alphanumerics and hyphens, nothing that sanitises to nothing) is the log's rule too.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ContentType } from '@migo/sdk';
import type { Id, MessageContent } from '@migo/sdk';

import {
  buildConversationLog,
  formatTranscriptText,
  isLoggableContent,
  logBody,
  logFileName,
  planSnapshots,
  snapshotByteSize,
} from '../src/lib/chat-logs.js';
import type { StoredChatLog } from '../src/lib/chat-logs.js';

/** The same id text shape the other suites use. */
const CONVERSATION = '0123456789ABCDEFGHJKMNPQRS' as Id;
const OTHER = '0123456789ABCDEFGHJKMNPQRT' as Id;
const ALICE = '0123456789ABCDEFGHJKMNPRST' as Id;
const ME = '0123456789ABCDEFGHJKMNPRS2' as Id;

/** A fixed clock, so a built log is byte-comparable across runs. */
const NOW = Date.parse('2026-09-10T12:00:00Z');

function text(body: string): MessageContent {
  return { type: ContentType.Text, text: body };
}

function messageOf(overrides: {
  seq: number;
  senderId?: Id;
  content?: MessageContent;
  at?: number;
  deleted?: boolean;
  editedAt?: number;
}) {
  return {
    messageId: `msg-${overrides.seq}` as Id,
    conversationId: CONVERSATION,
    seq: overrides.seq,
    senderId: overrides.senderId ?? ALICE,
    senderDevice: 'device' as Id,
    content: overrides.content ?? text(`body ${overrides.seq}`),
    createdAt: overrides.at ?? NOW,
    ...(overrides.deleted !== undefined ? { deleted: overrides.deleted } : {}),
    ...(overrides.editedAt !== undefined ? { editedAt: overrides.editedAt } : {}),
  };
}

test('a log body speaks the transcript vocabulary, and a tombstone keeps its row', () => {
  assert.equal(logBody(text('hello there'), false), 'hello there');
  assert.equal(
    logBody(
      {
        type: ContentType.MediaRef,
        mediaId: 'media' as Id,
        mimeType: 'image/png',
        sizeBytes: 5,
        key: new Uint8Array(32),
        nonce: new Uint8Array(24),
        caption: 'the plan',
      },
      false,
    ),
    '📎 the plan',
  );
  assert.equal(
    logBody(
      {
        type: ContentType.MediaRef,
        mediaId: 'media' as Id,
        mimeType: 'image/png',
        sizeBytes: 5,
        key: new Uint8Array(32),
        nonce: new Uint8Array(24),
        caption: '  ',
      },
      false,
    ),
    '📎 Attachment',
  );
  assert.equal(
    logBody(
      {
        type: ContentType.VoiceNoteRef,
        mediaId: 'media' as Id,
        mimeType: 'audio/wav',
        sizeBytes: 5,
        durationMs: 12_000,
        key: new Uint8Array(32),
        nonce: new Uint8Array(24),
      },
      false,
    ),
    '🎤 Voice note (12s)',
  );
  assert.equal(
    logBody(
      { type: ContentType.Reaction, targetMessageId: 'm' as Id, emoji: '👍', remove: false },
      false,
    ),
    'Reacted 👍',
  );
  // The tombstone's exact words, not a second phrasing of the same event.
  assert.equal(logBody(text('never printed'), true), 'Message deleted');
});

test('a control event is protocol traffic, never log content', () => {
  const control: MessageContent = {
    type: ContentType.ControlEvent,
    event: 'sender-key',
    data: new Uint8Array(),
  };
  assert.equal(isLoggableContent(text('hi')), true);
  assert.equal(isLoggableContent(control), false);
});

test('a built log lands in seq order with the caller-named senders, and dates itself', () => {
  const log = buildConversationLog(
    CONVERSATION,
    'The Plan',
    [
      messageOf({ seq: 3, senderId: ME, editedAt: NOW + 1000 }),
      messageOf({ seq: 1 }),
      messageOf({
        seq: 2,
        content: { type: ContentType.ControlEvent, event: 'sender-key', data: new Uint8Array() },
      }),
      messageOf({ seq: 4, deleted: true }),
    ],
    (senderId) => (senderId === ME ? 'You' : 'Alice'),
    NOW,
  );
  assert.deepEqual(
    log.messages.map((entry) => entry.seq),
    [1, 3, 4],
  );
  assert.equal(log.title, 'The Plan');
  assert.equal(log.exportedAt, NOW);
  assert.equal(log.messages[0]?.senderName, 'Alice');
  assert.equal(log.messages[1]?.senderName, 'You');
  assert.equal(log.messages[1]?.edited, true);
  assert.equal(log.messages[2]?.deleted, true);
});

test('a rendered transcript carries its title, its stamp, its size, and day dividers', () => {
  // Two stamps a day apart in UTC; the divider labels are computed with the same device-local
  // formatting the renderer uses, so the case pins the grouping without pinning a timezone.
  const dayOne = Date.parse('2026-09-09T10:00:00Z');
  const dayTwo = Date.parse('2026-09-10T09:00:00Z');
  const localDay = (at: number): string => {
    const date = new Date(at);
    return `${date.getFullYear()}-${`${date.getMonth() + 1}`.padStart(2, '0')}-${`${date.getDate()}`.padStart(2, '0')}`;
  };
  const log = buildConversationLog(
    CONVERSATION,
    'The Plan',
    [messageOf({ seq: 1, at: dayOne }), messageOf({ seq: 2, at: dayTwo, senderId: ME })],
    (senderId) => (senderId === ME ? 'You' : 'Alice'),
    NOW,
  );
  const rendered = formatTranscriptText(log);
  // The header states what the file is and how much of it there is.
  assert.match(rendered, /^# The Plan\n/);
  assert.match(rendered, /# Exported 2026-09-10T12:00:00\.000Z — 2 messages/);
  // Each message names its sender and carries an archival (zone-explicit) timestamp.
  assert.match(rendered, /\[2026-09-09T10:00:00\.000Z\] Alice: body 1/);
  assert.match(rendered, /\[2026-09-10T09:00:00\.000Z\] You: body 2/);
  // Both day dividers are present, in transcript order, whatever the runner's timezone.
  const firstDivider = rendered.indexOf(`— ${localDay(dayOne)} —`);
  const secondDivider = rendered.indexOf(`— ${localDay(dayTwo)} —`);
  assert.ok(firstDivider !== -1, 'the first day has a divider');
  assert.ok(secondDivider !== -1, 'the second day has a divider');
  assert.ok(firstDivider < secondDivider, 'dividers group the lines in order');
  // An edited line says so.
  const editedLog = buildConversationLog(
    CONVERSATION,
    'The Plan',
    [messageOf({ seq: 1, editedAt: NOW })],
    () => 'Alice',
    NOW,
  );
  assert.match(formatTranscriptText(editedLog), /body 1 \(edited\)/);
});

test('a log filename survives a hostile title and never sanitises to nothing', () => {
  assert.equal(logFileName('The Plan', 'txt'), 'migo-log-the-plan.txt');
  assert.equal(logFileName('/etc/passwd!', 'json'), 'migo-log-etc-passwd.json');
  assert.equal(logFileName('😀😀', 'txt'), 'migo-log-chat.txt');
  assert.equal(logFileName('', 'json'), 'migo-log-chat.json');
});

test('the eviction plan replaces the same conversation and drops the oldest beyond the cap', () => {
  const snapshotOf = (conversationId: Id, title: string, savedAt: number): StoredChatLog => ({
    ...buildConversationLog(conversationId, title, [], () => 'Alice', NOW),
    savedAt,
  });

  // Replacing the same conversation keeps the set's size and the newer stamp.
  const oldest = snapshotOf('0123456789ABCDEFGHJKMNPQRA' as Id, 'old', NOW);
  const newer = snapshotOf('0123456789ABCDEFGHJKMNPQRB' as Id, 'new', NOW + 1);
  const replaced = planSnapshots(
    [oldest, newer],
    snapshotOf(newer.conversationId, 'new again', NOW + 5),
  );
  assert.equal(replaced.length, 2);
  assert.equal(
    replaced.find((log) => log.conversationId === newer.conversationId)?.savedAt,
    NOW + 5,
  );

  // A cap that cannot hold the big old snapshots keeps the small recent ones and drops the old:
  // eviction is oldest-first-out, never newest-first.
  const bigOld = snapshotOf('0123456789ABCDEFGHJKMNPQRC' as Id, 'c'.repeat(2000), NOW);
  const bigOlder = snapshotOf('0123456789ABCDEFGHJKMNPQRD' as Id, 'd'.repeat(2000), NOW + 1);
  const smallNew = snapshotOf('0123456789ABCDEFGHJKMNPQRE' as Id, 'e', NOW + 2);
  const cap = snapshotByteSize(bigOld) + 10;
  const evicted = planSnapshots([bigOld, bigOlder, smallNew], snapshotOf(OTHER, 'f', NOW + 3), cap);
  assert.equal(
    evicted.some((log) => log.conversationId === bigOld.conversationId),
    false,
  );
  assert.equal(
    evicted.some((log) => log.conversationId === bigOlder.conversationId),
    false,
  );
  assert.equal(
    evicted.some((log) => log.conversationId === smallNew.conversationId),
    true,
  );
  // The incoming snapshot leads the set: newest first is the order the plan keeps.
  assert.equal(evicted[0]?.conversationId, OTHER);

  // The incoming snapshot is kept even when it alone exceeds the cap: the cap bounds
  // accumulation, not existence.
  const huge = planSnapshots(
    [bigOld, bigOlder],
    snapshotOf(OTHER, 'a very long conversation', NOW + 3),
    1,
  );
  assert.equal(huge.length, 1);
  assert.equal(huge[0]?.conversationId, OTHER);
});

test('snapshot size accounting is the JSON the store will actually write', () => {
  const snapshot: StoredChatLog = {
    ...buildConversationLog(CONVERSATION, 'The Plan', [messageOf({ seq: 1 })], () => 'Alice', NOW),
    savedAt: NOW,
  };
  assert.equal(snapshotByteSize(snapshot), JSON.stringify(snapshot).length);
  assert.ok(snapshotByteSize(snapshot) > 0);
});
