/**
 * What the call surface is allowed to say, and when it is allowed to exist.
 *
 * The tests pin three layers, each against the rule that would silently regress under an
 * innocent-looking refactor:
 *
 *   1. **The signal helpers.** The seal is real per-call encryption under the house AEAD, and the
 *      key's channel is pinned with it: a call-key control event must round-trip the call id and
 *      the key exactly, and refuse anything of the wrong width, because a malformed event on the
 *      E2EE message layer is dropped like any other noise. A malformed envelope must throw rather
 *      than hand WebRTC nonsense, and the legacy version-1 envelope — a pre-encryption build's
 *      framing — must still open, so a peer that has not upgraded is served honestly.
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
import {
  CallManagerProvider,
  answerMediaWithFallback,
  iceServersForCall,
  useCall,
} from '../src/lib/migo/call-manager.js';
import type { TurnClient } from '../src/lib/migo/call-manager.js';
import {
  CALL_KEY_EVENT,
  CallSignalFormatError,
  INVITE_BLOCKED,
  INVITE_BUSY,
  INVITE_DECLINED,
  INVITE_EXPIRED,
  INVITE_RINGING,
  answersRingingCall,
  decodeCallKeyEvent,
  decodeIceBatch,
  decodeSdpDescription,
  displayStateOf,
  encodeCallKeyEvent,
  encodeIceBatch,
  encodeSdpDescription,
  endedReasonLine,
  endsRingingCall,
  endReasonLabel,
  formatCallDuration,
  generateCallKey,
  incomingInviteDisposition,
  inviteEndReason,
  mediaKindLabel,
  openCallSignal,
  ringTimeoutMs,
  sealCallSignal,
  sdpDisposition,
  SdpDisposition,
} from '../src/lib/migo/call-signal.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import type { TurnServer } from '@migo/sdk';

const ME = 'me' as Id;
const ADA = 'ada' as Id;
const CALL = 'call_1' as Id;
/**
 * A well-formed id for the key-event codec, which round-trips the call id through its 16 wire
 * bytes — the fixture ids above are fine for the seal (its domain is the id as text) but not for
 * a codec that must parse one.
 */
const CALL_ID = '0123456789ABCDEFGHJKMNPQRS' as Id;
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
    cameraOn: false,
    degraded: false,
    quality: null,
    sharingScreen: false,
    screenStream: null,
    outputs: [],
    cameras: [],
    outputId: null,
    nowMs: NOW,
    endedAt: null,
    localStream: null,
    remoteStream: null,
    onAccept: () => {},
    onDecline: () => {},
    onCancel: () => {},
    onEnd: () => {},
    onToggleMute: () => {},
    onToggleCamera: () => {},
    onSwitchCamera: () => {},
    onToggleScreenShare: () => {},
    onSelectOutput: () => {},
    onDismiss: () => {},
    ...overrides,
  };
  return renderToStaticMarkup(<CallScreen {...props} />);
}

// --- the seal, its key, and the key's channel ---

test('the seal is real per-call encryption: version 2, round-trip, fresh key every call', () => {
  const payload = new TextEncoder().encode('v=0\r\no=-...');
  const key = generateCallKey();
  const sealed = sealCallSignal(payload, key, CALL);

  assert.equal(sealed[0], 2, 'the envelope version byte');
  // The house AEAD's own output behind the version byte: 24-byte nonce, ciphertext, 16-byte tag.
  assert.equal(sealed.length, 1 + 24 + payload.length + 16);
  assert.deepEqual(openCallSignal(sealed, key, CALL), payload, 'the round trip is exact');

  const secondKey = generateCallKey();
  assert.equal(secondKey.length, 32, 'the key is the AEAD-width 32 bytes');
  assert.notDeepEqual(key, secondKey, 'every call mints its own key');
});

