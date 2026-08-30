/**
 * Runtime configuration, resolved from the browser's own location first and build-time
 * environment variables second.
 *
 * Everything here is public by construction: the values are inlined into the browser bundle. The API
 * origin and gateway URL are endpoints, not secrets. The client never embeds a server secret of any
 * kind; see `.env.example`.
 *
 * The user's actual choice of host, port and scheme is the {@link ServerEndpoint} persisted in
 * IndexedDB. The resolution order on a fresh visit (no snapshot):
 *
 *   1. `NEXT_PUBLIC_MIGO_API_URL`, when the build was made with one — the deployer's explicit
 *      instruction, and the strongest signal there is.
 *   2. The browser's own origin: the server that served this page, on port 8080. When the web
 *      client and the Migo server are deployed together (the docker-compose and single-VPS
 *      posture), this is the correct guess on the very first load, and it saves the user from
 *      staring at a "could not reach the server" that was only ever a wrong default.
 *   3. `http://localhost:8080`, the development fallback.
 */

import { serverEndpointFromUrl } from '@migo/sdk';
import type { ServerEndpoint } from '@migo/sdk';

export interface WebConfig {
  /** The REST origin the bundled env supplies, when the build was made with one. */
  defaultApiUrl: string | undefined;
  /** Reported to the server in the handshake; informational only. */
  appVersion: string;
}

const DEFAULT_APP_VERSION = '0.1.0';

export const config: WebConfig = {
  defaultApiUrl: process.env.NEXT_PUBLIC_MIGO_API_URL,
  appVersion: process.env.NEXT_PUBLIC_MIGO_APP_VERSION ?? DEFAULT_APP_VERSION,
};

/**
 * The endpoint a fresh visit (no persisted snapshot) uses.
 *
 * Same-origin detection rather than a burned-in default: the page that served this
 * JavaScript is the page whose server the user is most likely trying to reach, and
 * port 8080 is where that server listens in every deployment this repository ships.
 */
export function defaultServerEndpoint(): ServerEndpoint {
  if (config.defaultApiUrl !== undefined) {
    return serverEndpointFromUrl(config.defaultApiUrl);
  }
  if (typeof window !== 'undefined' && window.location) {
    const { protocol, hostname } = window.location;
    const isSecure = protocol === 'https:';
    const scheme = isSecure ? 'https' : 'http';
    const endpoint = serverEndpointFromUrl(`${scheme}://${hostname}:8080`);
    // The gateway rides the same port as REST (migod serves /ws on its HTTP listener),
    // not the REST port + 1 the URL helper assumes for split-port deployments.
    // The schemes follow the page's own protocol: a server that served this page over
    // plain HTTP has no TLS certificate, so the SDK's non-loopback default of
    // Wss/Https would try to open a TLS connection to a plain-HTTP port and fail.
    return {
      ...endpoint,
      scheme: isSecure ? 'Wss' : 'Ws',
      restScheme: isSecure ? 'Https' : 'Http',
      gatewayPort: endpoint.port,
    };
  }
  return {
    ...serverEndpointFromUrl('http://localhost:8080'),
    scheme: 'Ws',
    restScheme: 'Http',
    gatewayPort: 8080,
  };
}
