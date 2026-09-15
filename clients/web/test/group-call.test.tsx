/**
 * What the group-call surface is allowed to say, and when it is allowed to exist.
 *
 * The tests pin four layers, each against the rule that would silently regress under an
 * innocent-looking refactor:
 *
 *   1. **The roster projection.** One seat per account is a server rule the client only observes —
 *      so an arrival for a known account replaces that seat (and moves it to the end, because a
 *      replaced seat is a fresh join), an arrival for a new account appends, and a departure
 *      removes. A departure that empties the call retires it, and a departure naming this
 *      session's exact account *and* device is the seat being replaced from this account's other
 *      device — a different fact from "the call ended", the same way section 180 keeps the 1:1
 *      ended reasons apart.
 *   2. **The roster screen, state by state.** Every state names itself — *Joining…*, the
 *      participant count, or one of four distinct notes — and the "you" seat is findable in the
 *      list. The placeholder offer this build joins with is a real sealed envelope (the wire rule
 *      is about what the server sees, not about whether the media plane has landed).
 *   3. **The join button's gate.** The group-call control exists only where a group conversation
 *      is — the chat header passes the conversation id for a `Group` conversation and nothing for
 *      any other kind, so the button appears precisely where the SFU's membership gate can admit
 *      the joiner.
 *   4. **The call in progress, as a member who is not seated hears it.** The same announcements
 *      that keep a roster true also tell a not-yet-seated member a call is running — and the join
 *      that answers it must reuse the running call's id, because a fresh id would mint a second
 *      call the conversation did not ask for.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';
import { isValidElement } from 'react';
import type { ReactNode } from 'react';

import { CallMediaKind } from '@migo/sdk';
import type { GroupCallJoinedEvent, GroupCallLeftEvent, GroupCallRoster, Id } from '@migo/sdk';

import { GroupCallButton } from '../src/components/call-buttons.js';
import { GroupCallScreen } from '../src/components/group-call-overlay.js';
import type { GroupCallScreenProps } from '../src/components/group-call-overlay.js';
import { GroupCallManagerProvider, useGroupCall } from '../src/lib/migo/group-call-manager.js';
import type { ActiveGroupCall } from '../src/lib/migo/group-call-manager.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import {
  GROUP_CALL_NOTES,
  groupCallNoteLabel,
  inProgressArrived,
  inProgressDeparted,
  isCallRetired,
  namesOwnSeat,
  placeholderSealedOffer,
  seatArrived,
  seatDeparted,
  seatsFromSnapshot,
} from '../src/lib/migo/group-roster.js';
import {
  generateCallKey,
  openCallSignal,
  decodeSdpDescription,
} from '../src/lib/migo/call-signal.js';

const ME = 'me' as Id;
const ME_DEVICE = 'me_phone' as Id;
const ADA = 'ada' as Id;
const BEN = 'ben' as Id;
const CALL = 'call_1' as Id;
const CONVERSATION = 'conv_1' as Id;
const NOW = Date.parse('2026-09-12T12:00:00Z');

// --- fixtures ---

function snapshot(participants: GroupCallRoster['participants']): GroupCallRoster {
  return {
    callId: CALL,
    conversationId: CONVERSATION,
    userId: ME,
    deviceId: ME_DEVICE,
    participantCount: participants.length,
    participants,
  };
}

function joined(overrides: Partial<GroupCallJoinedEvent> = {}): GroupCallJoinedEvent {
  return {
    callId: CALL,
    conversationId: CONVERSATION,
    userId: BEN,
    deviceId: 'ben_phone' as Id,
    participantCount: 2,
    ...overrides,
  };
}

function departed(overrides: Partial<GroupCallLeftEvent> = {}): GroupCallLeftEvent {
  return {
    callId: CALL,
    conversationId: CONVERSATION,
    userId: BEN,
    deviceId: 'ben_phone' as Id,
    participantCount: 1,
    ...overrides,
  };
}

function activeCall(overrides: Partial<ActiveGroupCall> = {}): ActiveGroupCall {
  return {
    callId: CALL,
    conversationId: CONVERSATION,
    mediaKind: CallMediaKind.Audio,
    phase: 'seated',
    note: null,
    seats: [
      { userId: ME, deviceId: ME_DEVICE, joinedAt: NOW - 60_000 },
      { userId: ADA, deviceId: 'ada_laptop' as Id, joinedAt: NOW - 30_000 },
    ],
    participantCount: 2,
    joinedAt: NOW - 60_000,
    ...overrides,
  };
}

function screen(overrides: Partial<GroupCallScreenProps> = {}): string {
  const props: GroupCallScreenProps = {
    call: activeCall(),
    names: new Map<Id, string>([
      [ME, 'Me First'],
      [ADA, 'Ada Lovelace'],
    ]),
    meId: ME,
    nowMs: NOW,
    onLeave: () => {},
    onDismiss: () => {},
    ...overrides,
  };
  return renderToStaticMarkup(<GroupCallScreen {...props} />);
}

// --- the placeholder offer ---

test('the join offer is a real sealed envelope, even though it is a placeholder', () => {
  const key = generateCallKey();
  const sealed = placeholderSealedOffer(key, CALL);

  // Version 2, the house AEAD's own output behind it — the same envelope a media-bearing offer
  // travels in, so the server (and every other participant) sees nothing but opaque bytes.
  assert.equal(sealed[0], 2, 'the envelope version byte');
  const opened = decodeSdpDescription(openCallSignal(sealed, key, CALL));
  assert.equal(opened.type, 'offer');
  assert.equal(opened.sdp, '', 'the placeholder carries an empty description, honestly');
  // Bound to its call, like every sealed call signal: it must not open as another call's.
  assert.throws(() => openCallSignal(sealed, key, 'call_other' as Id));
});

// --- the roster projection ---

test('the snapshot builds the roster in the server’s join order', () => {
  const roster = snapshot([
    {
      userId: ADA,
      deviceId: 'ada_laptop' as Id,
      joinedAt: NOW - 30_000,
      sealedOffer: new Uint8Array([2]),
    },
    { userId: ME, deviceId: ME_DEVICE, joinedAt: NOW - 60_000, sealedOffer: new Uint8Array([2]) },
  ]);
  const seats = seatsFromSnapshot(roster);
  assert.deepEqual(
    seats.map((seat) => seat.userId),
    [ADA, ME],
    'the snapshot’s order is kept verbatim, not re-derived',
  );
  assert.equal(seats[0]?.deviceId, 'ada_laptop');
  assert.equal(seats[0]?.joinedAt, NOW - 30_000);
});

test('an arrival appends a new account and replaces a same-account seat, never a second seat', () => {
  const seats = seatsFromSnapshot(
    snapshot([
      { userId: ME, deviceId: ME_DEVICE, joinedAt: NOW - 60_000, sealedOffer: new Uint8Array([2]) },
      {
        userId: ADA,
        deviceId: 'ada_laptop' as Id,
        joinedAt: NOW - 30_000,
        sealedOffer: new Uint8Array([2]),
      },
    ]),
  );

  // A new account: appended at the end, in join order.
  const withBen = seatArrived(seats, joined({ participantCount: 3 }), NOW);
  assert.deepEqual(
    withBen.map((seat) => seat.userId),
    [ME, ADA, BEN],
  );

  // The same account on a new device: one seat per account, so the seat is replaced — and the
  // replacement is a fresh join, so it takes the end of the join order.
  const adaMoved = seatArrived(
    withBen,
    joined({ userId: ADA, deviceId: 'ada_phone' as Id, participantCount: 3 }),
    NOW,
  );
  assert.deepEqual(
    adaMoved.map((seat) => `${seat.userId}:${seat.deviceId}`),
    [`${ME}:${ME_DEVICE}`, 'ben:ben_phone', 'ada:ada_phone'],
  );
});

test('a departure removes the seat; a count of zero retires the call', () => {
  const seats = seatsFromSnapshot(
    snapshot([
      { userId: ME, deviceId: ME_DEVICE, joinedAt: NOW - 60_000, sealedOffer: new Uint8Array([2]) },
      {
        userId: ADA,
        deviceId: 'ada_laptop' as Id,
        joinedAt: NOW - 30_000,
        sealedOffer: new Uint8Array([2]),
      },
    ]),
  );
  const withoutAda = seatDeparted(seats, departed({ userId: ADA, participantCount: 1 }));
  assert.deepEqual(
    withoutAda.map((seat) => seat.userId),
    [ME],
  );
  assert.equal(
    isCallRetired(departed({ participantCount: 0 })),
    true,
    'the last seat retired the call',
  );
  assert.equal(isCallRetired(departed({ participantCount: 1 })), false);
});

test('only the exact account and device names this session’s own seat', () => {
  const me = { accountId: ME, deviceId: ME_DEVICE };
  assert.equal(namesOwnSeat({ userId: ME, deviceId: ME_DEVICE }, me), true);
  // Same account, another device: that seat’s movement is roster news, not this screen’s end.
  assert.equal(namesOwnSeat({ userId: ME, deviceId: 'me_laptop' as Id }, me), false);
  assert.equal(namesOwnSeat({ userId: ADA, deviceId: 'ada_laptop' as Id }, me), false);
});

test('the four notes stay distinct, none a bare “call ended”', () => {
  const labels = new Set(Object.values(GROUP_CALL_NOTES));
  assert.equal(labels.size, 4, 'every note is a different fact');
  assert.equal(groupCallNoteLabel('left'), 'You left the call');
  assert.equal(groupCallNoteLabel('ended'), 'The call ended');
  assert.equal(groupCallNoteLabel('moved'), 'Continued on another device');
  assert.equal(groupCallNoteLabel('connection'), 'Connection lost');
});

// --- the roster screen, state by state ---

test('a joining screen names itself, renders no roster yet, and offers only cancel', () => {
  const markup = screen({
    call: activeCall({ phase: 'joining', seats: [], participantCount: 0, joinedAt: null }),
  });
  assert.ok(markup.includes('Joining…'), 'the phase must say itself');
  assert.ok(!markup.includes('in this call'), 'no count before there is a seat');
  assert.ok(markup.includes('aria-label="Cancel joining the call"'));
  assert.ok(!markup.includes('aria-label="Leave the call"'));
});

test('a seated screen states the count, marks the roster’s you, and offers leave', () => {
  const markup = screen();
  assert.ok(markup.includes('2 in this call'));
  assert.ok(markup.includes('Ada Lovelace'), 'the roster renders names');
  assert.ok(markup.includes('You'), 'this session’s seat is findable');
  assert.ok(markup.includes('aria-label="Leave the call"'));
  assert.ok(markup.includes('Roster only in this build'), 'no media is stated, not hidden');
  // The duration reads the passed clock: one minute seated renders 1:00.
  assert.ok(markup.includes('1:00'));
});

test('an ended screen shows its note and a way back, not a leave control', () => {
  const markup = screen({ call: activeCall({ note: 'ended' }) });
  assert.ok(markup.includes('The call ended'));
  assert.ok(markup.includes('Back to chats'));
  assert.ok(!markup.includes('aria-label="Leave the call"'));
  // The other notes are different facts, each with its own words.
  assert.ok(
    screen({ call: activeCall({ note: 'moved' }) }).includes('Continued on another device'),
  );
  assert.ok(screen({ call: activeCall({ note: 'connection' }) }).includes('Connection lost'));
  assert.ok(screen({ call: activeCall({ note: 'left' }) }).includes('You left the call'));
});

// --- the join button's gate ---

test('the group-call button exists only where a group conversation is', () => {
  assert.equal(
    renderToStaticMarkup(
      <GroupCallButton conversationId={null} inProgress={null} onJoin={() => Promise.resolve()} />,
    ),
    '',
    'no group conversation, no button',
  );
  const markup = renderToStaticMarkup(
    <GroupCallButton
      conversationId={CONVERSATION}
      inProgress={null}
      onJoin={() => Promise.resolve()}
    />,
  );
  assert.ok(markup.includes('aria-label="Join group call"'));
  // One button, not a voice/video pair: this build renders the roster, and a video button would
  // promise video it cannot show.
  assert.equal(markup.match(/<button/g)?.length ?? 0, 1);
});

// --- a call in progress, as a member who is not seated hears it ---

test('announcements keep the call a member is not seated in, and its count', () => {
  // The first seat opens the conversation's entry: a member who never joined now knows a call
  // is running, which call it is, and how big — the whole "join in progress" fact.
  let tracked = inProgressArrived(new Map(), joined({ participantCount: 1 }));
  assert.deepEqual(tracked.get(CONVERSATION), {
    callId: CALL,
    participantCount: 1,
  });
  // Every movement after that is a count update on the same entry.
  tracked = inProgressArrived(
    tracked,
    joined({ userId: ADA, deviceId: 'ada_laptop' as Id, participantCount: 2 }),
  );
  assert.deepEqual(tracked.get(CONVERSATION), { callId: CALL, participantCount: 2 });
  tracked = inProgressDeparted(tracked, departed({ userId: ADA, participantCount: 1 }));
  assert.deepEqual(tracked.get(CONVERSATION), { callId: CALL, participantCount: 1 });
});

test('the last seat retires the tracked call; a fresh call replaces the entry', () => {
  let tracked = inProgressArrived(new Map(), joined());
  // Zero is the retirement: nothing is left to join, so the entry goes entirely rather than
  // sitting at a count no join can answer.
  tracked = inProgressDeparted(tracked, departed({ participantCount: 0 }));
  assert.equal(tracked.size, 0);
  // A new call in the same conversation is a new entry — the announcement stream is the only
  // source this device has, and it names the call it names.
  tracked = inProgressArrived(tracked, joined({ callId: 'call_next' as Id, participantCount: 1 }));
  assert.deepEqual(tracked.get(CONVERSATION), { callId: 'call_next' as Id, participantCount: 1 });
});

test('a call in progress turns the join button into joining the running call, by its id', () => {
  const joins: Array<{ conversationId: Id; callId: Id | undefined }> = [];
  // The button is a plain function of its props (no hooks), so the test can invoke it directly
  // and press what it rendered — the rig's static markup cannot carry a click.
  const button: ReactNode = GroupCallButton({
    conversationId: CONVERSATION,
    inProgress: { callId: CALL, participantCount: 2 },
    onJoin: (conversationId, callId) => {
      joins.push({ conversationId, callId });
      return Promise.resolve();
    },
  });
  const markup = renderToStaticMarkup(button);
  assert.ok(markup.includes('aria-label="Join group call in progress (2)"'));
  assert.ok(isValidElement(button));
  (button.props as { onClick: () => void }).onClick();
  // The join must carry the running call's id: a fresh one would mint a second call the
  // conversation did not ask for, and the id is the protocol's idempotency key.
  assert.deepEqual(joins, [{ conversationId: CONVERSATION, callId: CALL }]);
});

test('with no call in progress the join button passes no id, and the manager mints one', () => {
  const joins: Array<{ conversationId: Id; callId: Id | undefined }> = [];
  const button: ReactNode = GroupCallButton({
    conversationId: CONVERSATION,
    inProgress: null,
    onJoin: (conversationId, callId) => {
      joins.push({ conversationId, callId });
      return Promise.resolve();
    },
  });
  assert.ok(renderToStaticMarkup(button).includes('aria-label="Join group call"'));
  assert.ok(isValidElement(button));
  (button.props as { onClick: () => void }).onClick();
  assert.deepEqual(joins, [{ conversationId: CONVERSATION, callId: undefined }]);
});

test('the manager exposes the in-progress read and its actions bound', () => {
  function Probe(): ReactNode {
    const manager = useGroupCall();
    return (
      <div
        data-inprogress={manager.groupCallInProgress(CONVERSATION) === null ? 'none' : 'call'}
        data-bound={
          typeof manager.joinGroupCall === 'function' &&
          typeof manager.leaveGroupCall === 'function' &&
          typeof manager.dismissGroupCall === 'function'
            ? 'bound'
            : 'missing'
        }
      />
    );
  }

  const markup = renderToStaticMarkup(
    <MigoContext.Provider
      value={{
        status: 'ready',
        connectionState: 'ready',
        accountId: ME,
        deviceId: null,
        error: null,
        resetNonce: 0,
        persistKeyStore: () => {},
        client: null,
        register: () => Promise.resolve(),
        loginWithFile: () => Promise.resolve(),
        logout: () => Promise.resolve(),
      }}
    >
      <GroupCallManagerProvider>
        <Probe />
      </GroupCallManagerProvider>
    </MigoContext.Provider>,
  );
  // No announcements have arrived (there is no client), so nothing is in progress — the read
  // must be honest about that, not throw or invent.
  assert.ok(markup.includes('data-inprogress="none"'));
  assert.ok(markup.includes('data-bound="bound"'), 'every action the UI calls must be exposed');
});
