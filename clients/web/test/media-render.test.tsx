/**
 * What the message list is allowed to turn server-controlled content into.
 *
 * Section 122 is blunt: a media message carries the sender's *claimed* `mimeType`, and it must never
 * be trusted. The web client honours this by never letting the claim pick a renderer that executes
 * anything: an image claim embeds the bytes inside an `<img>` (where neither HTML nor SVG scripts
 * run), a non-image claim renders the document row (a download anchor, never an embed), and without
 * a resolver both stay short text placeholders (`📎 caption`, `🎤 Voice note`). That is the safe
 * design, and this test locks it in: a regression that "helpfully" rendered `<img src=…>` or
 * `<object>` off the sender's claim, or printed the mime type, would open an XSS / content-sniffing
 * hole that no functional test would notice, because the placeholder text would still look right
 * beside it.
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

const markup = renderToStaticMarkup(
  <MessageList
    messages={messages}
    selfId={'me' as Id}
    showSenders={false}
    profiles={new Map()}
    readUpTo={0}
    onReply={() => {}}
    onDelete={() => {}}
    deleting={false}
    hasEarlier={false}
    loadingEarlier={false}
    onLoadEarlier={() => {}}
  />,
);

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

// --- the document row ---

/**
 * A document message (a `MediaRef` whose claimed mime is not an image) renders as the file row,
 * under the same section-122 rules as everything above: the filename is attacker-controlled text
 * that must stay inert, the claimed mime is never printed, and the renderer never embeds the bytes.
 * `renderToStaticMarkup` runs no effects, so the row below is its pending state — icon, name, and
 * size render immediately from the message; only the download link waits for the object URL.
 */
const docMarkup = renderToStaticMarkup(
  <MessageList
    messages={[
      msg({
        type: ContentType.MediaRef,
        mediaId: 'media_doc' as Id,
        mimeType: 'application/pdf', // the honest claim for this test's document
        sizeBytes: 2_048,
        key: KEY,
        nonce: NONCE,
        caption: '<script>evil.pdf</script>',
      }),
    ]}
    selfId={'me' as Id}
    showSenders={false}
    profiles={new Map()}
    readUpTo={0}
    onReply={() => {}}
    onDelete={() => {}}
    deleting={false}
    hasEarlier={false}
    loadingEarlier={false}
    onLoadEarlier={() => {}}
    mediaObjectFor={() => Promise.resolve(null)}
  />,
);

test('a document renders as the file row: name and size, never an image embed', () => {
  assert.ok(docMarkup.includes('doc-attachment'), 'the document row rendered');
  assert.ok(docMarkup.includes('doc-name'), 'the filename element rendered');
  assert.ok(docMarkup.includes('2.0 KB'), 'the size is formatted beside the name');
  // React escapes every text node, so a literal tag in this markup is an element the
  // app itself rendered — which for a document must be none of these. The row's own
  // file/download icons are svg and are not on the list for exactly that reason.
  for (const tag of ['<img', '<script', '<iframe', '<object', '<embed', '<video']) {
    assert.ok(!docMarkup.includes(tag), `rendered a live ${tag}> element for a document`);
  }
});

test('a hostile document filename is shown, but only as inert escaped text', () => {
  assert.ok(docMarkup.includes('&lt;script&gt;evil.pdf&lt;/script&gt;'), 'filename not rendered');
});

test("a document's claimed mime type is never printed", () => {
  assert.ok(!docMarkup.includes('application/pdf'), 'the claimed mime leaked into the transcript');
});
