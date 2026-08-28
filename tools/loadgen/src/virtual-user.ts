/**
 * One virtual user: a throwaway account plus the real {@link MigoClient} that drives it.
 *
 * The whole point of the load generator is that a VU is not a mock. It goes through the exact SDK
 * path a browser would — REST register, gateway handshake, key publication, end-to-end sealing — so
 * what the run measures is the real system under real crypto, not a stubbed happy path. Each VU gets
 * its own in-memory key store (the SDK's default when none is supplied), which keeps VUs
 * cryptographically independent, just as separate devices are.
 */

import { MigoClient, Platform, BandwidthMode, serverEndpointFromUrl } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import type { Config } from './config.js';

export interface VirtualUserDeps {
  readonly config: Config;
  readonly password: string;
  /** Per-run tag mixed into usernames so repeated runs never collide on a taken username. */
  readonly runTag: string;
  /** Sink for inbound event-handling errors, surfaced by the client off the request path. */
  readonly onEventError: (error: unknown) => void;
}

export class VirtualUser {
  readonly index: number;
  readonly username: string;
  readonly client: MigoClient;

  /** True once {@link start} has registered and opened the gateway session. */
  connected = false;
  /** For paired scenarios: the peer this VU converses with. */
  partner: VirtualUser | undefined = undefined;
  /** For paired scenarios: the conversation this VU sends into. */
  conversationId: Id | undefined = undefined;

  readonly #config: Config;
  readonly #password: string;

  constructor(index: number, deps: VirtualUserDeps) {
    this.index = index;
    this.username = `${deps.config.usernamePrefix}-${deps.runTag}-${index}`;
    this.#config = deps.config;
    this.#password = deps.password;
    this.client = MigoClient.create({
      server: serverEndpointFromUrl(deps.config.apiUrl),
      deviceDisplayName: `loadgen/${deps.runTag}/${index}`,
      requestTimeoutMs: deps.config.requestTimeoutMs,
      hello: {
        platform: Platform.LoadTest,
        appVersion: deps.config.appVersion,
        locale: deps.config.locale,
        bandwidthMode: BandwidthMode.Normal,
      },
      onEventError: deps.onEventError,
    });
  }

  /** Register the account and open the gateway session. Resolves once the client is ready to send. */
  async start(): Promise<void> {
    await this.client.register({
      username: this.username,
      password: this.#password,
      locale: this.#config.locale,
      country: this.#config.country,
    });
    this.connected = true;
  }

  /** Close the gateway session. Best-effort: teardown must not fail a run that already produced data. */
  async stop(): Promise<void> {
    try {
      await this.client.disconnect();
    } catch {
      // Ignore: the socket may already be gone, and a failed disconnect changes no measurement.
    }
    this.connected = false;
  }
}
