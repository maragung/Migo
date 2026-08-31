/**
 * The user's chosen server address.
 *
 * The bootstrap surface used to be a single string the caller had to build — `https://node.example/ws`,
 * say — and a typo in any byte silently routed the client at a different deployment. A user who
 * self-hosts wants a chance to type the address, see what they typed, and not have the SDK then turn
 * that into an obscure URL on their behalf. This is the small, explicit shape that lets them do
 * that: every field is something they can change in the form, every derived URL comes from those
 * fields by a rule the file documents, and a one-line shorthand is parsed without ever becoming a
 * second way to express the same thing.
 *
 * The transport and scheme enums are deliberately split: a WebSocket on `WS` is the plain-TCP dev
 * affordance, a WebSocket on `WSS` is the production form, and QUIC is the second realtime transport
 * option. A server advertises QUIC via the `QUIC` feature bit only when its optional QUIC listener
 * is enabled (a QUIC-capable host offers `FEATURE.QUIC` itself through `hello.features`). This SDK
 * validates and persists the choice; the data path it opens is still WebSocket.
 */

/** The transport the realtime socket speaks. */
export type Transport = 'WebSocket' | 'Quic';

/** The TLS posture of the WebSocket transport. */
export type WsScheme = 'Ws' | 'Wss';

/** The TLS posture of the QUIC transport. */
export type QuicScheme = 'Quic' | 'QuicTls';

/** The TLS posture for the REST surface. */
export type RestScheme = 'Http' | 'Https';

/** The scheme paired with the chosen transport. */
export type Scheme = WsScheme | QuicScheme;

/** Hosts that are exempt from the "always use TLS" default. */
const LOOPBACK_HOSTS: ReadonlySet<string> = new Set(['localhost', '127.0.0.1', '::1']);

/**
 * A parsed host, lowercased and with any inline `:port` already split off into {@link ServerEndpoint.port}.
 */
export type Host = string;

/**
 * The user-configured server: one host, one REST port, one gateway port, and the pair of schemes that
 * tell the client whether each side is plain or encrypted.
 *
 * The two ports are split on purpose. They default together (gateway = rest + 1) but the form lets a
 * user who has to, because of a reverse-proxy setup, point the REST origin and the realtime socket
 * at different listeners without either side being magic. The transport enum is the only one that has
 * to grow when a new realtime path lands; the schemes are already expressed at the level the form
 * and the protocol both speak.
 */
export interface ServerEndpoint {
  /** The lowercased host (no scheme, no port, no path). */
  host: Host;
  /** The REST port. Must be in `[1, 65535]`. */
  port: number;
  /** The gateway port. Defaults to `port + 1`; the form exposes an override. */
  gatewayPort: number;
  /** The realtime transport. */
  transport: Transport;
  /** The TLS posture of the realtime transport. */
  scheme: Scheme;
  /** The TLS posture of REST, paired with the realtime choice when sensible. */
  restScheme: RestScheme;
}

/** What a {@link parseServerEndpoint} call can return on a bad input. */
export class ServerEndpointError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ServerEndpointError';
  }
}

/**
 * Builds a {@link ServerEndpoint} for a host that is a development loopback.
 *
 * The default schemes are the only one the dev policy allows: a plain WebSocket and a plain HTTP on
 * the same port, with the gateway on the next one up. A `wss://` request to a loopback would be a
 * user trying to talk to a TLS-only deployment that has nothing to do with their local node, and
 * the form does not need to enable it.
 */
export function defaultLoopbackServerEndpoint(host: string, port = 18080): ServerEndpoint {
  return {
    host: host.toLowerCase(),
    port,
    gatewayPort: port + 1,
    transport: 'WebSocket',
    scheme: 'Ws',
    restScheme: 'Http',
  };
}

/**
 * Builds a {@link ServerEndpoint} for a production-style address.
 *
 * A non-loopback host forces the TLS postures. The REST origin speaks HTTPS and the gateway speaks
 * WSS — the only configuration the audit allows once a deployment is reachable from outside this
 * machine, and the form's default when the user types a real domain.
 */
export function defaultInternetServerEndpoint(host: string, port = 443): ServerEndpoint {
  return {
    host: host.toLowerCase(),
    port,
    gatewayPort: port,
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  };
}

/**
 * True when the host is one the local-dev policy applies to: a plain WebSocket is allowed there,
 * a `https://` request would point at a TLS-only deployment that has nothing to do with this machine.
 */
export function isLoopbackHost(host: string): boolean {
  return LOOPBACK_HOSTS.has(host.toLowerCase());
}

/**
 * Picks a default scheme pair for a host. Loopback defaults to plain (dev), anything else to TLS.
 *
 * Splitting the rule from the constructor means a form field that just lost focus can rebuild its
 * pair without re-running the whole endpoint construction, and a unit test can pin the rule without
 * standing up a full endpoint.
 */
export function defaultSchemesForHost(host: string): { scheme: WsScheme; restScheme: RestScheme } {
  if (isLoopbackHost(host)) {
    return { scheme: 'Ws', restScheme: 'Http' };
  }
  return { scheme: 'Wss', restScheme: 'Https' };
}

