/**
 * Builds the {@link ClientHello} for this web client.
 *
 * The hello reports the platform, app version, locale, and a bandwidth preference to the server. The
 * access token and device id are filled in by the client from the grant, so they are intentionally
 * absent from {@link ClientHello}.
 */

import { BandwidthMode, Platform } from '@migo/sdk';
import type { ClientHello } from '@migo/sdk';

import { config } from '@/lib/config.js';

/** The hello for a browser session, using the browser's locale and letting the server pace bandwidth. */
export function webHello(): ClientHello {
  const locale = typeof navigator !== 'undefined' && navigator.language ? navigator.language : 'en';
  return {
    platform: Platform.Web,
    appVersion: config.appVersion,
    locale,
    bandwidthMode: BandwidthMode.Auto,
  };
}

/** A human-readable device name recorded on the account's device list. */
export function deviceDisplayName(): string {
  if (typeof navigator === 'undefined') {
    return 'Migo Web';
  }
  const ua = navigator.userAgent;
  if (/edg\//i.test(ua)) return 'Migo Web (Edge)';
  if (/chrome\//i.test(ua)) return 'Migo Web (Chrome)';
  if (/firefox\//i.test(ua)) return 'Migo Web (Firefox)';
  if (/safari\//i.test(ua)) return 'Migo Web (Safari)';
  return 'Migo Web';
}
