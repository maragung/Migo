/**
 * The hello the store reports — the same shape the web client sends, so the server sees one
 * browser family, not two.
 */

import { BandwidthMode, Platform } from '@migo/sdk';
import type { ClientHello } from '@migo/sdk';

import { config } from './config.js';

/** The hello for a browser session of the store. */
export function storeHello(): ClientHello {
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
    return 'Migo Store';
  }
  const ua = navigator.userAgent;
  if (/edg\//i.test(ua)) return 'Migo Store (Edge)';
  if (/chrome\//i.test(ua)) return 'Migo Store (Chrome)';
  if (/firefox\//i.test(ua)) return 'Migo Store (Firefox)';
  if (/safari\//i.test(ua)) return 'Migo Store (Safari)';
  return 'Migo Store';
}