/**
 * Parses the two shorthand shapes a user is likely to type, and the split form for everything else.
 *
 * Three inputs are recognised:
 *
 *   - `host` — bare, e.g. `migo.example.com`. The port defaults to {@link DEFAULT_REST_PORT}.
 *   - `host:port` — a single colon and a numeric port, e.g. `migo.example.com:8443`. Anything else
 *     (no colon, no number) is rejected.
 *   - Two arguments, the host and the port. The form uses this when the user has typed the two
 *     fields separately.
 *
 * The IPv6 form (`[::1]:18080`) is intentionally not supported yet; the brief is dev/local, the
 * production path uses hostnames, and adding a parser for a case the form has no field for would
 * mean untested code in the hot path of a register screen.
 */
export function parseHost(
  input: string,
  portFallback = DEFAULT_REST_PORT,
): { host: string; port: number } {
  const trimmed = input.trim();
  if (trimmed === '') {
    throw new ServerEndpointError('host is required');
  }
  const colon = trimmed.indexOf(':');
  if (colon < 0) {
    return { host: trimmed.toLowerCase(), port: portFallback };
  }
  // Multiple colons that are not the IPv6 bracket form are not a host:port the form can take.
  if (trimmed.indexOf(':', colon + 1) >= 0) {
    throw new ServerEndpointError(`host cannot contain more than one colon: ${trimmed}`);
  }
  const host = trimmed.slice(0, colon).toLowerCase();
  const portText = trimmed.slice(colon + 1);
  if (host === '') {
    throw new ServerEndpointError(`host is empty: ${trimmed}`);
  }
  const port = Number.parseInt(portText, 10);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new ServerEndpointError(`port is out of range (1..65535): ${portText}`);
  }
  if (portText !== port.toString()) {
    // Reject `8080abc` and friends — `parseInt` would accept them.
    throw new ServerEndpointError(`port is not a whole number: ${portText}`);
  }
  return { host, port };
}

/** The REST port used when none is supplied. Matches the dev policy default. */
export const DEFAULT_REST_PORT = 18080;
/** The gateway port used when none is supplied. Defaults to `restPort + 1`. */
export const DEFAULT_GATEWAY_PORT_OFFSET = 1;

/**
 * Validates the ranges on the numeric fields. Split out so the constructor and the parser share it.
 */
export function validatePorts(port: number, gatewayPort: number): void {
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new ServerEndpointError(`rest port is out of range (1..65535): ${port}`);
  }
  if (!Number.isInteger(gatewayPort) || gatewayPort < 1 || gatewayPort > 65535) {
    throw new ServerEndpointError(`gateway port is out of range (1..65535): ${gatewayPort}`);
  }
}

/**
 * Result type for {@link validate} on a {@link ServerEndpoint}.
 *
 * A `Result<ServerEndpoint, Error>` is the only public face; the constructors throw on a bad shape
 * (a caller that hands a malformed endpoint in gets a clear error), and the validators return the
 * same shape so a form can show the same message either way.
 */
export interface ServerEndpointValidation {
  /** `null` when the endpoint is well-formed. */
  error: string | null;
}

const VALID_REST_SCHEMES: ReadonlySet<RestScheme> = new Set<RestScheme>(['Http', 'Https']);
const VALID_WS_SCHEMES: ReadonlySet<WsScheme> = new Set<WsScheme>(['Ws', 'Wss']);
const VALID_QUIC_SCHEMES: ReadonlySet<QuicScheme> = new Set<QuicScheme>(['Quic', 'QuicTls']);

/**
 * Checks every field of an endpoint and returns the first failure, or `null` when the shape is sound.
 *
 * The four rules are the same ones a user-visible form rejects on:
 *
 *   1. `host` is not empty.
 *   2. `port` and `gatewayPort` are integers in `[1, 65535]`.
 *   3. The `(transport, scheme)` pair is consistent: a WebSocket transport takes a `Ws` or `Wss`
 *      scheme and never a `Quic*` scheme, and the QUIC transport takes a `Quic*` scheme and
 *      never a `Ws*` scheme.
 *   4. `restScheme` is `Http` or `Https`.
 *
 * The rules are deliberately written as four checks rather than a long boolean expression: each one
 * names the field it is about, so the message a form shows can be the message a test pins.
 */
export function validateServerEndpoint(endpoint: ServerEndpoint): ServerEndpointValidation {
  if (typeof endpoint.host !== 'string' || endpoint.host.trim() === '') {
    return { error: 'host is required' };
  }
  if (!Number.isInteger(endpoint.port) || endpoint.port < 1 || endpoint.port > 65535) {
    return { error: `rest port is out of range (1..65535): ${String(endpoint.port)}` };
  }
  if (
    !Number.isInteger(endpoint.gatewayPort) ||
    endpoint.gatewayPort < 1 ||
    endpoint.gatewayPort > 65535
  ) {
    return { error: `gateway port is out of range (1..65535): ${String(endpoint.gatewayPort)}` };
  }
  if (endpoint.transport === 'WebSocket') {
    if (!VALID_WS_SCHEMES.has(endpoint.scheme as WsScheme)) {
      return { error: 'WebSocket transport requires WS or WSS scheme' };
    }
  } else {
    if (!VALID_QUIC_SCHEMES.has(endpoint.scheme as QuicScheme)) {
      return { error: 'QUIC transport requires QUIC or QUIC-TLS scheme' };
    }
  }
  if (!VALID_REST_SCHEMES.has(endpoint.restScheme)) {
    return { error: 'REST scheme must be HTTP or HTTPS' };
  }
  return { error: null };
}