test('the seal refuses wrong keys, wrong calls, and anything that is not its own envelope', () => {
  const payload = new TextEncoder().encode('candidate:1 1 udp 2130706431 192.168.1.4 54321');
  const key = generateCallKey();
  const sealed = sealCallSignal(payload, key, CALL);

  assert.throws(() => openCallSignal(sealed, generateCallKey(), CALL), CallSignalFormatError);
  // The call id is associated data: a blob sealed for one call must not open as another's.
  assert.throws(
    () => openCallSignal(sealed, key, 'call_other' as Id),
    CallSignalFormatError,
    'the envelope is bound to its call',
  );
  const edited = sealed.slice();
  const last = edited.length - 1;
  edited[last] = (edited[last] ?? 0) ^ 0x01;
  assert.throws(() => openCallSignal(edited, key, CALL), CallSignalFormatError);

  assert.throws(() => openCallSignal(new Uint8Array(10), key, CALL), CallSignalFormatError);
  assert.throws(() => openCallSignal(new Uint8Array(0), key, CALL), CallSignalFormatError);
  const wrongVersion = new Uint8Array(1 + 24 + 4 + 16);
  wrongVersion[0] = 99;
  assert.throws(() => openCallSignal(wrongVersion, key, CALL), CallSignalFormatError);
  // A future version must be refused, not best-effort parsed.
  const future = sealCallSignal(new Uint8Array([9]), key, CALL);
  future[0] = 3;
  assert.throws(() => openCallSignal(future, key, CALL), CallSignalFormatError);
});

test('the legacy version-1 envelope still opens, and is never written', () => {
  // An older build framed payloads with a version byte, zero key and nonce slots, and clear bytes
  // behind them. A call from a peer still on that build must be readable — the payload was never
  // encrypted, so any key opens it — while this build's own seal (the version byte above) never
  // produces the shape again.
  const payload = new TextEncoder().encode('v=0\r\no=- legacy');
  const legacy = new Uint8Array(1 + 32 + 12 + payload.length);
  legacy[0] = 1;
  legacy.set(payload, 1 + 32 + 12);

  assert.deepEqual(
    openCallSignal(legacy, generateCallKey(), CALL),
    payload,
    'the legacy envelope carries no encryption to check',
  );
  assert.equal(sealCallSignal(payload, generateCallKey(), CALL)[0], 2);
});

