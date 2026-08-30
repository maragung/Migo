/**
 * What a voice note bubble is allowed to be in markup.
 *
 * Playback is imperative — a `new Audio` created on click, never an `<audio>` element in the tree —
 * so the server-rendered markup is the bubble's whole attack surface, and it is pinned here:
 *
 *   1. **The claimed mime type is never acted on, and never printed** — the section 122 rule the
 *      image placeholder test already polices, restated for the player.
 *   2. **A waveform becomes at most the display bar count of DOM**, no matter its sent length —
 *      the waveform bytes are sender-controlled, so a hostile 5,000-bar blob must fold, not render.
 *   3. **No `<audio>` element exists**, only the play control, the bars, and the shared `M:SS`
 *      duration label.
 *   4. **Without a URL resolver the bubble stays the text placeholder** — the same no-client
 *      fallback media references have, which is also what keeps the recorded placeholder contract
 *      (`🎤 Voice note (Ns)`) true wherever the player cannot run.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ContentType } from '@migo/sdk';
import type { Id, IncomingMessage, MessageContent } from '@migo/sdk';

import { MessageList } from '../src/components/message-list.js';
import { WAVEFORM_BARS } from '../src/lib/migo/voice.js';

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

function render(messages: IncomingMessage[], mediaUrlFor?: () => Promise<string | null>): string {
  return renderToStaticMarkup(
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
      mediaUrlFor={mediaUrlFor}
    />,
  );
}

function voiceNote(overrides: { waveform?: Uint8Array; mimeType?: string }): IncomingMessage {
  return msg({
    type: ContentType.VoiceNoteRef,
    mediaId: 'media_voice' as Id,
    mimeType: overrides.mimeType ?? 'audio/webm',
    sizeBytes: 30,
    durationMs: 34_000,
    key: KEY,
    nonce: NONCE,
    ...(overrides.waveform !== undefined ? { waveform: overrides.waveform } : {}),
  });
}

test('a voice note with a resolver renders the player, not a placeholder', () => {
  const markup = render([voiceNote({ waveform: new Uint8Array(WAVEFORM_BARS).fill(128) })], () =>
    Promise.resolve('https://media.example.test/v'),
  );
  assert.ok(markup.includes('aria-label="Play voice note"'), 'the play control lost its label');
  assert.ok(markup.includes('0:34'), 'the duration label lost its M:SS form');
  assert.equal((markup.match(/class="voice-bar"/g) ?? []).length, WAVEFORM_BARS);
});

test('the player renders no audio element — playback is imperative, created on click', () => {
  const markup = render([voiceNote({ waveform: new Uint8Array([5]) })], () =>
    Promise.resolve('https://media.example.test/v'),
  );
  assert.ok(!markup.includes('<audio'), 'an <audio> element was rendered into the tree');
});

test('a hostile waveform length folds to the display bar count, not unbounded DOM', () => {
  const markup = render([voiceNote({ waveform: new Uint8Array(5_000).fill(200) })], () =>
    Promise.resolve('https://media.example.test/v'),
  );
  assert.equal(
    (markup.match(/class="voice-bar"/g) ?? []).length,
    WAVEFORM_BARS,
    'a sender-controlled waveform length must not become DOM node count',
  );
});

test('a voice note without a waveform falls back to a progress bar', () => {
  const markup = render([voiceNote({})], () => Promise.resolve('https://media.example.test/v'));
  assert.ok(!markup.includes('voice-bar'), 'bars rendered for a waveform that was never sent');
  assert.ok(markup.includes('voice-progress'), 'the progress fallback is missing');
});

test("the sender's claimed mime type is never trusted, and never even printed", () => {
  const markup = render([voiceNote({ mimeType: 'audio/x-evil' })], () =>
    Promise.resolve('https://media.example.test/v'),
  );
  assert.ok(!markup.includes('audio/x-evil'));
});

test('without a resolver the voice note stays the text placeholder', () => {
  const markup = render([voiceNote({ waveform: new Uint8Array([5]) })]);
  assert.ok(markup.includes('🎤 Voice note (34s)'), 'the placeholder fallback was lost');
  assert.ok(!markup.includes('voice-bar'), 'a player rendered with no way to resolve media');
});