/**
 * Throws {@link ServerEndpointError} when {@link validateServerEndpoint} returned an error.
 *
 * Split from the validators that return a result so a constructor can call the throwing form and
 * have a single error path, while the form calls the result form and renders the message.
 */
export function assertValidServerEndpoint(endpoint: ServerEndpoint): void {
  const result = validateServerEndpoint(endpoint);
  if (result.error !== null) {
    throw new ServerEndpointError(result.error);
  }
}

/**
 * Returns the gateway scheme prefix, taking the transport into account.
 *
 * Split from the URL builder because the form's display logic wants to show the scheme alone (a
 * select), and a test pins the rule with a plain `assert.equal` rather than a full URL parse.
 */
export function gatewaySchemePrefix(endpoint: ServerEndpoint): string {
  if (endpoint.transport === 'Quic') {
    // Both QUIC schemes map to the single "quic" URL prefix: the TLS posture rides in the REST
    // scheme, not the gateway scheme. This matches the Rust and Kotlin clients.
    return 'quic';
  }
  return endpoint.scheme === 'Wss' ? 'wss' : 'ws';
}

/** The REST scheme prefix for the URL, taking the `restScheme` field on its own. */
export function restSchemePrefix(endpoint: ServerEndpoint): string {
  return endpoint.restScheme === 'Https' ? 'https' : 'http';
}

/**
 * The REST origin, e.g. `http://localhost:18080`. No trailing slash.
 *
 * Used by {@link BootstrapClient} as its base URL; the trailing-slash normalisation the bootstrap
 * client applies internally handles the rest.
 */
export function restBaseUrl(endpoint: ServerEndpoint): string {
  return `${restSchemePrefix(endpoint)}://${endpoint.host}:${endpoint.port}`;
}

/**
 * The gateway WebSocket URL, e.g. `ws://localhost:18081/ws`. No trailing slash.
 *
 * The `/ws` path is the path the server exposes; the SDK does not let callers change it because
 * exposing the path would mean a self-hoster setting it and never knowing which one the server
 * actually answers. The path is the contract.
 */
export function gatewayUrl(endpoint: ServerEndpoint): string {
  return `${gatewaySchemePrefix(endpoint)}://${endpoint.host}:${endpoint.gatewayPort}/ws`;
}

/**
 * Resolves a server URL string (the env or the legacy form) into a {@link ServerEndpoint}.
 *
 * `MIGO_PUBLIC_URL` and the dev default both arrive as a REST origin, e.g. `http://localhost:18080`
 * or `http://migo.example`. The form gives the SDK structured fields today; the env gives it a
 * string from before the form existed. This is the bridge: take the string, derive the
 * endpoint, and let the rest of the SDK forget it was ever a string.
 *
 * The URL's own scheme is the ground truth for both postures — the same rule the Android and
 * desktop clients apply:
 *
 *   - `https://…` is the production pair (`Wss`/`Https`) with the gateway on the same port;
 *   - `http://…` on a loopback host is the dev pair (`Ws`/`Http`) with the gateway on the next
 *     port (the split-port dev policy);
 *   - `http://…` on any other host is a single-port plain deployment (this repository's own
 *     VPS shape: `migod` serves `/ws` on its HTTP listener), so the gateway rides the same
 *     port, plain.
 */
export function serverEndpointFromUrl(url: string): ServerEndpoint {
  const trimmed = url.trim();
  if (trimmed === '') {
    return defaultLoopbackServerEndpoint('localhost');
  }
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new ServerEndpointError(`not a valid URL: ${url}`);
  }
  const scheme = parsed.protocol === 'https:' ? 'Https' : 'Http';
  const restPort =
    parsed.port === '' ? (scheme === 'Https' ? 443 : 80) : Number.parseInt(parsed.port, 10);
  const host = parsed.hostname.toLowerCase();
  // The URL's own scheme decides both postures (see the doc above). Only a plain origin on a
  // loopback host keeps the dev split — gateway on the next port; everything else serves `/ws`
  // on the port the URL names.
  const gatewayScheme: WsScheme = scheme === 'Https' ? 'Wss' : 'Ws';
  const gatewayPort =
    scheme === 'Https' || !isLoopbackHost(host) ? restPort : restPort + DEFAULT_GATEWAY_PORT_OFFSET;
  return {
    host,
    port: restPort,
    gatewayPort,
    transport: 'WebSocket',
    scheme: gatewayScheme,
    restScheme: scheme,
  };
}
