'use client';

/**
 * The device half of the call stack: which microphone, camera, and speaker a call uses, and the
 * constraints each is acquired under.
 *
 * Section 180 makes device choice a requirement rather than a nicety — a voice call must offer
 * speaker, earpiece, Bluetooth, and wired headset with a switch between them *while the call runs*,
 * and a video call must offer a front/back camera switch. None of that is a permission the
 * application can assert: the platform decides what exists, the labels are empty until a call has
 * granted access, and several browsers have no output selection at all. So everything here is a
 * pure reading of a device list plus the constraints to acquire with, and the manager above decides
 * what to do with what the platform actually reports.
 *
 * # Why the constraints are written out
 *
 * The section names echo cancellation, noise suppression, and automatic gain control explicitly.
 * They are not free: each costs CPU, and a gain control that pumps a quiet room up to noise is a
 * worse call than one that does not. Naming them makes the call's audio processing a decision this
 * client made and can be read, rather than whatever the browser's defaults happened to be for the
 * current release — and the browser is free to ignore any of the three, which the UI must not
 * claim otherwise about.
 */

/** One selectable device, named as the list shows it. */
export interface CallDevice {
  /** The `MediaDeviceInfo.deviceId`, which is what constraints and `setSinkId` take. */
  id: string;
  /**
   * What to call it on screen. A device's `label` is empty until a call has granted capture
   * permission, so the fallback is a position rather than an invented name.
   */
  label: string;
}

/** The audio processing section 180 requires, named rather than left to a browser default. */
export function callAudioConstraints(): MediaTrackConstraints {
  return {
    echoCancellation: true,
    noiseSuppression: true,
    autoGainControl: true,
  };
}

/**
 * The constraints a call's camera is acquired under, or `false` for a call that wants no camera.
 *
 * An `exact` device id is deliberately not used: a camera that has been unplugged or a device id
 * that has rotated since the user chose it would make `getUserMedia` fail outright, and a call that
 * cannot open the user's second-choice camera is a worse outcome than one that opens the first.
 * The device id is therefore an ideal, and the browser substitutes when it must.
 */
export function callVideoConstraints(deviceId: string | null): boolean | MediaTrackConstraints {
  return deviceId === null ? true : { deviceId: { ideal: deviceId } };
}

/**
 * The audio outputs the platform offers, in list order.
 *
 * An empty list is the honest answer on a browser with no output selection, and on one whose
 * permission gate hides the devices — the caller decides what to draw from it, never the list.
 */
export function outputDevicesOf(devices: readonly MediaDeviceInfo[]): CallDevice[] {
  return named(
    devices.filter((device) => device.kind === 'audiooutput'),
    'Output',
  );
}

/** The cameras the platform offers, in list order — the front/back list a video call switches over. */
export function cameraDevicesOf(devices: readonly MediaDeviceInfo[]): CallDevice[] {
  return named(
    devices.filter((device) => device.kind === 'videoinput'),
    'Camera',
  );
}

function named(devices: readonly MediaDeviceInfo[], noun: string): CallDevice[] {
  const seen = new Map<string, number>();
  return devices.map((device, index) => {
    // A blank label is the permission gate, not a device without a name; counting from one is the
    // one description that is true of every unlabelled device.
    const base = device.label === '' ? `${noun} ${index + 1}` : device.label;
    const count = (seen.get(base) ?? 0) + 1;
    seen.set(base, count);
    // Two outputs can share a label — a headset that presents two endpoints — and a menu with two
    // identical rows is a menu the user cannot use. The position is what tells them apart.
    return { id: device.deviceId, label: count === 1 ? base : `${base} (${count})` };
  });
}

/**
 * The camera to switch to, given the list, the one in use, and a direction.
 *
 * Wraps, because a two-camera phone switching forward from the back camera should land on the
 * front one rather than stopping — the control's promise is "the other camera", and with exactly
 * two there is only one other. Null when there is nothing to switch to, which is the fact a control
 * that would otherwise do nothing needs.
 */
export function switchCameraId(
  cameras: readonly CallDevice[],
  current: string | null,
  direction: 1 | -1 = 1,
): string | null {
  if (cameras.length < 2) {
    return null;
  }
  const index = cameras.findIndex((camera) => camera.id === current);
  // An unknown current camera — the browser substituted one — starts from the front of the list.
  const from = index === -1 ? 0 : index;
  return cameras[(from + direction + cameras.length) % cameras.length]?.id ?? null;
}

/**
 * The one method an output selector has, declared here rather than relied on from the platform's
 * types: whether `setSinkId` is in the lib this compiles against depends on the TypeScript release,
 * and a browser that lacks the method must be *told* about it at runtime rather than fail to build.
 */
type OutputSink = { setSinkId?: (deviceId: string) => Promise<void> };

/** The prototype reached without the compiler's help, for the same reason as {@link OutputSink}. */
function outputSinkOf(element: object): OutputSink {
  return element as unknown as OutputSink;
}

/**
 * Whether this browser can choose an audio output at all.
 *
 * `setSinkId` is the only mechanism, and it is absent on Firefox and Safari. The control is drawn
 * from this answer rather than from the device list, because a list of outputs on a browser that
 * cannot route to them would be a button that lies about what it did.
 */
export function canSelectOutput(): boolean {
  return (
    typeof HTMLMediaElement !== 'undefined' &&
    typeof outputSinkOf(HTMLMediaElement.prototype).setSinkId === 'function'
  );
}

/**
 * Routes an element's audio to one output, or back to the platform's default when given null.
 *
 * Applied to the element rather than carried on the stream: the sink is a property of the playback
 * element, so it has to be re-applied whenever the choice moves. Resolves without doing anything on
 * a browser that has no output selection, and rejects when the device has gone — which the caller
 * treats as "the call keeps playing where it was" rather than as a failed call.
 */
export async function applyOutputDevice(
  element: HTMLMediaElement,
  deviceId: string | null,
): Promise<void> {
  const sink = outputSinkOf(element);
  if (typeof sink.setSinkId !== 'function') {
    return;
  }
  await sink.setSinkId(deviceId ?? '');
}
