/**
 * What the call surface is allowed to say, and when it is allowed to exist.
 *
 * The tests pin three layers, each against the rule that would silently regress under an
 * innocent-looking refactor:
 *
 *   1. **The signal helpers.** The placeholder seal is framing, not encryption — the test pins the
 *      envelope's exact shape (version byte, 32 zero key bytes, 12 zero nonce bytes, then the
 *      payload) so the day real key material lands, the swap is a deliberate change to this test
 *      and not a quiet drift. A malformed envelope must throw rather than hand WebRTC nonsense.
 *   2. **The call screen, state by state.** Section 180 requires every state to name itself and
 *      the ended reasons to be told apart — "Declined" and "Connection lost" are different facts a
 *      user needs before calling back, and a screen that renders them all as "Call ended" throws
 *      the distinction away. The duration is `M:SS` with a zero floor (never `-4:51`, never `NaN`).
 *   3. **The header buttons' gate.** Call buttons exist only where the wire's 1:1 invite can name
 *      a callee — a direct conversation with a second member — and nowhere else, so a group thread
 *      never grows a button whose call no signaling could complete.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';
import type { ReactNode } from 'react';

import {
  ConversationKind,
  EncryptionMode,
  CallEndReason,
  CallMediaKind,
  CallState,
} from '@migo/sdk';
import type { ActiveCall, CallInviteEvent, ConversationSummary, Id } from '@migo/sdk';

import { CallErrorCard, CallScreen } from '../src/components/call-overlay.js';
import type { CallScreenProps } from '../src/components/call-overlay.js';
import { CallButtons } from '../src/components/call-buttons.js';
import { callPeerFor } from '../src/components/chat-window.js';
import { CallManagerProvider, useCall } from '../src/lib/migo/call-manager.js';
import {
  CallSignalFormatError,
  decodeIceBatch,
  decodeSdpDescription,
  displayStateOf,
  encodeIceBatch,
  encodeSdpDescription,
  endReasonLabel,
  formatCallDuration,
  mediaKindLabel,
  openCallSignal,
  sealCallSignal,
} from '../src/lib/migo/call-signal.js';
import { MigoContext } from '../src/lib/migo/provider.js';

const ME = 'me' as Id;
const ADA = 'ada' as Id;
const CALL = 'call_1' as Id;
const CONVERSATION = 'conv_1' as Id;
const NOW = Date.parse('2026-08-30T12:00:00Z');

// --- fixtures ---

function activeCall(overrides: Partial<ActiveCall> = {}): ActiveCall {
  return {
    callId: CALL,
    conversationId: CONVERSATION,
    callerId: ME,
    calleeId: ADA,
    mediaKind: CallMediaKind.Audio,
    state: CallState.Ringing,
    isCaller: true,
    ...overrides,
  };
}

function incomingCall(mediaKind = 0): CallInviteEvent {
  return {
    callId: CALL,
    conversationId: CONVERSATION,
    callerId: ADA,
    callerDevice: 'ada_device' as Id,
    mediaKind,
    expiresAt: NOW + 45_000,
    sealedOffer: new Uint8Array([1, 2, 3]),
  };
}

function screen(overrides: Partial<CallScreenProps> = {}): string {
  const props: CallScreenProps = {
    call: null,
    incoming: null,
    peerName: 'Ada Lovelace',
    peerId: 'ada',
    muted: false,
    degraded: false,
    nowMs: NOW,
    endedAt: null,
    localStream: null,
    remoteStream: null,
    onAccept: () => {},
    onDecline: () => {},
    onCancel: () => {},
    onEnd: () => {},
    onToggleMute: () => {},
    onDismiss: () => {},
    ...overrides,
  };
  return renderToStaticMarkup(<CallScreen {...props} />);
}

// --- the placeholder seal ---

test('the placeholder seal frames a payload with version, zero key, zero nonce', () => {
  const payload = new TextEncoder().encode('v=0\r\no=-...');
  const sealed = sealCallSignal(payload);

  assert.equal(sealed.length, 1 + 32 + 12 + payload.length);
  assert.equal(sealed[0], 1, 'the envelope version byte');
  assert.deepEqual(
    sealed.slice(1, 33),
    new Uint8Array(32),
    'the key slot is placeholder zeros, the length the real crypto will use',
  );
  assert.deepEqual(sealed.slice(33, 45), new Uint8Array(12), 'the nonce slot likewise');
  assert.deepEqual(openCallSignal(sealed), payload, 'the round trip is exact');
});

test('the seal refuses to open anything that is not its own envelope', () => {
  const tooShort = new Uint8Array(10);
  assert.throws(() => openCallSignal(tooShort), CallSignalFormatError);
  const wrongVersion = new Uint8Array(1 + 32 + 12 + 4);
  wrongVersion[0] = 99;
  assert.throws(() => openCallSignal(wrongVersion), CallSignalFormatError);
  // A future version must be refused, not best-effort parsed.
  const future = sealCallSignal(new Uint8Array([9]));
  future[0] = 2;
  assert.throws(() => openCallSignal(future), CallSignalFormatError);
});

test('SDP descriptions and ICE batches round-trip through their codecs and refuse impostors', () => {
  const description = { type: 'offer' as const, sdp: 'v=0\r\no=- 1 1 IN IP4 127.0.0.1' };
  assert.deepEqual(decodeSdpDescription(encodeSdpDescription(description)), description);

  const candidates: RTCIceCandidateInit[] = [
    {
      candidate: 'candidate:1 1 udp 2130706431 192.168.1.4 54321 typ host',
      sdpMid: '0',
      sdpMLineIndex: 0,
    },
    {
      candidate: 'candidate:2 1 udp 1686052607 10.0.0.4 54322 typ srflx',
      sdpMid: '0',
      sdpMLineIndex: 0,
    },
  ];
  assert.deepEqual(decodeIceBatch(encodeIceBatch(candidates)), candidates);

  assert.throws(
    () => decodeSdpDescription(new TextEncoder().encode('{"sdp":"no type"}')),
    CallSignalFormatError,
  );
  assert.throws(
    () => decodeIceBatch(new TextEncoder().encode('{"not":"a batch"}')),
    CallSignalFormatError,
  );
});

// --- the words and numbers ---

test('a duration renders as M:SS with a zero floor', () => {
  assert.equal(formatCallDuration(0), '0:00');
  assert.equal(formatCallDuration(83_000), '1:23');
  assert.equal(formatCallDuration(65_000), '1:05');
  assert.equal(formatCallDuration(3_661_000), '61:01', 'minutes are unbounded, still one glance');
  assert.equal(
    formatCallDuration(-5_000),
    '0:00',
    'a negative elapsed time must not render a sign',
  );
});

test('the display state maps the wire\u2019s five states plus the client-side degraded judgement', () => {
  assert.equal(displayStateOf(activeCall({ state: CallState.Ringing }), false), 'ringing');
  assert.equal(displayStateOf(activeCall({ state: CallState.Connecting }), false), 'connecting');
  assert.equal(displayStateOf(activeCall({ state: CallState.Connected }), false), 'connected');
  assert.equal(
    displayStateOf(activeCall({ state: CallState.Reconnecting }), false),
    'reconnecting',
  );
  assert.equal(displayStateOf(activeCall({ state: CallState.Ended }), false), 'ended');
  // Degraded is connected-but-worse, never a state of its own on the wire.
  assert.equal(displayStateOf(activeCall({ state: CallState.Connected }), true), 'degraded');
  assert.equal(displayStateOf(activeCall({ state: CallState.Ringing }), true), 'ringing');
});

test('ended reasons stay distinct, as section 180 requires', () => {
  assert.equal(endReasonLabel(CallEndReason.ByCaller), 'Call ended');
  assert.equal(endReasonLabel(CallEndReason.ByCallee), 'Call ended');
  assert.equal(endReasonLabel(CallEndReason.Declined), 'Declined');
  assert.equal(endReasonLabel(CallEndReason.NoAnswer), 'No answer');
  assert.equal(endReasonLabel(CallEndReason.Failed), 'Failed to connect');
  assert.equal(endReasonLabel(CallEndReason.Network), 'Connection lost');
  assert.equal(endReasonLabel(undefined), 'Call ended');
  assert.equal(mediaKindLabel(CallMediaKind.Audio), 'voice call');
  assert.equal(mediaKindLabel(CallMediaKind.Video), 'video call');
});

// --- the call screen, state by state ---

test('an incoming call names the caller, the kind, and offers exactly accept and decline', () => {
  const markup = screen({ incoming: incomingCall(0) });
  assert.ok(markup.includes('Ada Lovelace'), 'the caller\u2019s name is missing');
  assert.ok(markup.includes('Incoming voice call'), 'the call kind is missing');
  assert.ok(
    markup.includes('aria-label="Accept voice call"'),
    'accept must be labelled for screen readers',
  );
  assert.ok(markup.includes('aria-label="Decline call"'));
  // The accept glyph follows the kind: a video call offers the camera, not the handset.
  const video = screen({ incoming: incomingCall(1) });
  assert.ok(video.includes('Incoming video call'));
  assert.ok(video.includes('aria-label="Accept video call"'));
});

test('a call we placed reads Calling… while it rings, with a cancel control', () => {
  const markup = screen({ call: activeCall({ state: CallState.Ringing, isCaller: true }) });
  assert.ok(markup.includes('Calling…'));
  assert.ok(markup.includes('aria-label="Cancel call"'));
  assert.ok(
    !markup.includes('aria-label="Accept'),
    'a ringing call we placed has nothing to accept',
  );
});

test('a connected audio call shows the running duration, mute, and end', () => {
  const markup = screen({
    call: activeCall({ state: CallState.Connected, startedAt: NOW - 83_000 }),
  });
  assert.ok(markup.includes('Connected'));
  assert.ok(markup.includes('1:23'), 'the duration timer is missing');
  assert.ok(markup.includes('aria-label="Mute microphone"'));
  assert.ok(markup.includes('aria-label="End call"'));
  assert.ok(!markup.includes('<video'), 'an audio call must not render video elements');
});

test('a connected video call renders the remote view and a muted self-view', () => {
  const markup = screen({
    call: activeCall({
      state: CallState.Connected,
      mediaKind: CallMediaKind.Video,
      startedAt: NOW - 5_000,
    }),
  });
  const videos = markup.match(/<video/g) ?? [];
  assert.equal(videos.length, 2, 'a video call renders exactly the remote and local views');
  assert.ok(markup.includes('muted'), 'the self-view must be muted or it echoes');
  assert.ok(markup.includes('0:05'));
  // No video elements before media exists to show: ringing shows the avatar instead.
  const ringing = screen({
    call: activeCall({ state: CallState.Ringing, mediaKind: CallMediaKind.Video }),
  });
  assert.ok(!(ringing.match(/<video/g) ?? []).length, 'a ringing video call has no views yet');
});

test('a reconnecting call says so instead of going silent, and can still be ended', () => {
  const markup = screen({
    call: activeCall({ state: CallState.Reconnecting, startedAt: NOW - 30_000 }),
  });
  assert.ok(markup.includes('Reconnecting…'), 'the state must name itself (section 180)');
  assert.ok(markup.includes('aria-label="End call"'));
});

test('a degraded call states that video paused while the call continues', () => {
  const markup = screen({
    call: activeCall({ state: CallState.Connected, startedAt: NOW - 30_000 }),
    degraded: true,
  });
  assert.ok(markup.includes('Poor connection'), 'the degraded line is missing');
  assert.ok(markup.includes('0:30'), 'a degraded call is still connected, still counting');
  assert.ok(markup.includes('aria-label="End call"'));
});

test('an ended call states its reason, its duration if it had one, and offers a way back', () => {
  const declined = screen({
    call: activeCall({ state: CallState.Ended, endReason: CallEndReason.Declined }),
  });
  assert.ok(
    declined.includes('Declined'),
    'the reason must be named, not folded into "Call ended"',
  );
  assert.ok(
    !declined.includes('call-timer'),
    'a call that never connected has no duration to show',
  );
  assert.ok(declined.includes('Back to chats'), 'the dismiss control is missing');

  const dropped = screen({
    call: activeCall({
      state: CallState.Ended,
      endReason: CallEndReason.Network,
      startedAt: NOW - 65_000,
      isCaller: false,
    }),
    endedAt: NOW,
  });
  assert.ok(dropped.includes('Connection lost'));
  assert.ok(dropped.includes('1:05'), 'the total duration is missing from the ended screen');
});

test('with no call and no invite, the screen renders nothing at all', () => {
  assert.equal(screen(), '');
});

test('a placement failure states the fact and offers a close, never a payload', () => {
  const markup = renderToStaticMarkup(
    <CallErrorCard
      message="Microphone or camera unavailable. Check permissions and try again."
      onDismiss={() => {}}
    />,
  );
  assert.ok(markup.includes('Microphone or camera unavailable.'));
  assert.ok(markup.includes('Close'));
  assert.ok(!markup.includes('[object'), 'an error must never stringify a cause');
});

// --- the header buttons and their gate ---

test('the call buttons appear with both kinds labelled, and dial the peer of the thread', () => {
  const markup = renderToStaticMarkup(
    <CallButtons
      conversationId={CONVERSATION}
      peerId={ADA}
      onStartCall={() => Promise.resolve()}
    />,
  );
  assert.ok(markup.includes('aria-label="Voice call"'));
  assert.ok(markup.includes('aria-label="Video call"'));
  // Without a peer there is nothing to dial: the component is the gate, not the stylesheet.
  assert.equal(
    renderToStaticMarkup(
      <CallButtons
        conversationId={CONVERSATION}
        peerId={null}
        onStartCall={() => Promise.resolve()}
      />,
    ),
    '',
    'call buttons rendered for a conversation with no callable peer',
  );
});

function summary(kind: ConversationKind, members?: Id[]): ConversationSummary {
  return {
    conversationId: CONVERSATION,
    kind,
    encryption: EncryptionMode.EndToEnd,
    lastSeq: 1,
    readSeq: 1,
    ...(members !== undefined ? { members } : {}),
  };
}

test('only a direct conversation with a second member has a callable peer', () => {
  assert.equal(callPeerFor(summary(ConversationKind.Direct, [ME, ADA]), ME), ADA);
  // A group or room has an audience, not a callee — that call is the SFU flow, not this build's.
  assert.equal(callPeerFor(summary(ConversationKind.Group, [ME, ADA]), ME), null);
  assert.equal(callPeerFor(summary(ConversationKind.Room, [ME, ADA]), ME), null);
  // A note-to-self direct thread has nobody to dial; so does a summary that named nobody.
  assert.equal(callPeerFor(summary(ConversationKind.Direct, [ME]), ME), null);
  assert.equal(callPeerFor(summary(ConversationKind.Direct), ME), null);
  assert.equal(callPeerFor(undefined, ME), null);
});

// --- the manager's contract ---

test('the call manager context starts with no call, no invite, and its actions bound', () => {
  function Probe(): ReactNode {
    const call = useCall();
    return (
      <div
        data-active={call.activeCall === null ? 'none' : 'call'}
        data-incoming={call.incomingCall === null ? 'none' : 'ringing'}
        data-muted={String(call.muted)}
        data-ended={String(call.endedAt === null)}
      >
        {typeof call.startCall === 'function' &&
        typeof call.acceptCall === 'function' &&
        typeof call.answerCall === 'function' &&
        typeof call.declineCall === 'function' &&
        typeof call.cancelCall === 'function' &&
        typeof call.endCall === 'function' &&
        typeof call.toggleMute === 'function' &&
        typeof call.dismissCall === 'function'
          ? 'bound'
          : 'missing'}
      </div>
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
        client: null,
        register: () => Promise.resolve(),
        login: () => Promise.resolve(),
        logout: () => Promise.resolve(),
      }}
    >
      <CallManagerProvider>
        <Probe />
      </CallManagerProvider>
    </MigoContext.Provider>,
  );
  assert.ok(markup.includes('data-active="none"'));
  assert.ok(markup.includes('data-incoming="none"'));
  assert.ok(markup.includes('data-muted="false"'));
  assert.ok(markup.includes('data-ended="true"'));
  assert.ok(markup.includes('bound'), 'the manager must expose every action the UI calls');
});
