/**
 * Runtime configuration, read from build-time public environment variables.
 *
 * Everything here is public by construction: the values are inlined into the browser bundle. The API
 * origin and gateway URL are endpoints, not secrets. The client never embeds a server secret of any
 * kind; see `.env.example`.
 */

export interface WebConfig {
  /** REST origin for bootstrap (register, login, refresh, logout). */
  apiBaseUrl: string;
  /** Gateway WebSocket URL for the resumable realtime session. */
  gatewayUrl: string;
  /** Reported to the server in the handshake; informational only. */
  appVersion: string;
}

const DEFAULT_API_URL = 'http://localhost:8080';
const DEFAULT_GATEWAY_URL = 'ws://localhost:8080/ws';
const DEFAULT_APP_VERSION = '0.1.0';

export const config: WebConfig = {
  apiBaseUrl: process.env.NEXT_PUBLIC_MIGO_API_URL ?? DEFAULT_API_URL,
  gatewayUrl: process.env.NEXT_PUBLIC_MIGO_GATEWAY_URL ?? DEFAULT_GATEWAY_URL,
  appVersion: process.env.NEXT_PUBLIC_MIGO_APP_VERSION ?? DEFAULT_APP_VERSION,
};
