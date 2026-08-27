/**
 * What the message list is allowed to turn server-controlled content into.
 *
 * Section 122 is blunt: a media message carries the sender's *claimed* `mimeType`, and it must never
 * be trusted. The web client honours this by never embedding media at all — `renderBody` turns every
 * non-text body into a short text placeholder (`📎 caption`, `🎤 Voice note`) and ignores the mime
 * type entirely. That is the safe design, and this test locks it in: a regression that "helpfully"
 * rendered `<img src=…>` or `<object>` off the sender's claim, or printed the mime type, would open
 * an XSS / content-sniffing hole that no functional test would notice, because the placeholder text
 * would still look right beside it.
 *
 * The second half is escaping. Message text, captions, and reaction emoji are attacker-controlled
 * strings from the far end of an end-to-end channel the server cannot police. Rendered as React text
 * children they are auto-escaped, so a `<script>` in a caption becomes inert text; the test feeds each
 * field a live-tag payload and asserts, against the real server-rendered HTML, that not one becomes a
 * tag. Control events — protocol signals, not chat — must produce no bubble at all.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ContentType } from '@migo/sdk';
import type { Id, IncomingMessage, MessageContent } from '@migo/sdk';

import { MessageList } from '../src/components/message-list.js';

const KEY = new Uint8Array([1, 2, 3]);
const NONCE = new Uint8Array([4, 5, 6]);
const CREATED = Date.parse('2026-08-26T12:00:00Z');

let seq = 0;
function msg(content: MessageContent): IncomingMessage {
  seq += 1;
  return {
    messageId: `msg_${seq}` as Id,
    conversationId: 'conv_1' as Id,
    seq,
    senderId: 'them' as Id,
    senderDevice: 'dev_1' as Id,
    content,
    createdAt: CREATED,
  };
}

// Every attacker-controlled string field gets a live-tag payload.
const messages: IncomingMessage[] = [
  msg({ type: ContentType.Text, text: '<script>alert(1)</script><img src=x onerror=alert(2)>' }),
  msg({
    type: ContentType.MediaRef,
    mediaId: 'media_1' as Id,
    mimeType: 'text/html', // a lying content-type the client must not act on
    sizeBytes: 10,
    key: KEY,
    nonce: NONCE,
    caption: '<svg onload=alert(3)></svg>',
  }),
  msg({
    type: ContentType.MediaRef,
    mediaId: 'media_2' as Id,
    mimeType: 'image/svg+xml', // another dangerous claimed type
    sizeBytes: 20,
    key: KEY,
    nonce: NONCE,
  }),
  msg({
    type: ContentType.VoiceNoteRef,
    mediaId: 'media_3' as Id,
    mimeType: 'audio/webm',
    sizeBytes: 30,
    durationMs: 5_000,
    key: KEY,
    nonce: NONCE,
  }),
  msg({
    type: ContentType.Reaction,
    targetMessageId: 'msg_1' as Id,
    emoji: '<b>x</b>',
    remove: false,
  }),
  // A control event: a protocol signal that must never surface as a chat bubble.
  msg({ type: ContentType.ControlEvent, event: 'sender-key', data: new Uint8Array([9, 9]) }),
];

const markup = renderToStaticMarkup(<MessageList messages={messages} selfId={'me' as Id} />);

test('no server-controlled string is ever rendered as a live HTML element', () => {
  // Because every hostile `<` is escaped to `&lt;`, the literal `<tag` opener only appears if a REAL
  // element was created — which for media/text/reactions it must never be.
  for (const tag of [
    '<script',
    '<img',
    '<svg',
    '<iframe',
    '<object',
    '<embed',
    '<video',
    '<audio',
  ]) {
    assert.ok(!markup.includes(tag), `rendered a live ${tag}> element`);
  }
});

test('a hostile caption and message body are shown, but only as inert escaped text', () => {
  // The payloads are still displayed to the user — just neutralised. Their escaped form proves it.
  assert.ok(markup.includes('&lt;script&gt;'), 'the text payload was not rendered at all');
  assert.ok(markup.includes('&lt;svg'), 'the caption payload was not rendered at all');
  assert.ok(markup.includes('&lt;b&gt;'), 'the reaction payload was not rendered at all');
});

test("the sender's claimed mime type is never trusted, and never even printed", () => {
  assert.ok(!markup.includes('text/html'));
  assert.ok(!markup.includes('image/svg+xml'));
});

test('media and voice notes appear as labelled text placeholders, not embeds', () => {
  assert.ok(markup.includes('📎'), 'a media reference lost its placeholder');
  assert.ok(markup.includes('🎤 Voice note (5s)'), 'a voice note lost its placeholder');
  // A media reference with no caption falls back to a generic label rather than an empty bubble.
  assert.ok(
    markup.includes('Attachment'),
    'a caption-less media reference lost its fallback label',
  );
});

test('control events produce no bubble at all', () => {
  // One bubble (and one clock) per visible message; the six inputs include one control event.
  const bubbles = markup.match(/class="meta"/g) ?? [];
  assert.equal(bubbles.length, 5);
  assert.ok(!markup.includes('sender-key'), 'a control-event signal leaked into the transcript');
});