test('a call key rides a control event as call id then key, or is dropped as noise', () => {
  const key = generateCallKey();
  const event = encodeCallKeyEvent(CALL_ID, key);
  assert.equal(event.length, 16 + 32, 'the wire bytes of the id, then the key');

  const decoded = decodeCallKeyEvent(event);
  assert.ok(decoded !== null, 'a well-formed event decodes');
  assert.equal(decoded?.callId, CALL_ID, 'the call id round-trips through its wire bytes');
  assert.deepEqual(decoded?.key, key, 'the key arrives verbatim');

  // The event name itself is part of the contract: the manager listens for exactly this string,
  // the way the SDK's sender-key handling does.
  assert.equal(CALL_KEY_EVENT, 'call-key');

  assert.equal(
    decodeCallKeyEvent(new Uint8Array(16)),
    null,
    'a too-short event is dropped, not thrown over',
  );
  assert.equal(
    decodeCallKeyEvent(new Uint8Array(16 + 32 + 1)),
    null,
    'a too-long event is dropped likewise',
  );
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

test('an arriving SDP is read as an answer or a renegotiation, never as both', () => {
  const offer = { type: 'offer' as const, sdp: 'v=0' };
  const answer = { type: 'answer' as const, sdp: 'v=0' };

  // The two cases the call actually runs on: the side that offered is answered, and the side that
  // did not is offered a restart to answer.
  assert.equal(sdpDisposition(answer, true), SdpDisposition.Answer);
  assert.equal(sdpDisposition(offer, false), SdpDisposition.Renegotiation);

  // CALL_SDP is Critical, so a redelivery is a frame the transport is entitled to hand over twice.
  // A second answer applied to a connection that is no longer waiting for one is a state error, and
  // a second offer answered twice is a renegotiation nobody asked for — both are ignored.
  assert.equal(sdpDisposition(answer, false), SdpDisposition.Ignore);
  assert.equal(sdpDisposition(offer, true), SdpDisposition.Ignore);

  // pranswer and rollback are states this build never puts on the wire; a relay carrying one is
  // not part of a call it is running.
  assert.equal(sdpDisposition({ type: 'pranswer', sdp: 'v=0' }, false), SdpDisposition.Ignore);
  assert.equal(sdpDisposition({ type: 'rollback', sdp: '' }, true), SdpDisposition.Ignore);
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
  assert.equal(endReasonLabel(CallEndReason.Busy), 'Busy');
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

test('the quality indicator shows the measured tier, and nothing before there is one', () => {
  const connected = activeCall({ state: CallState.Connected, startedAt: NOW - 30_000 });
  // Null is not a good rung: a screen that opens on "Excellent" has reported something it never
  // checked, which is the one thing an indicator exists not to do.
  const unmeasured = screen({ call: connected });
  assert.ok(!unmeasured.includes('call-quality'), 'no measurement, no indicator');
  assert.ok(!unmeasured.includes('Excellent'), 'an unmeasured call must not claim the best tier');
  for (const [quality, word] of [
    ['full', 'Excellent'],
    ['bitrate-capped', 'Good'],
    ['resolution-lowered', 'Average'],
    ['frame-rate-lowered', 'Poor'],
    ['video-off', 'Very poor'],
  ] as const) {
    const markup = screen({ call: connected, quality });
    assert.ok(markup.includes(`call-quality ${quality}`), `the ${quality} rung is not marked`);
    assert.ok(markup.includes(word), `the ${quality} rung must read as ${word}`);
  }
  // A voice call is measured too: a lossy line is a fact about the call with no camera in it.
  const voice = screen({
    call: activeCall({ state: CallState.Connected, mediaKind: CallMediaKind.Audio }),
    quality: 'frame-rate-lowered',
  });
  assert.ok(voice.includes('Poor'), 'a voice call reports its link the same way');
});

test('a connected video call offers screen sharing, and a voice call never does', () => {
  const video = screen({
    call: activeCall({
      state: CallState.Connected,
      mediaKind: CallMediaKind.Video,
      startedAt: NOW - 5_000,
    }),
  });
  assert.ok(video.includes('aria-label="Share your screen"'));

  // Section 180 makes screen sharing a video-call capability: a voice call has no video m-line for
  // a share to ride, so the control that could not do anything is not drawn at all.
  const voice = screen({
    call: activeCall({ state: CallState.Connected, mediaKind: CallMediaKind.Audio }),
  });
  assert.ok(!voice.includes('Share your screen'), 'a voice call cannot share a screen');

  // And it belongs to the states a live link exists in, like the other connected controls.
  const ringing = screen({
    call: activeCall({ state: CallState.Ringing, mediaKind: CallMediaKind.Video }),
  });
  assert.ok(!ringing.includes('Share your screen'), 'nothing is shared before a call connects');
});

test('the camera control belongs to a video call, and says which way it will go', () => {
  const video = activeCall({
    state: CallState.Connected,
    mediaKind: CallMediaKind.Video,
    startedAt: NOW - 5_000,
  });
  // Section 180 lists camera on and off among the controls a call screen carries, and the label is
  // the state rather than the action: a button that always read "Camera" would leave the user
  // guessing which way the press goes.
  assert.ok(screen({ call: video, cameraOn: true }).includes('aria-label="Turn camera off"'));
  assert.ok(screen({ call: video, cameraOn: false }).includes('aria-label="Turn camera on"'));

  // A voice call publishes no camera, so there is no control to draw — and the same rule the share
  // button follows keeps it off the states either side of a live call.
  const voice = screen({
    call: activeCall({ state: CallState.Connected, mediaKind: CallMediaKind.Audio }),
  });
  assert.ok(!voice.includes('Turn camera'), 'a voice call has no camera to turn');
  const ringing = screen({
    call: activeCall({ state: CallState.Ringing, mediaKind: CallMediaKind.Video }),
    cameraOn: true,
  });
  assert.ok(!ringing.includes('Turn camera'), 'the controls belong to a connected call');
});

test('the camera switch appears only where the platform reported a second camera', () => {
  const call = activeCall({
    state: CallState.Connected,
    mediaKind: CallMediaKind.Video,
    startedAt: NOW - 5_000,
  });
  const one = [{ id: 'cam-1', label: 'Front camera' }];
  const two = [...one, { id: 'cam-2', label: 'Back camera' }];

  // Section 180 asks for a front/back switch; a device with one camera has nothing to switch to,
  // and a button that did nothing would be a control that lies about what it did.
  assert.ok(!screen({ call, cameras: one }).includes('Switch camera'));
  assert.ok(screen({ call, cameras: two }).includes('aria-label="Switch camera"'));
  // A voice call has no camera to move between, however many the device has.
  const voice = activeCall({ state: CallState.Connected, mediaKind: CallMediaKind.Audio });
  assert.ok(!screen({ call: voice, cameras: two }).includes('Switch camera'));
});

test('the audio output list is the platform’s, drawn only when there is one to offer', () => {
  const voice = activeCall({ state: CallState.Connected, mediaKind: CallMediaKind.Audio });
  // A browser with no output selection reports nothing, and the control is drawn from that empty
  // list rather than from this client's idea of what a phone has.
  assert.ok(!screen({ call: voice }).includes('Audio output'));

  const outputs = [
    { id: 'speaker', label: 'Speaker' },
    { id: 'headset', label: 'Wired headset' },
  ];
  const withOutputs = screen({ call: voice, outputs, outputId: 'headset' });
  // The names are the platform's own — section 180's speaker, earpiece, Bluetooth, and wired
  // headset are whatever `enumerateDevices` reported, never a set this client invented.
  assert.ok(withOutputs.includes('Audio output'));
  assert.ok(withOutputs.includes('Speaker'));
  assert.ok(withOutputs.includes('Wired headset'));
  assert.ok(withOutputs.includes('System default'), 'the default is a choice, not an absence');
});

test('the self-view says a camera that is off is off, rather than showing a black frame', () => {
  const call = activeCall({
    state: CallState.Connected,
    mediaKind: CallMediaKind.Video,
    startedAt: NOW - 5_000,
  });
  const camera = { id: 'cam' } as unknown as MediaStream;
  assert.ok(
    screen({ call, cameraOn: true, localStream: camera }).includes('aria-label="Your video"'),
  );
  // A user who pressed "camera off" and still sees a video element labelled as their video is
  // looking at the one thing that would make them press it again.
  assert.ok(
    screen({ call, cameraOn: false, localStream: camera }).includes('aria-label="Camera off"'),
  );
  // Except while sharing: the screen is what is being sent, and it wins over a camera that is off.
  const desktop = { id: 'screen' } as unknown as MediaStream;
  assert.ok(
    screen({
      call,
      cameraOn: false,
      localStream: camera,
      screenStream: desktop,
      sharingScreen: true,
    }).includes('aria-label="The screen you are sharing"'),
  );
});

test('the sharer keeps reading that the screen is being shared, for as long as it is', () => {
  const call = activeCall({
    state: CallState.Connected,
    mediaKind: CallMediaKind.Video,
    startedAt: NOW - 5_000,
  });
  const idle = screen({ call });
  assert.ok(!idle.includes('call-sharing'), 'no share, no indicator');

  const sharing = screen({ call, sharingScreen: true });
  // The requirement is not "a notice appears" but "it is still there while the share runs": a
  // forgotten share is the leak section 180 names, so the indicator is not a toast and does not
  // depend on any other state to survive.
  assert.ok(sharing.includes('call-sharing'), 'the sharer must see that it is sharing');
  assert.ok(sharing.includes('You are sharing your screen'), 'and it must say so in words');
  assert.ok(sharing.includes('aria-label="Stop sharing your screen"'), 'and offer the way out');

  // It survives the degraded rung, which is the state a share most plausibly outlives the user's
  // attention in: video is paused for the peer, and the capture is still running.
  const degraded = screen({ call, sharingScreen: true, degraded: true });
  assert.ok(degraded.includes('You are sharing your screen'));
});

test('the self-view shows what is being sent: the screen while sharing, the camera otherwise', () => {
  const call = activeCall({
    state: CallState.Connected,
    mediaKind: CallMediaKind.Video,
    startedAt: NOW - 5_000,
  });
  const camera = { id: 'cam' } as unknown as MediaStream;
  const desktop = { id: 'screen' } as unknown as MediaStream;

  // The label is the only part of this a static render can read, and it is the part that matters:
  // a self-view labelled "Your video" over a shared desktop tells the user the wrong thing about
  // what the peer is receiving.
  assert.ok(
    screen({ call, cameraOn: true, localStream: camera }).includes('aria-label="Your video"'),
  );
  const sharing = screen({ call, localStream: camera, screenStream: desktop, sharingScreen: true });
  assert.ok(sharing.includes('aria-label="The screen you are sharing"'));
});

test('a measured rung is not shown while the call is still ringing or already ended', () => {
  // The tier describes a live link, so it belongs to the states a link exists in and not to the
  // ones either side of them.
  assert.ok(
    !screen({ call: activeCall({ state: CallState.Ringing }), quality: 'full' }).includes(
      'call-quality',
    ),
    'a ringing call has no link to report on',
  );
  assert.ok(
    !screen({
      call: activeCall({ state: CallState.Ended, endReason: CallEndReason.ByCaller }),
      quality: 'full',
    }).includes('call-quality'),
    'an ended call has no link to report on',
  );
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
        typeof call.toggleCamera === 'function' &&
        typeof call.switchCamera === 'function' &&
        typeof call.setOutputDevice === 'function' &&
        typeof call.toggleScreenShare === 'function' &&
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
        persistKeyStore: () => {},
        client: null,
        register: () => Promise.resolve(),
        loginWithFile: () => Promise.resolve(),
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

// --- the ring's lifecycle: redelivery, expiry, and the phantom ring ---

test('the invite status vocabulary is the wire\u2019s: 0 ringing, 1 declined, 2 expired, 3 blocked', () => {
  assert.equal(INVITE_RINGING, 0);
  assert.equal(INVITE_DECLINED, 1);
  assert.equal(INVITE_EXPIRED, 2);
  assert.equal(INVITE_BLOCKED, 3);
});

/** The occupancy facts the manager reads when an invite lands, for the disposition tests below. */
function occupancy(
  ringingCallId: Id | null,
  activeCallId: Id | null = null,
  busy = activeCallId !== null,
): { ringingCallId: Id | null; activeCallId: Id | null; busy: boolean } {
  return { ringingCallId, activeCallId, busy };
}

test('a redelivered invite for the ring already showing is ignored, never declined', () => {
  // Critical frames are delivered at least once: the second copy of the invite that is ringing
  // right now is the expected one, and declining it would hang up the call the user is being
  // rung for.
  assert.equal(incomingInviteDisposition(incomingCall(), occupancy(CALL), NOW), 'ignore');
  // A genuinely different call while a ring shows is the busy case, and it must be declined.
  assert.equal(
    incomingInviteDisposition(
      { ...incomingCall(), callId: 'call_busy' as Id },
      occupancy(CALL),
      NOW,
    ),
    'decline-busy',
  );
});

test('a redelivered invite for the call already answered is ignored, and an ended call blocks nothing', () => {
  assert.equal(
    incomingInviteDisposition(incomingCall(), occupancy(null, CALL, true), NOW),
    'ignore',
  );
  // A call that just ended and is still on screen does not occupy the device: a new call rings.
  assert.equal(
    incomingInviteDisposition(
      { ...incomingCall(), callId: 'call_next' as Id },
      occupancy(null, CALL, false),
      NOW,
    ),
    'ring',
  );
  // But a different call while a live one runs is busy, as ever.
  assert.equal(
    incomingInviteDisposition(
      { ...incomingCall(), callId: 'call_other' as Id },
      occupancy(null, CALL, true),
      NOW,
    ),
    'decline-busy',
  );
});

test('an invite that expired in flight rings nobody; a fresh one with the device free rings', () => {
  const stale = { ...incomingCall(), expiresAt: NOW - 1 };
  assert.equal(incomingInviteDisposition(stale, occupancy(null), NOW), 'ignore');
  assert.equal(incomingInviteDisposition(incomingCall(), occupancy(null), NOW), 'ring');
});

test('an Ended for the ringing call retires the ring; any other state event does not', () => {
  const ended = { callId: CALL, state: 4, reason: 0 };
  assert.ok(endsRingingCall(ended, CALL), 'the caller canceling must stop the callee\u2019s ring');
  assert.ok(!endsRingingCall({ ...ended, callId: 'call_other' as Id }, CALL));
  assert.ok(!endsRingingCall(ended, null), 'with no ring showing there is nothing to retire');
  assert.ok(
    !endsRingingCall({ ...ended, state: 1 }, CALL),
    'a Connecting event for the ring is not an end',
  );
});

test('a Connecting or Connected for the ringing call was answered on a sibling device', () => {
  // The server rings every device on the account and publishes the answer to both parties, so
  // the device still ringing hears the call move on without it. That is a retirement of the
  // ring \u2014 a "answered elsewhere" note \u2014 never a missed call and never a decline.
  const connecting = { callId: CALL, state: 1 };
  assert.ok(
    answersRingingCall(connecting, CALL),
    'the sibling that answered must stop this device\u2019s ring',
  );
  assert.ok(answersRingingCall({ ...connecting, state: 2 }, CALL));
  assert.ok(!answersRingingCall({ ...connecting, callId: 'call_other' as Id }, CALL));
  assert.ok(
    !answersRingingCall(connecting, null),
    'with no ring showing there is nothing to retire',
  );
  assert.ok(
    !answersRingingCall({ ...connecting, state: 4 }, CALL),
    'an Ended is the missed-call path, not the answered-elsewhere one',
  );
  assert.ok(
    !answersRingingCall({ ...connecting, state: 0 }, CALL),
    'a Ringing state names no answer',
  );
});

test('the caller\u2019s local ring timeout mirrors the invite expiry, floored at zero', () => {
  assert.equal(ringTimeoutMs(NOW + 45_000, NOW), 45_000);
  assert.equal(
    ringTimeoutMs(NOW - 10_000, NOW),
    0,
    'a reply that arrived late must fire the mirror at once, never a negative delay',
  );
});

test('a blocked invite refusal is a distinct fact on the caller\u2019s ended screen', () => {
  // The reason enum has no Blocked member: every non-expired refusal is Declined on the wire
  // side, and the raw status rides along for the screen.
  assert.equal(inviteEndReason(INVITE_DECLINED), CallEndReason.Declined);
  assert.equal(inviteEndReason(INVITE_EXPIRED), CallEndReason.NoAnswer);
  assert.equal(inviteEndReason(INVITE_BLOCKED), CallEndReason.Declined);
  assert.equal(inviteEndReason(INVITE_BUSY), CallEndReason.Busy);

  const blocked = activeCall({
    state: CallState.Ended,
    endReason: CallEndReason.Declined,
    inviteStatus: INVITE_BLOCKED,
  });
  assert.equal(endedReasonLine(blocked), 'Unavailable');
  assert.equal(
    endedReasonLine(
      activeCall({
        state: CallState.Ended,
        endReason: CallEndReason.Declined,
        inviteStatus: INVITE_DECLINED,
      }),
    ),
    'Declined',
  );
  assert.equal(
    endedReasonLine(activeCall({ state: CallState.Ended, endReason: CallEndReason.Network })),
    'Connection lost',
  );

  // The screen itself: the caller who was blocked sees the distinct word, never "Declined".
  const markup = screen({ call: blocked });
  assert.ok(markup.includes('Unavailable'));
  assert.ok(!markup.includes('Declined'), 'a blocked refusal must not read as a human decline');
});

test('the missed-call note is a notice, labelled as one, not a placement failure', () => {
  const markup = renderToStaticMarkup(
    <CallErrorCard message="Missed call" onDismiss={() => {}} label="Missed call" />,
  );
  assert.ok(markup.includes('Missed call'));
  assert.ok(markup.includes('aria-label="Missed call"'));
});

// --- the peer connection's ICE servers ---

test('ICE servers map the TURN relays and always keep the public STUN fallback', async () => {
  const requested: Id[] = [];
  const client: TurnClient = {
    calls: {
      getTurnServers: (callId: Id) => {
        requested.push(callId);
        const servers: TurnServer[] = [
          {
            url: 'turn:relay.example.test:3478',
            username: 'caller',
            credential: 'secret',
            ttlSeconds: 300,
            region: 'eu',
          },
          {
            url: 'turn:anon.example.test:3478',
            username: '',
            credential: '',
            ttlSeconds: 300,
            region: 'us',
          },
        ];
        return Promise.resolve(servers);
      },
    },
  };
  assert.deepEqual(await iceServersForCall(client, CALL), [
    { urls: 'turn:relay.example.test:3478', username: 'caller', credential: 'secret' },
    { urls: 'turn:anon.example.test:3478' },
    { urls: 'stun:stun.l.google.com:19302' },
  ]);
  assert.deepEqual(requested, [CALL], 'the relays must be fetched for the call they serve');
});

test('a TURN list that fails or comes back empty still leaves the STUN fallback', async () => {
  const failing: TurnClient = {
    calls: { getTurnServers: () => Promise.reject(new Error('relay config unreachable')) },
  };
  assert.deepEqual(await iceServersForCall(failing, CALL), [
    { urls: 'stun:stun.l.google.com:19302' },
  ]);
  const empty: TurnClient = {
    calls: { getTurnServers: () => Promise.resolve([]) },
  };
  assert.deepEqual(await iceServersForCall(empty, CALL), [
    { urls: 'stun:stun.l.google.com:19302' },
  ]);
});

// --- answering without a camera ---

/** A stream stand-in: the manager only stores and hands it around, never inspects it. */
function fakeStream(label: string): MediaStream {
  return label as unknown as MediaStream;
}

test('a video answer without a camera falls back to audio instead of declining', async () => {
  const asked: CallMediaKind[] = [];
  const audioOnly = fakeStream('audio-only');
  const acquire = (kind: CallMediaKind): Promise<MediaStream> => {
    asked.push(kind);
    // A device with a microphone and no camera: the video ask is refused, the audio ask is not.
    if (kind === CallMediaKind.Video) {
      return Promise.reject(new DOMException('no camera', 'NotFoundError'));
    }
    return Promise.resolve(audioOnly);
  };

  const stream = await answerMediaWithFallback(CallMediaKind.Video, acquire);
  assert.equal(stream, audioOnly, 'the answer proceeds on audio');
  assert.deepEqual(
    asked,
    [CallMediaKind.Video, CallMediaKind.Audio],
    'the video ask is tried first and the audio retry is the fallback, not the default',
  );
});

test('a voice answer never retries, and a device with no microphone at all still fails', async () => {
  const asked: CallMediaKind[] = [];
  const acquire = (kind: CallMediaKind): Promise<MediaStream> => {
    asked.push(kind);
    return Promise.resolve(fakeStream('voice'));
  };
  const stream = await answerMediaWithFallback(CallMediaKind.Audio, acquire);
  assert.deepEqual(
    asked,
    [CallMediaKind.Audio],
    'a voice call has nothing to fall back from: one ask, no retry',
  );
  assert.ok(stream !== undefined);

  const nothing: CallMediaKind[] = [];
  const noMic = (kind: CallMediaKind): Promise<MediaStream> => {
    nothing.push(kind);
    return Promise.reject(new DOMException('no microphone either', 'NotFoundError'));
  };
  await assert.rejects(
    answerMediaWithFallback(CallMediaKind.Video, noMic),
    'with no microphone the audio retry fails too, and the original failure stands for the decline',
  );
  assert.deepEqual(nothing, [CallMediaKind.Video, CallMediaKind.Audio]);
});
