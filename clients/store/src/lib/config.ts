/**
 * Runtime configuration for the store.
 *
 * Same posture as the web client's `lib/config.ts`: everything here is public by construction
 * (inlined into the browser bundle), endpoints and contract addresses — never a secret. The
 * endpoint resolution prefers the browser's own origin, which on the single-VPS deployment is
 * the machine that served this page.
 */

import { serverEndpointFromUrl } from '@migo/sdk';
import type { ServerEndpoint } from '@migo/sdk';

export interface StoreConfig {
  /** Reported to the server in the handshake; informational only. */
  appVersion: string;
}

const DEFAULT_APP_VERSION = '0.1.0';

export const config: StoreConfig = {
  appVersion: DEFAULT_APP_VERSION,
};

/**
 * The endpoint a fresh visit uses: the browser's own origin on port 8080, the same guess the
 * web client makes — the server that served this page is the server whose store this is.
 */
export function defaultServerEndpoint(): ServerEndpoint {
  if (typeof window !== 'undefined' && window.location) {
    const { protocol, hostname } = window.location;
    const scheme = protocol === 'https:' ? 'https' : 'http';
    return serverEndpointFromUrl(`${scheme}://${hostname}:8080`);
  }
  return {
    ...serverEndpointFromUrl('http://localhost:8080'),
    gatewayPort: 8080,
  };
}
