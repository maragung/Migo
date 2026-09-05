/**
 * What a message row is allowed to say about deletion, reads, and replies.
 *
 * Three rendering rules carry correctness weight and would silently regress under a "helpful"
 * refactor, so they are pinned here against the real server-rendered markup:
 *
 *   1. **A tombstone shows no content, ever.** The deletion stream replaces a message the sender
 *      unsent; the row that replaces it must keep the message's place in the thread but reveal
 *      nothing of what it said. A regression that kept rendering the body (or the reply quote
 *      above it) would un-delete the message for everyone who had not read it yet — invisible to
 *      any functional test, because the list would still look fine.
 *   2. **Read ticks read the watermark, not the clock.** Our own messages show one tick until the
 *      peer's Read receipt covers their sequence, two after. The marker must be derived from
 *      `seq <= readUpTo` exactly; an off-by-one marks the boundary message read when it is not.
 *   3. **A reply quotes its target or admits it is gone.** The quote resolves the target in the
 *      same thread; a target that is absent or since tombstoned renders as "[deleted]" — never an
 *      empty quote (which reads as corruption) and never the deleted target's text.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ContentType } from '@migo/sdk';
import type { Id, UserProfile } from '@migo/sdk';

import { MessageList } from '../src/components/message-list.js';
import type { InterleavedRow } from '../src/components/message-list.js';
import type { ThreadMessage } from '../src/lib/migo/use-chat.js';

const CREATED = Date.parse('2026-08-26T12:00:00Z');

const PROFILES = new Map<Id, UserProfile>([
  ['ada' as Id, { userId: 'ada' as Id, publicId: 'MGO-ADA', username: 'ada', displayName: 'Ada' }],
]);

let seq = 0;
function msg(fields: {
  senderId: Id;
  text: string;
  deleted?: boolean;
  replyTo?: Id;
  createdAt?: number;
}): ThreadMessage {
  seq += 1;
  return {
    messageId: `msg_${seq}` as Id,
    conversationId: 'conv_1' as Id,
    seq,
    senderId: fields.senderId,
    senderDevice: 'dev_1' as Id,
    content: { type: ContentType.Text, text: fields.text },
    createdAt: fields.createdAt ?? CREATED,
    ...(fields.deleted ? { deleted: true } : {}),
    ...(fields.replyTo ? { replyTo: fields.replyTo } : {}),
  };
}

function render(
  messages: ThreadMessage[],
  props: { readUpTo?: number; showSenders?: boolean; interleaved?: InterleavedRow[] } = {},
): string {
  return renderToStaticMarkup(
    <MessageList
      messages={messages}
      selfId={'me' as Id}
      showSenders={props.showSenders ?? false}
      profiles={PROFILES}
      readUpTo={props.readUpTo ?? 0}
      onReply={() => {}}
      onDelete={() => {}}
      deleting={false}
      hasEarlier={false}
      loadingEarlier={false}
      onLoadEarlier={() => {}}
      interleaved={props.interleaved}
    />,
  );
}

test('a deleted message becomes a content-free tombstone in place', () => {
  const before = msg({ senderId: 'ada' as Id, text: 'before the unsent one' });
  const deleted = msg({ senderId: 'ada' as Id, text: 'this was retracted', deleted: true });
  const after = msg({ senderId: 'me' as Id, text: 'after it' });
  const markup = render([before, deleted, after]);

  assert.ok(markup.includes('Message deleted'), 'the tombstone row is missing');
  assert.ok(!markup.includes('this was retracted'), 'a tombstone leaked its content');
  assert.ok(
    markup.includes('class="bubble tombstone"'),
    'the tombstone row lost its distinguishing style',
  );
  // Position is preserved: the tombstone sits between its neighbours, not at the end.
  const at = (needle: string): number => markup.indexOf(needle);
  assert.ok(
    at('before the unsent one') < at('Message deleted') && at('Message deleted') < at('after it'),
    'the tombstone did not keep the deleted message\u2019s position',
  );
});

test('our own messages carry one tick until the peer\u2019s Read watermark covers them', () => {
  const covered = msg({ senderId: 'me' as Id, text: 'read by the peer' });
  const pending = msg({ senderId: 'me' as Id, text: 'not yet read' });
  // The watermark stops between the two: the first is read, the second only sent.
  const markup = render([covered, pending], { readUpTo: covered.seq });

  assert.ok(markup.includes('>✓✓<'), 'a read message lost its double tick');
  assert.ok(markup.includes('>✓<'), 'an unread own message lost its single tick');
  // The boundary is inclusive: seq <= readUpTo is read, and the labels say which is which.
  assert.ok(markup.includes('title="Read"'), 'the read tick lost its accessible label');
  assert.ok(markup.includes('title="Sent"'), 'the sent tick lost its accessible label');
  assert.equal((markup.match(/title="Read"/g) ?? []).length, 1);
  assert.equal((markup.match(/title="Sent"/g) ?? []).length, 1);
  // Only our own messages are marked — an inbound message carries no ticks at all.
  const inbound = render([msg({ senderId: 'ada' as Id, text: 'their words' })]);
  assert.ok(!inbound.includes('✓'), 'an inbound message was given a delivery marker');
});

test('a reply quotes its target above the bubble', () => {
  const target = msg({ senderId: 'ada' as Id, text: 'the quoted line' });
  const reply = msg({ senderId: 'me' as Id, text: 'the answer', replyTo: target.messageId });
  const markup = render([target, reply]);

  assert.ok(markup.includes('class="reply-quote"'), 'the reply row lost its quote');
  assert.ok(markup.includes('the quoted line'), 'the quote does not show the target\u2019s text');
  assert.ok(markup.includes('Ada'), 'the quote does not name who is quoted');
});

test('a reply whose target is gone, or since deleted, quotes "[deleted]"', () => {
  const live = msg({ senderId: 'ada' as Id, text: 'still here' });
  const gone = msg({ senderId: 'me' as Id, text: 'reply to nothing', replyTo: 'msg_ghost' as Id });
  const deleted = msg({ senderId: 'ada' as Id, text: 'was here', deleted: true });
  const toDeleted = msg({
    senderId: 'me' as Id,
    text: 'reply to a tombstone',
    replyTo: deleted.messageId,
  });
  const markup = render([live, gone, deleted, toDeleted]);

  const quotes = markup.match(/class="reply-quote-text">([^<]*)</g) ?? [];
  assert.equal(quotes.length, 2, 'each reply should render exactly one quote');
  for (const quote of quotes) {
    assert.ok(quote.includes('[deleted]'), `a vanished target was quoted as: ${quote}`);
  }
  assert.ok(!markup.includes('was here'), 'a deleted target\u2019s text leaked into a quote');
});

test('a group thread names every line, with an avatar only on the run head', () => {
  // The transcript is a script, not a stack of bubbles: the design repeats the nickname on every
  // line, because that is what a reader scans for in a room where six people are talking. What
  // *is* run-gated is the avatar — the 24px gutter is reserved on every line and filled only on
  // the first of a run, so the text column starts at one x either way.
  //
  // This deliberately inverts the older convention, where the name headed a run and our own
  // messages carried no name at all. Both changed together: a self line reads "You: …" like
  // everyone else's, in the fixed teal rather than a hashed colour.
  const run = [
    msg({ senderId: 'ada' as Id, text: 'first from Ada' }),
    msg({ senderId: 'ada' as Id, text: 'second from Ada' }),
    msg({ senderId: 'me' as Id, text: 'our own words' }),
  ];
  const markup = render(run, { showSenders: true });

  const names = markup.match(/class="sender-name /g) ?? [];
  assert.equal(names.length, 3, 'every line should open with its sender name');
  assert.ok(markup.includes('first from Ada'));
  assert.ok(markup.includes('second from Ada'));
  // Two run heads — Ada's first message and ours — so two avatars, not three.
  const avatars = markup.match(/class="avatar"/g) ?? [];
  assert.equal(avatars.length, 2, 'exactly the run heads should carry an avatar');

  // The name colour is a hash of the name, so one person keeps one colour down the transcript;
  // our own lines take the fixed self colour instead of joining the cycle.
  const adaColours = new Set(markup.match(/class="sender-name nick-\d"/g) ?? []);
  assert.equal(adaColours.size, 1, 'one sender should hash to one colour');
  assert.ok(markup.includes('class="sender-name nick-self"'), 'our own line should be self-marked');
});

test('an interleaved system row sits between the messages around it, in time order', () => {
  // The join happened between the two messages, so the pill must read between them — not under
  // the thread, where a live-region pile would put it below even the message that followed it.
  const before = msg({ senderId: 'ada' as Id, text: 'before the join' });
  const after = msg({
    senderId: 'ada' as Id,
    text: 'after the join',
    createdAt: CREATED + 60_000,
  });
  const markup = render([before, after], {
    interleaved: [{ at: CREATED + 30_000, key: 'room-1', node: 'Bekti joined the room' }],
  });

  const at = (needle: string): number => markup.indexOf(needle);
  assert.ok(
    at('before the join') < at('Bekti joined the room') &&
      at('Bekti joined the room') < at('after the join'),
    'a membership notice did not sit between the messages around it',
  );
});

test('an interleaved row older than every loaded message still renders, ahead of them', () => {
  const only = msg({ senderId: 'ada' as Id, text: 'the only message' });
  const markup = render([only], {
    interleaved: [{ at: CREATED - 60_000, key: 'room-0', node: 'Bekti came back' }],
  });

  const at = (needle: string): number => markup.indexOf(needle);
  assert.ok(
    at('Bekti came back') < at('the only message'),
    'an early notice was dropped or misplaced',
  );
});

test('an interleaved system line breaks the sender run', () => {
  // A notice between two messages from one sender reads as a new turn, so the message after it is
  // a run head again. Now that the name repeats on every line, the run is visible in the *avatar*:
  // without the notice Ada's second line would carry none, with it she carries a second face.
  const first = msg({ senderId: 'ada' as Id, text: 'first from Ada' });
  const second = msg({
    senderId: 'ada' as Id,
    text: 'second from Ada',
    createdAt: CREATED + 60_000,
  });
  const unbroken = render([first, second], { showSenders: true });
  const broken = render([first, second], {
    showSenders: true,
    interleaved: [{ at: CREATED + 30_000, key: 'room-1', node: 'Bekti left' }],
  });

  assert.equal(
    (unbroken.match(/class="avatar"/g) ?? []).length,
    1,
    'an unbroken run should draw one avatar',
  );
  assert.equal(
    (broken.match(/class="avatar"/g) ?? []).length,
    2,
    'a system line should break the sender run',
  );
});

test('the load-earlier control appears only when history is missing, and shows its busy state', () => {
  const idle = render([msg({ senderId: 'ada' as Id, text: 'hi' })]);
  assert.ok(!idle.includes('Load earlier'), 'a complete thread offered paging');

  const paged = renderToStaticMarkup(
    <MessageList
      messages={[msg({ senderId: 'ada' as Id, text: 'hi' })]}
      selfId={'me' as Id}
      showSenders={false}
      profiles={PROFILES}
      readUpTo={0}
      onReply={() => {}}
      onDelete={() => {}}
      deleting={false}
      hasEarlier
      loadingEarlier={false}
      onLoadEarlier={() => {}}
    />,
  );
  assert.ok(paged.includes('Load earlier messages'), 'the paging control is missing');
  assert.ok(!paged.includes('disabled'), 'an idle paging control must not be disabled');

  const busy = renderToStaticMarkup(
    <MessageList
      messages={[]}
      selfId={'me' as Id}
      showSenders={false}
      profiles={PROFILES}
      readUpTo={0}
      onReply={() => {}}
      onDelete={() => {}}
      deleting={false}
      hasEarlier
      loadingEarlier
      onLoadEarlier={() => {}}
    />,
  );
  assert.ok(busy.includes('Loading…'), 'the busy label is missing');
  assert.ok(busy.includes('disabled'), 'a busy paging control must not be clickable');
});
