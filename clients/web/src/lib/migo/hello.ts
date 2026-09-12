/**
 * Builds the {@link ClientHello} for this web client.
 *
 * The hello reports the platform, app version, locale, and a bandwidth preference to the server. The
 * access token and device id are filled in by the client from the grant, so they are intentionally
 * absent from {@link ClientHello}.
 *
 * The feature set is the stock client's plus `GROUP_CALL`: the bit is opt-in by the SDK's own rule
 * — a client that offers it should be one that has a group-call UI, and this client now does (the
 * roster screen). The server does not gate the SFU opcodes on the bit, so offering it is a
 * statement about this client, not a request the server must grant.
 */

import { BandwidthMode, DEFAULT_CLIENT_FEATURES, Platform, protocol } from '@migo/sdk';
import type { ClientHello } from '@migo/sdk';

import { config } from '@/lib/config.js';

/** The bits a browser session offers: the stock set, plus the group-call UI this client carries. */
const WEB_FEATURES: bigint = DEFAULT_CLIENT_FEATURES | protocol.FEATURE.GROUP_CALL;

/** The hello for a browser session, using the browser's locale and letting the server pace bandwidth. */
export function webHello(): ClientHello {
  const locale = typeof navigator !== 'undefined' && navigator.language ? navigator.language : 'en';
  return {
    platform: Platform.Web,
    appVersion: config.appVersion,
    locale,
    bandwidthMode: BandwidthMode.Auto,
    features: WEB_FEATURES,
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
