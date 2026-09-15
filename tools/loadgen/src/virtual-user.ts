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
import type { Id, WireBytes } from '@migo/sdk';

import type { Config } from './config.js';

export interface VirtualUserDeps {
  readonly config: Config;
  readonly passphrase: string;
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
  readonly #passphrase: string;

  constructor(index: number, deps: VirtualUserDeps) {
    this.index = index;
    // Underscores, never hyphens: the server's username validator (migo-auth's
    // credential rules) admits only letters, digits, dots and underscores, and
    // a hyphenated name is refused with VALIDATION_FAILED before the run ever
    // opens a session. The run tag is base36 by construction and the default
    // prefix "loadgen" is legal, so the separators are the only place an
    // illegal character can sneak in.
    this.username = `${deps.config.usernamePrefix}_${deps.runTag}_${index}`;
    this.#config = deps.config;
    this.#passphrase = deps.passphrase;
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
      passphrase: this.#passphrase,
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

  /**
   * This session's wire bytes so far (§171): a snapshot of the SDK transport's counters.
   *
   * The counters are the same ones the web client shows in its Diagnostics group — every byte
   * written to the gateway socket at its post-compression wire size (frame headers included,
   * HELLO and ACK included) and every byte read off the socket at the incoming record's outer
   * envelope size. They survive a reconnect and count the reconnect's own handshake, because
   * those bytes are a real cost the client paid; a VU builds exactly one transport per run, so
   * the reading taken after teardown is the whole session. The REST bootstrap (register, key
   * publication) rides HTTP and is not part of these counters.
   */
  wireBytes(): WireBytes {
    return this.client.wireBytes;
  }
}
