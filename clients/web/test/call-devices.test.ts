/**
 * The device half of the call stack: what a device list means, and the constraints a call is
 * acquired under.
 *
 * Section 180 makes device choice a requirement — speaker, earpiece, Bluetooth, and wired headset
 * with movement between them while a call runs; a front/back camera switch; echo cancellation,
 * noise suppression, and automatic gain control named rather than left to a browser default. All of
 * it is a reading of what the platform reports, so the tests here are about the readings: an empty
 * list staying empty, a label that the permission gate blanked out, two outputs sharing a name, and
 * a camera switch that wraps rather than stopping.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  applyOutputDevice,
  callAudioConstraints,
  callVideoConstraints,
  cameraDevicesOf,
  canSelectOutput,
  microphoneDevicesOf,
  outputDevicesOf,
  switchCameraId,
} from '../src/lib/migo/call-devices.js';
import type { CallDevice } from '../src/lib/migo/call-devices.js';

/** One `MediaDeviceInfo` as `enumerateDevices` hands it over: a kind, an id, and a label. */
function info(kind: MediaDeviceKind, deviceId: string, label: string): MediaDeviceInfo {
  return { kind, deviceId, label, groupId: `group-${deviceId}`, toJSON: () => ({}) };
}

test('a device list is read by kind, and an unlabelled one is numbered rather than named', () => {
  const devices = [
    info('audioinput', 'mic-1', 'Internal microphone'),
    info('videoinput', 'cam-1', 'Front camera'),
    // The label is empty until a call has granted capture permission; that is the permission gate,
    // not a device without a name, and counting is the one description true of them all.
    info('videoinput', 'cam-2', ''),
    info('audiooutput', 'out-1', 'Speaker'),
    info('audiooutput', 'out-2', 'Speaker'),
  ];

  assert.deepEqual(cameraDevicesOf(devices), [
    { id: 'cam-1', label: 'Front camera' },
    { id: 'cam-2', label: 'Camera 2' },
  ]);
  // Two outputs can share a label — a headset presenting two endpoints — and a menu with two
  // identical rows is a menu the user cannot use, so the repeat carries its position.
  assert.deepEqual(outputDevicesOf(devices), [
    { id: 'out-1', label: 'Speaker' },
    { id: 'out-2', label: 'Speaker (2)' },
  ]);
  // The microphones are the third list and the only one that is never empty on a running call: a
  // call cannot open a microphone the platform did not report.
  assert.deepEqual(microphoneDevicesOf(devices), [{ id: 'mic-1', label: 'Internal microphone' }]);

  // A platform that reports no output device at all is an empty list, not an invented default:
  // the caller draws no speaker control from it, which is the honest answer.
  assert.deepEqual(outputDevicesOf([info('audioinput', 'mic-1', 'Mic')]), []);
  assert.deepEqual(microphoneDevicesOf([info('audiooutput', 'out-1', 'Speaker')]), []);
});

test('the camera switch wraps, and reports nothing when there is nothing to switch to', () => {
  const cameras: CallDevice[] = [
    { id: 'cam-1', label: 'Front' },
    { id: 'cam-2', label: 'Back' },
    { id: 'cam-3', label: 'Wide' },
  ];
  assert.equal(switchCameraId(cameras, 'cam-1'), 'cam-2');
  assert.equal(switchCameraId(cameras, 'cam-3'), 'cam-1', 'the list wraps rather than stopping');
  assert.equal(switchCameraId(cameras, 'cam-1', -1), 'cam-3');
  // The browser substitutes when the chosen camera is gone, so a current id that is not in the
  // list is a real state — the switch starts from the front of what the platform reports now.
  assert.equal(switchCameraId(cameras, 'gone'), 'cam-2');

  // One camera is the fact the control is drawn from: a two-camera phone switches, a laptop with
  // one webcam has nothing to offer and must not be given a button that does nothing.
  assert.equal(switchCameraId([{ id: 'cam-1', label: 'Only' }], 'cam-1'), null);
  assert.equal(switchCameraId([], null), null);
});

test('a call names its audio processing, and asks for a camera without demanding one', () => {
  // Section 180 names echo cancellation, noise suppression, and automatic gain control. Writing
  // them out makes the call's audio processing a decision that can be read here rather than
  // whatever the current browser release happened to default to.
  assert.deepEqual(callAudioConstraints(), {
    echoCancellation: true,
    noiseSuppression: true,
    autoGainControl: true,
  });

  // A chosen microphone keeps all three, because a switch is about which device hears the user and
  // not about how the audio is processed — and its id is an ideal for the same reason the camera's
  // is: a headset that has been unplugged must not make the acquisition fail on a call that is
  // already running.
  assert.deepEqual(callAudioConstraints('mic-2'), {
    echoCancellation: true,
    noiseSuppression: true,
    autoGainControl: true,
    deviceId: { ideal: 'mic-2' },
  });

  // No camera chosen is the platform's own choice; a chosen one is an ideal rather than exact,
  // because a camera that has been unplugged or a device id that has rotated since the user picked
  // it would otherwise make the acquisition fail outright instead of substituting.
  assert.equal(callVideoConstraints(null), true);
  assert.deepEqual(callVideoConstraints('cam-2'), { deviceId: { ideal: 'cam-2' } });
});

test('routing the audio takes effect on the element, and is silent where the browser cannot route', async () => {
  // The sink is a property of the playback element, so the routing is a call on the element rather
  // than anything the stream carries. An empty id is the platform's default, which is the one a
  // browser that just lost the chosen headset lands on.
  const asked: string[] = [];
  const element = {
    setSinkId: (deviceId: string): Promise<void> => {
      asked.push(deviceId);
      return Promise.resolve();
    },
  };

  await applyOutputDevice(element, 'out-2');
  await applyOutputDevice(element, null);
  assert.deepEqual(asked, ['out-2', ''], 'null is the default rather than a skipped call');

  // A browser without output selection — Firefox and Safari — must resolve without doing anything
  // rather than throw into a call that has no control to draw the choice from anyway.
  await applyOutputDevice({}, 'out-1');

  // The gate the control is drawn from is the platform's own, and in Node there is no
  // HTMLMediaElement to ask: the honest answer is no, never a guess that assumes a browser.
  assert.equal(canSelectOutput(), false);
});
