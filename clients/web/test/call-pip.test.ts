/**
 * Which mechanism floats a call, and what happens when the browser refuses.
 *
 * Section 180 asks for picture-in-picture, and the web answers that name with two different things:
 * a document window that can carry the call's own controls, and a floating video element that the
 * page cannot put a control into. The tests here pin the order between them — the document window
 * must win wherever it exists, because the fallback is a picture the user has to come back to the
 * tab to hang up — and pin the refusals, since every one of these calls is a request the browser may
 * decline and a call must not be left claiming it floated when it did not.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  elementPipActive,
  enterElementPip,
  exitElementPip,
  groupPipWindowSize,
  openPipWindow,
  pipMode,
  pipModeOf,
  pipWindowSize,
} from '../src/lib/migo/call-pip.js';

test('the better of the two mechanisms wins, and neither is a control at all', () => {
  // A browser with both — which is where the Document Picture-in-Picture API lands as it spreads —
  // must take the document window, because the element fallback drops every control the call has.
  assert.equal(
    pipModeOf({ documentPictureInPicture: true, elementPictureInPicture: true }),
    'document',
  );
  assert.equal(
    pipModeOf({ documentPictureInPicture: true, elementPictureInPicture: false }),
    'document',
  );
  // A browser with only the element API still gets a floating picture, which is the honest reading
  // of the requirement there: it is what that browser can do.
  assert.equal(
    pipModeOf({ documentPictureInPicture: false, elementPictureInPicture: true }),
    'element',
  );
  // Neither is a browser that must be offered no control at all, rather than a button that cannot
  // float anything.
  assert.equal(
    pipModeOf({ documentPictureInPicture: false, elementPictureInPicture: false }),
    'none',
  );
});

test('a runtime without a document offers nothing, rather than assuming a browser', () => {
  // The capability is read at runtime because support differs by browser and by release. In Node —
  // which is where the server render and this test run — there is no document, and the honest
  // answer is none: a guess here is a button on a page that cannot honour it.
  assert.equal(pipMode(), 'none');
  assert.equal(elementPipActive(), false);
});

test('the floating window opens at the picture’s proportions, and as a strip for a voice call', () => {
  const video = pipWindowSize(true);
  // 16:9, the shape a camera publishes: a window at any other ratio would letterbox the peer.
  assert.equal(video.width / video.height, 16 / 9);
  assert.ok(video.width > video.height, 'a video window is wider than it is tall');

  // A voice call has no picture, so the window carries an identity line and one row of controls.
  // It stays wider than tall — the peer's name is a line of text, not a column — and is shorter
  // than the video case, since nothing has to be seen in it.
  const voice = pipWindowSize(false);
  assert.ok(voice.height < video.height);
  assert.ok(voice.width > voice.height);

  // A group call has no picture to follow at all — its card is a roster — so its window is a column
  // tall enough for several seats rather than either of the one-to-one shapes, and it does not vary
  // with the media kind the way the one-to-one pair does.
  const group = groupPipWindowSize();
  assert.ok(group.height > video.height, 'a roster needs more height than a picture');
  assert.ok(group.width <= video.width, 'and no more width than the picture window');
});

test('a refused window leaves the call where it is', async () => {
  // No API in this runtime, so the request has nothing to open. The manager treats null as "still
  // in the tab", which is the state a refused request has to land in: the alternative is a control
  // that reads as pressed while nothing is floating.
  assert.equal(await openPipWindow(pipWindowSize(true)), null);

  // The fallback refuses the same way: there is no element to float, and a call with no video yet
  // must not throw out of the press.
  assert.equal(await enterElementPip(null, () => {}), false);

  // Taking a call back out of a float that does not exist is silence rather than a throw, because
  // the element leaves on its own when the user closes the browser's window.
  await exitElementPip();
});
