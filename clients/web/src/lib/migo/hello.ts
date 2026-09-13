/**
 * Builds the {@link ClientHello} for this web client.
 *
 * The hello reports the platform, app version, locale, and a bandwidth preference to the server. The
 * access token and device id are filled in by the client from the grant, so they are intentionally
 * absent from {@link ClientHello}.
 *
 * The feature set is the stock client's plus the families this client renders: `GROUP_CALL`, and
 * the `CALLS`, `ECONOMY`, and `GAMES` bits the wire now gates the families' opcodes on (the
 * server refuses a frame for a feature the session did not advertise, and withholds the family's
 * events too). Offering a bit is a statement about this client: the call overlay answers rings,
 * the gift picker and the wallet spend the balance, and the game launcher plays — each half is
 * the reason its bit is here rather than in the SDK's stock set, which stays neutral because a
 * client that cannot render a family should not promise it.
 *
 * `RICH_PRESENCE` arrives with the stock set rather than being added here, and this client is the
 * reason it is in that set: it writes the status the bit gates (the profile panel's save and the me
 * bar's publish both carry `customStatus`) and renders what comes back (the profile panel, the
 * friends list, and both me bars). A session that offered the bit without those two halves would
 * be a promise it could not keep.
 */

import { BandwidthMode, DEFAULT_CLIENT_FEATURES, Platform, protocol } from '@migo/sdk';
import type { ClientHello } from '@migo/sdk';

import { config } from '@/lib/config.js';

/** The bits a browser session offers: the stock set plus the families this client renders. */
const WEB_FEATURES: bigint =
  DEFAULT_CLIENT_FEATURES |
  protocol.FEATURE.GROUP_CALL |
  protocol.FEATURE.CALLS |
  protocol.FEATURE.ECONOMY |
  protocol.FEATURE.GAMES;

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
