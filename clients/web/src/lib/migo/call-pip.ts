/**
 * Picture-in-picture for a call: whether this browser can float one, and how the floating window is
 * asked for.
 *
 * Section 180 asks for picture-in-picture, and on the web that name covers two different mechanisms
 * with different consequences:
 *
 *   - The **Document Picture-in-Picture** API opens a real always-on-top window with its own
 *     document, into which the call can render whatever it likes. That is the one Migo must have,
 *     because a floating call that carries no controls is a call the user has to come back to the
 *     tab to hang up: the window gets the peer's name, the state, the timer, and the mute, camera
 *     and hang-up buttons, so moving the call out of the tab does not take the ability to act on it.
 *   - **Element picture-in-picture** floats a single `video` element and nothing else. The browser
 *     draws its own return-to-tab affordance around it and the page cannot add a control, so this is
 *     a fallback that keeps the picture visible rather than a way to run a call.
 *
 * The order is therefore a capability order and not a preference: whichever of the two a browser
 * offers, the better one wins, and the pair is read at runtime because support differs by browser
 * and by release. There is no third answer — a browser with neither gets no control at all, since a
 * button that cannot float anything would be a button that lies about what it did.
 *
 * Nothing here touches a stream, a peer connection, or the call's signalling: floating a call is a
 * placement of the same media in a different window, and both mechanisms leave the sender, the
 * receiver, and the encryption exactly where they were.
 */

/** Which mechanism this browser offers for a floating call, best first. */
export type PipMode = 'document' | 'element' | 'none';

/** What a browser was found to offer, as a value rather than as a read of the globals. */
export interface PipSurroundings {
  /** Whether the Document Picture-in-Picture API is present. */
  documentPictureInPicture: boolean;
  /** Whether the document reports element picture-in-picture as enabled. */
  elementPictureInPicture: boolean;
}

/**
 * The better of the two mechanisms this browser offers, or none.
 *
 * Written over an explicit value rather than over the globals so the ordering is pinnable: the
 * interesting cases are a browser with both — where the document window has to win — and one with
 * neither, which is the state this function is called in on a server.
 */
export function pipModeOf(surroundings: PipSurroundings): PipMode {
  if (surroundings.documentPictureInPicture) {
    return 'document';
  }
  if (surroundings.elementPictureInPicture) {
    return 'element';
  }
  return 'none';
}

/**
 * The Document Picture-in-Picture entry point as this client needs it.
 *
 * Declared here rather than taken from the DOM lib because the API is not in it yet: TypeScript's
 * `lib.dom.d.ts` types element picture-in-picture but has no `documentPictureInPicture`, and the
 * window it opens is an ordinary `Window` whose document the call renders into.
 */
interface DocumentPictureInPictureApi {
  requestWindow(options?: { width?: number; height?: number }): Promise<Window>;
}

/** The API object, or null where the browser does not have it. */
function documentPipApi(): DocumentPictureInPictureApi | null {
  if (typeof globalThis === 'undefined') {
    return null;
  }
  const api = (globalThis as { documentPictureInPicture?: unknown }).documentPictureInPicture;
  if (typeof api !== 'object' || api === null) {
    return null;
  }
  const request = (api as { requestWindow?: unknown }).requestWindow;
  if (typeof request !== 'function') {
    return null;
  }
  return api as DocumentPictureInPictureApi;
}

/**
 * What this browser offers right now.
 *
 * A server render has no document, and the honest answer there is none: the control is drawn from
 * this, so a guess that assumed a browser would put a button on a page that cannot honour it.
 */
export function pipMode(): PipMode {
  if (typeof document === 'undefined') {
    return 'none';
  }
  return pipModeOf({
    documentPictureInPicture: documentPipApi() !== null,
    // The document's own gate rather than the method's presence: a browser can have the method and
    // still refuse to enter, and this property is the one that answers that before the press.
    elementPictureInPicture: document.pictureInPictureEnabled === true,
  });
}

/** The opening size of a floating call window, in CSS pixels. */
export interface PipWindowSize {
  width: number;
  height: number;
}

/**
 * How large the floating window is asked to be.
 *
 * A video call opens at the picture's own proportions, 16:9, at a size that stays watchable on a
 * laptop without covering the work behind it; a voice call has no picture to make room for and gets
 * a strip tall enough for the peer's name and one row of controls. Both are requests rather than
 * settings — the user can resize the window, and a browser may ignore the size entirely — so
 * nothing in the call depends on the numbers landing.
 */
export function pipWindowSize(hasVideo: boolean): PipWindowSize {
  return hasVideo ? { width: 320, height: 180 } : { width: 300, height: 104 };
}

/**
 * How large the floating window is asked to be for a group call.
 *
 * A group call's card is a roster rather than a picture — one floated element can carry one
 * participant's video and no controls, which is not what a group call is — so the window asks for a
 * column tall enough that several seats, the count and a row of controls are all visible at once,
 * and its proportions follow that list rather than any video the call may also be carrying.
 */
export function groupPipWindowSize(): PipWindowSize {
  return { width: 320, height: 360 };
}

/**
 * Opens the floating window, or reports that it did not open.
 *
 * A request that is refused — the user has turned the API off, or the browser declines a window
 * opened outside a gesture — resolves to null, and the call stays where it is rather than being
 * left in a state that claims it floated. The caller closes the window it is given.
 */
export async function openPipWindow(size: PipWindowSize): Promise<Window | null> {
  const api = documentPipApi();
  if (api === null) {
    return null;
  }
  try {
    return await api.requestWindow({ width: size.width, height: size.height });
  } catch {
    return null;
  }
}

/**
 * Floats one video element, the fallback where the document API is absent.
 *
 * The element is the browser's from the moment this resolves: it is moved into the browser's own
 * window and restored on exit, and the page keeps no control over it while it floats. That is the
 * whole difference between the two mechanisms, and the reason this one is only drawn from a video
 * call — a voice call has no element to float.
 *
 * The caller is told when the float ends, because the end can come from the browser rather than from
 * this client: the user closes the floating window, or the video leaves the document. The element is
 * the one that reports it — the event belongs to the element and this is the only place that holds
 * it — and a listener that outlived a refused request would leave the caller believing a call was
 * floating, so one that fails to start is taken straight back off.
 */
export async function enterElementPip(
  video: HTMLVideoElement | null,
  onLeave: () => void,
): Promise<boolean> {
  if (video === null || typeof video.requestPictureInPicture !== 'function') {
    return false;
  }
  video.addEventListener('leavepictureinpicture', onLeave, { once: true });
  try {
    await video.requestPictureInPicture();
    return true;
  } catch {
    video.removeEventListener('leavepictureinpicture', onLeave);
    return false;
  }
}

/** Takes the floating element back into the page, doing nothing where nothing is floating. */
export async function exitElementPip(): Promise<void> {
  if (typeof document === 'undefined' || document.pictureInPictureElement === null) {
    return;
  }
  try {
    await document.exitPictureInPicture();
  } catch {
    // The element left on its own between the check and the call — the user closed the floating
    // window, or the call ended. There is nothing left to take back.
  }
}

/** Whether an element is floating right now, without assuming a document exists. */
export function elementPipActive(): boolean {
  return typeof document !== 'undefined' && document.pictureInPictureElement !== null;
}
