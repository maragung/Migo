/**
 * One virtual user: a throwaway account plus the real {@link MigoClient} that drives it.
 *
 * The whole point of the load generator is that a VU is not a mock. It goes through the exact SDK
 * path a browser would — REST register, gateway handshake, key publication, end-to-end sealing — so
 * what the run measures is the real system under real crypto, not a stubbed happy path. Each VU gets
 * its own in-memory key store (the SDK's default when none is supplied), which keeps VUs
 * cryptographically independent, just as separate devices are.
 */

import { BandwidthMode, DEFAULT_CLIENT_FEATURES, MigoClient, Platform, protocol } from '@migo/sdk';
import type { ConnectionState, EventErrorHandler, Id, WireBytes } from '@migo/sdk';

import { clientEndpoint } from './config.js';
import type { Config } from './config.js';

/**
 * The bits a load-generator session offers: the SDK's stock set plus the call family.
 *
 * `CALLS` is here because a scenario drives it and the server gates it. Every opcode the `calls`
 * scenario touches — 224 `CALL_INVITE` through 231 `CALL_ICE`, and the `CALL_INVITE_EVENT` that
 * answers one — carries `feature: "CALLS"` in the protocol table, the gateway refuses a frame
 * whose bit the session did not offer, and it withholds the family's events from such a session
 * as well, so a run that never offers the bit measures its own refusal rather than the relay it is
 * named for. That is what the first honest full-scale run printed: `call-invite` with `ok: 0`
 * against 466,958 errors, every one `remote:FEATURE_NOT_NEGOTIATED`, and an `errorRate` of 0.9979
 * on a step that had connected all 1,000 sessions and held its whole 120-second window — the
 * receiver's session was as silent as the caller's, for the same missing bit.
 *
 * The stock set stays the base rather than being spelled out again here, so a bit added to the
 * SDK's default reaches this tool without an edit. The two families the SDK leaves to the
 * application stay left out for the reason it gives: `GROUP_CALL`'s SFU opcodes are not
 * feature-gated by the server at all, and no scenario drives them. `ECONOMY` and `GAMES` are the
 * same case seen from `clients/web`'s side — a bit belongs to a client that spends and plays, and
 * this one does neither, so offering either would be a promise about work the tool does not do.
 *
 * Offering a bit a scenario never uses costs that scenario nothing: the server checks a bit
 * against the frames that carry it, not against the session, so the ten thousand idle sessions of
 * the connect scenario pay for `CALLS` with nothing beyond the bitmask they were already sending.
 */
const LOADGEN_FEATURES: bigint = DEFAULT_CLIENT_FEATURES | protocol.FEATURE.CALLS;

export interface VirtualUserDeps {
  readonly config: Config;
  readonly passphrase: string;
  /** Per-run tag mixed into usernames so repeated runs never collide on a taken username. */
  readonly runTag: string;
  /**
   * Sink for inbound event-handling errors, surfaced by the client off the request path.
   *
   * Typed as the SDK's own handler rather than as a one-parameter arrow. The loose annotation is
   * what let the arity slip once already: `EventErrorHandler` is
   * `(opcode: number, cause: unknown) => void`, so a `(error: unknown) => void` here says the SDK
   * may call this sink with one argument — which a two-parameter recorder cannot honour, and the
   * build is where that has to be discovered. Under the loose type it compiled, the opcode
   * arrived in place of the cause, and 255 real fan-out failures reached the report as
   * `unknown`. See `eventErrorRecorder`.
   */
  readonly onEventError: EventErrorHandler;
  /**
   * Sink for the SDK transport's connection-state transitions, passed straight through to
   * {@link MigoClient}. Loadgen reads it only to say where a stalled run had got to: a run that
   * drains mid-connect fails identically whether it never opened a socket or opened one and never
   * finished the handshake, and those are different bugs.
   */
  readonly onStateChange: (state: ConnectionState) => void;
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
      // Built from both URLs rather than from `apiUrl` alone: see `clientEndpoint`. The SDK's
      // own derivation reads a loopback `http://` origin as the split-port dev pair, which is
      // not the single-port node these harnesses start.
      server: clientEndpoint(deps.config),
      deviceDisplayName: `loadgen/${deps.runTag}/${index}`,
      requestTimeoutMs: deps.config.requestTimeoutMs,
      hello: {
        platform: Platform.LoadTest,
        appVersion: deps.config.appVersion,
        locale: deps.config.locale,
        bandwidthMode: BandwidthMode.Normal,
        features: LOADGEN_FEATURES,
      },
      onEventError: deps.onEventError,
      onStateChange: deps.onStateChange,
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
   * the reading is the whole session's — *while the session is open*. Callers must take the
   * reading before {@link stop}: the counters live on the SDK transport, and the SDK answers zero
   * for both directions once no session is established, so a snapshot taken after teardown
   * reports nothing no matter how much crossed the wire. The REST bootstrap (register, key
   * publication) rides HTTP and is not part of these counters.
   */
  wireBytes(): WireBytes {
    return this.client.wireBytes;
  }
}
