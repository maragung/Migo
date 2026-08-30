/**
 * What the sidebar may claim a conversation's last message said, and what the header may claim
 * about encryption.
 *
 * Both surfaces state facts to a user who cannot verify them by clicking, so both are pinned:
 *
 *   1. **The preview line only ever shows what the client actually knows.** The summary's
 *      `lastMessage` is a sealed event; its body becomes readable only through the provider's
 *      decrypt replay. When that has not (or cannot) happen, the line falls back to the event's
 *      cleartext *kind* placeholder — never to envelope bytes, a claimed mime type, or a guess. A
 *      regression that printed anything from inside the envelope would leak ciphertext onto the
 *      sidebar and read as corruption; one that guessed "Message" for every kind would quietly
 *      erase the 📎 / 🎤 / 🎉 vocabulary the bubbles also use.
 *   2. **The encryption label follows the server's `EncryptionMode`, not the conversation kind.**
 *      Kind says who is in a conversation; the mode says what protects it. A refactor that
 *      re-derived the label from `kind` (the old rule) would call an unencrypted room encrypted
 *      — exactly the claim the protocol's comment says the UI is *allowed* to make only from the
 *      mode.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ContentType, ConversationKind, EncryptionMode, MessageKind } from '@migo/sdk';
import type { ConversationSummary, Id, MessageEvent } from '@migo/sdk';

import { lastMessagePreviewLine } from '../src/components/conversation-list.js';
import { encryptionLabelFor } from '../src/components/chat-window.js';
import { messagePreview, truncate } from '../src/lib/message-preview.js';

function sealedEvent(kind: MessageKind): MessageEvent {
  return {
    messageId: 'msg_1' as Id,
    conversationId: 'conv_1' as Id,
    seq: 1,
    senderId: 'ada' as Id,
    senderDevice: 'dev_1' as Id,
    kind,
    envelope: new Uint8Array([7, 7, 7]),
    createdAt: Date.parse('2026-08-26T12:00:00Z'),
  };
}

function summary(kind: ConversationKind, lastMessage?: MessageEvent): ConversationSummary {
  return {
    conversationId: 'conv_1' as Id,
    kind,
    encryption: EncryptionMode.EndToEnd,
    lastSeq: lastMessage?.seq ?? 0,
    readSeq: 0,
    ...(lastMessage !== undefined ? { lastMessage } : {}),
  };
}

test('a decrypted text message previews its body, prefixed by the sender in a group', () => {
  const group = summary(ConversationKind.Group, sealedEvent(MessageKind.Text));
  assert.equal(
    lastMessagePreviewLine(group, { type: ContentType.Text, text: 'launch at dawn' }, 'Ada'),
    'Ada: launch at dawn',
  );
  // A 1:1 has one other voice, so the body speaks for itself.
  const direct = summary(ConversationKind.Direct, sealedEvent(MessageKind.Text));
  assert.equal(
    lastMessagePreviewLine(direct, { type: ContentType.Text, text: 'launch at dawn' }, 'Ada'),
    'launch at dawn',
  );
});

test('our own last message is prefixed as "You" in a group', () => {
  const group = summary(ConversationKind.Group, sealedEvent(MessageKind.Text));
  assert.equal(
    lastMessagePreviewLine(group, { type: ContentType.Text, text: 'on my way' }, 'You'),
    'You: on my way',
  );
});

test('a sealed body falls back to its kind placeholder, prefixed by the sender when known', () => {
  const group = summary(ConversationKind.Group, sealedEvent(MessageKind.Media));
  assert.equal(lastMessagePreviewLine(group, null, 'Ada'), 'Ada: 📎 Attachment');

  const voice = summary(ConversationKind.Direct, sealedEvent(MessageKind.Voice));
  assert.equal(lastMessagePreviewLine(voice, null, null), '🎤 Voice note');

  const gift = summary(ConversationKind.Group, sealedEvent(MessageKind.Gift));
  assert.equal(lastMessagePreviewLine(gift, null, null), '🎉 Gift');
});

test('a tombstoned last message previews as deleted, even over a stale decrypted body', () => {
  const retracted = { ...sealedEvent(MessageKind.Text), deleted: true };
  const group = summary(ConversationKind.Group, retracted);
  assert.equal(
    lastMessagePreviewLine(group, { type: ContentType.Text, text: 'taken back' }, 'Ada'),
    'Ada: [deleted]',
  );
  const direct = summary(ConversationKind.Direct, retracted);
  assert.equal(lastMessagePreviewLine(direct, null, null), '[deleted]');
});

test('a preview line is truncated as a whole, name prefix included', () => {
  const group = summary(ConversationKind.Group, sealedEvent(MessageKind.Text));
  const line = lastMessagePreviewLine(
    group,
    { type: ContentType.Text, text: 'word '.repeat(20).trim() },
    'Ada',
    40,
  );
  assert.ok(line !== null, 'a conversation with a last message must produce a preview line');
  assert.ok(line.length <= 40, `the preview line was not capped: ${line.length} chars`);
  assert.ok(line.endsWith('…'), 'a truncated preview lost its ellipsis');
  assert.ok(line.startsWith('Ada: '), 'truncation ate the sender prefix');
});

test('with no last message and nothing decrypted there is no preview to show', () => {
  const empty = summary(ConversationKind.Direct);
  assert.equal(lastMessagePreviewLine(empty, null, 'Ada'), null);
  const group = summary(ConversationKind.Group);
  assert.equal(lastMessagePreviewLine(group, null, 'Ada'), null);
});

test('a non-text body previews as its label, and truncation is word-aware', () => {
  const long = messagePreview(
    { type: ContentType.Text, text: 'the quick brown fox jumps over the lazy dog again' },
    20,
  );
  assert.equal(long, 'the quick brown…');
  assert.equal(
    messagePreview(
      {
        type: ContentType.MediaRef,
        mediaId: 'media_1' as Id,
        mimeType: 'text/html',
        sizeBytes: 1,
        key: new Uint8Array(),
        nonce: new Uint8Array(),
        caption: '  ',
      },
      40,
    ),
    '📎 Attachment',
  );
  // The truncation helper is exported for combined lines; it never widens a short one.
  assert.equal(truncate('short', 40), 'short');
});

test('the encryption label follows the summary\u2019s EncryptionMode, not the kind', () => {
  assert.equal(encryptionLabelFor(EncryptionMode.EndToEnd), '🔒 End-to-end encrypted');
  assert.equal(
    encryptionLabelFor(EncryptionMode.Transport),
    'Encrypted transport (server can read for moderation)',
  );
  assert.equal(encryptionLabelFor(EncryptionMode.None), 'Not encrypted');
  // Unknown is the server saying "do not claim anything"; no label is rendered for it.
  assert.equal(encryptionLabelFor(EncryptionMode.Unknown), null);
  assert.equal(encryptionLabelFor(undefined), null);
});
