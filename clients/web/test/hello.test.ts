/**
 * The client handshake fields, derived from the browser environment.
 *
 * `webHello` and `deviceDisplayName` feed what the server records about this session: the reported
 * platform and locale, and the human-readable device name that appears on the account's device list.
 * None of it is security-critical, but two things quietly matter. The locale derivation must survive a
 * browser (or a server-side prerender) that exposes no `navigator.language`, or the handshake builder
 * throws on a value it assumed was there and the whole connection never starts. And the device-name
 * user-agent checks are order-sensitive — an Edge user agent also contains "Chrome", a Chrome user
 * agent also contains "Safari" — so a reordering that looks harmless would mislabel every device. The
 * tests pin the fallbacks and the precedence with the ambiguous user agents that expose a wrong order.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { BandwidthMode, Platform } from '@migo/sdk';

import { config } from '../src/lib/config.js';
import { deviceDisplayName, webHello } from '../src/lib/migo/hello.js';
import { installNavigator } from './support/dom-stubs.js';

test('the hello reports the web platform, the configured version, and server-paced bandwidth', () => {
  const restore = installNavigator({ language: 'en-GB' });
  try {
    const hello = webHello();
    assert.equal(hello.platform, Platform.Web);
    assert.equal(hello.bandwidthMode, BandwidthMode.Auto);
    assert.equal(hello.appVersion, config.appVersion);
  } finally {
    restore.restore();
  }
});

test('the hello uses the browser locale when one is present', () => {
  const restore = installNavigator({ language: 'fr-FR' });
  try {
    assert.equal(webHello().locale, 'fr-FR');
  } finally {
    restore.restore();
  }
});

test('the hello falls back to "en" when the environment exposes no locale', () => {
  const restore = installNavigator({ language: undefined });
  try {
    assert.equal(webHello().locale, 'en');
  } finally {
    restore.restore();
  }
});

test('the device name recognises each browser from its user agent', () => {
  const cases: Array<[string, string]> = [
    // A real Edge UA also contains "Chrome"; Edge must win because it is checked first.
    ['Mozilla/5.0 (Windows) AppleWebKit/537 Chrome/120 Safari/537 Edg/120', 'Migo Web (Edge)'],
    ['Mozilla/5.0 (Windows) AppleWebKit/537 Chrome/120 Safari/537', 'Migo Web (Chrome)'],
    ['Mozilla/5.0 (Windows) Gecko Firefox/121', 'Migo Web (Firefox)'],
    // A real Safari UA contains "Safari" but not "Chrome".
    ['Mozilla/5.0 (Macintosh) AppleWebKit/605 Version/17 Safari/605', 'Migo Web (Safari)'],
  ];
  for (const [ua, expected] of cases) {
    const restore = installNavigator({ userAgent: ua });
    try {
      assert.equal(deviceDisplayName(), expected);
    } finally {
      restore.restore();
    }
  }
});

test('the device name falls back to plain "Migo Web" for an unrecognised user agent', () => {
  const restore = installNavigator({ userAgent: 'SomeRobot/1.0' });
  try {
    assert.equal(deviceDisplayName(), 'Migo Web');
  } finally {
    restore.restore();
  }
});

test('the device name is safe when there is no navigator at all (prerender)', () => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: undefined });
  try {
    assert.equal(deviceDisplayName(), 'Migo Web');
    assert.equal(webHello().locale, 'en');
  } finally {
    if (previous) {
      Object.defineProperty(globalThis, 'navigator', previous);
    } else {
      Reflect.deleteProperty(globalThis, 'navigator');
    }
  }
});
