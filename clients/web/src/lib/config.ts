/**
 * Runtime configuration, read from build-time public environment variables.
 *
 * Everything here is public by construction: the values are inlined into the browser bundle. The API
 * origin and gateway URL are endpoints, not secrets. The client never embeds a server secret of any
 * kind; see `.env.example`.
 *
 * The user's actual choice of host, port and scheme is now the {@link ServerEndpoint} persisted in
 * IndexedDB. The env-supplied URL remains the fallback when no snapshot exists: it is what a default
 * production build points at, and a self-hosted build can override it at deploy time.
 */

import { serverEndpointFromUrl } from '@migo/sdk';
import type { ServerEndpoint } from '@migo/sdk';

export interface WebConfig {
  /**
   * The REST origin the bundled env supplies. Used only as a fallback for the very first visit;
   * the runtime reads the user's chosen server from IndexedDB on every subsequent load.
   */
  defaultApiUrl: string;
  /** Reported to the server in the handshake; informational only. */
  appVersion: string;
}

const DEFAULT_API_URL = 'http://localhost:18080';
const DEFAULT_APP_VERSION = '0.1.0';

export const config: WebConfig = {
  defaultApiUrl: process.env.NEXT_PUBLIC_MIGO_API_URL ?? DEFAULT_API_URL,
  appVersion: process.env.NEXT_PUBLIC_MIGO_APP_VERSION ?? DEFAULT_APP_VERSION,
};

/** The fallback endpoint the very first visit (no persisted snapshot) uses. */
export function defaultServerEndpoint(): ServerEndpoint {
  return serverEndpointFromUrl(config.defaultApiUrl);
}
