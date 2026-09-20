/**
 * Command-line configuration for the load generator.
 *
 * Everything the tool needs is resolved here into an immutable {@link Config}: flags first, then a
 * few environment fallbacks (the same MIGO_/NEXT_PUBLIC_ variables that point the web client at a
 * server also point this tool at it), then built-in defaults. Parsing is strict — an unknown flag or
 * a malformed value is a {@link ConfigError}, not a silent default — because a load test that
 * quietly ran against the wrong target, or ten times slower than asked, is worse than one that
 * refused to start.
 */

import { serverEndpointFromUrl } from '@migo/sdk';
import type { ServerEndpoint, WsScheme } from '@migo/sdk';

import type { LogLevel } from './logger.js';

export interface Config {
  readonly apiUrl: string;
  readonly gatewayUrl: string;
  readonly scenario: string;
  readonly vus: number;
  readonly durationMs: number;
  readonly ratePerSec: number;
  readonly connectConcurrency: number;
  readonly appVersion: string;
  readonly locale: string;
  readonly country: string;
  readonly usernamePrefix: string;
  readonly passphrase: string | undefined;
  readonly requestTimeoutMs: number;
  readonly maxErrorRate: number;
  readonly output: 'text' | 'json';
  readonly logLevel: LogLevel;
}

export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ConfigError';
  }
}

export type ParseResult =
  { readonly help: true } | { readonly help: false; readonly config: Config };

const BOOLEAN_FLAGS = new Set(['help', 'quiet', 'verbose']);

export function parseArgs(argv: readonly string[], env: NodeJS.ProcessEnv): ParseResult {
  const values = new Map<string, string>();
  const flags = new Set<string>();

  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (token === undefined) continue;
    if (!token.startsWith('--')) throw new ConfigError(`unexpected argument: ${token}`);
    const body = token.slice(2);
    const eq = body.indexOf('=');
    const key = eq >= 0 ? body.slice(0, eq) : body;

    if (BOOLEAN_FLAGS.has(key)) {
      flags.add(key);
      continue;
    }
    if (eq >= 0) {
      values.set(key, body.slice(eq + 1));
      continue;
    }
    const next = argv[i + 1];
    if (next === undefined || next.startsWith('--'))
      throw new ConfigError(`missing value for --${key}`);
    values.set(key, next);
    i += 1;
  }

  if (flags.has('help')) return { help: true };

  const apiUrl =
    values.get('api-url') ??
    env.MIGO_API_URL ??
    env.NEXT_PUBLIC_MIGO_API_URL ??
    'http://localhost:8080';
  const gatewayUrl =
    values.get('gateway-url') ??
    env.MIGO_GATEWAY_URL ??
    env.NEXT_PUBLIC_MIGO_GATEWAY_URL ??
    deriveGatewayUrl(apiUrl);

  const config: Config = {
    apiUrl,
    gatewayUrl,
    scenario: values.get('scenario') ?? 'messaging',
    vus: parsePositiveInt('vus', values.get('vus'), 10),
    durationMs: parseDuration('duration', values.get('duration'), 30_000),
    ratePerSec: parseNonNegative('rate', values.get('rate'), 5),
    connectConcurrency: parsePositiveInt(
      'connect-concurrency',
      values.get('connect-concurrency'),
      20,
    ),
    appVersion: values.get('app-version') ?? env.NEXT_PUBLIC_MIGO_APP_VERSION ?? '0.1.0',
    locale: values.get('locale') ?? 'en-US',
    country: values.get('country') ?? 'ID',
    usernamePrefix: values.get('prefix') ?? 'loadgen',
    passphrase: values.get('passphrase'),
    requestTimeoutMs: parsePositiveInt(
      'request-timeout-ms',
      values.get('request-timeout-ms'),
      15_000,
    ),
    maxErrorRate: parseFraction('max-error-rate', values.get('max-error-rate'), 1),
    output: parseOutput(values.get('output')),
    logLevel: flags.has('quiet') ? 'quiet' : flags.has('verbose') ? 'verbose' : 'normal',
  };

  return { help: false, config };
}

/** Turns a REST base URL into a gateway URL: http->ws, https->wss, and /ws when it has no path. */
function deriveGatewayUrl(apiUrl: string): string {
  let url: URL;
  try {
    url = new URL(apiUrl);
  } catch {
    throw new ConfigError(`--api-url is not a valid URL: ${apiUrl}`);
  }
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  if (url.pathname === '' || url.pathname === '/') url.pathname = '/ws';
  return url.toString();
}

/**
 * The {@link ServerEndpoint} the virtual users dial — built from both URLs, not from `apiUrl` alone.
 *
 * This exists because `serverEndpointFromUrl` cannot be handed a loopback REST URL and be expected to
 * guess right. Its rule is the *dev pair*: on a loopback host a plain `http://` origin is assumed to
 * be the two-listener development shape, so the gateway moves to `rest + 1`. That is the correct
 * reading of `http://localhost:18080` for a developer running the dev pair, and the wrong reading of
 * every node this tool is pointed at: both load harnesses start a single `migod` that serves `/ws`
 * on the one port it binds (`MIGO_HTTP__BIND=127.0.0.1:$NODE_PORT`, `--api-url
 * http://localhost:$NODE_PORT`), so the derived gateway port was one nothing listens on and every
 * virtual user's WebSocket was refused before a single session existed. The run then measured a node
 * it never reached: zero frames in, zero sessions live, an error rate of zero over a denominator of
 * zero, and — because nothing was left holding the event loop — an exit status of zero.
 *
 * So the gateway is not derived here; it is read off {@link Config.gatewayUrl}, which the CLI or the
 * environment has already resolved to a URL naming an actual listener. `--gateway-url` therefore
 * governs the run instead of merely describing it in the report, and the port the report prints is
 * the port the sockets went to. The same explicit override the e2e harness applies, for the same
 * reason, against the same single-port node.
 *
 * The endpoint type carries one host, so the three ways a gateway URL could ask for something this
 * shape cannot say are refused rather than quietly replaced: another host would dial the API host
 * while the report said otherwise, a non-WebSocket scheme has no transport here, and a path other
 * than `/ws` cannot be honoured because the SDK does not let callers choose the gateway path. A load
 * run against the wrong target is worse than one that refused to start — the module's own rule.
 */
export function clientEndpoint(config: Config): ServerEndpoint {
  const endpoint = serverEndpointFromUrl(config.apiUrl);
  let gateway: URL;
  try {
    gateway = new URL(config.gatewayUrl);
  } catch {
    throw new ConfigError(`--gateway-url is not a valid URL: ${config.gatewayUrl}`);
  }
  if (gateway.protocol !== 'ws:' && gateway.protocol !== 'wss:') {
    throw new ConfigError(
      `--gateway-url must be a ws:// or wss:// URL, got "${config.gatewayUrl}" — the SDK's only ` +
        'realtime transport in this build is a WebSocket',
    );
  }
  const gatewayHost = gateway.hostname.toLowerCase();
  if (gatewayHost !== endpoint.host) {
    throw new ConfigError(
      `--gateway-url names host "${gatewayHost}" while --api-url names "${endpoint.host}"; a server ` +
        'endpoint has one host, so this run cannot dial a gateway on a different one — point both ' +
        'flags at the same node',
    );
  }
  if (gateway.pathname !== '/' && gateway.pathname !== '/ws') {
    throw new ConfigError(
      `--gateway-url has path "${gateway.pathname}"; the SDK dials the gateway at "/ws" and does ` +
        'not let callers choose the path, so this URL would be silently rewritten to the one the ' +
        'server answers',
    );
  }
  const scheme: WsScheme = gateway.protocol === 'wss:' ? 'Wss' : 'Ws';
  const portText = gateway.port;
  const gatewayPort =
    portText === '' ? (scheme === 'Wss' ? 443 : 80) : Number.parseInt(portText, 10);
  return { ...endpoint, gatewayPort, scheme };
}

function parsePositiveInt(name: string, raw: string | undefined, fallback: number): number {
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 1) {
    throw new ConfigError(`--${name} must be a positive integer, got "${raw}"`);
  }
  return value;
}

function parseNonNegative(name: string, raw: string | undefined, fallback: number): number {
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isFinite(value) || value < 0) {
    throw new ConfigError(`--${name} must be a non-negative number, got "${raw}"`);
  }
  return value;
}

function parseFraction(name: string, raw: string | undefined, fallback: number): number {
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isFinite(value) || value < 0 || value > 1) {
    throw new ConfigError(`--${name} must be between 0 and 1, got "${raw}"`);
  }
  return value;
}

function parseDuration(name: string, raw: string | undefined, fallback: number): number {
  if (raw === undefined) return fallback;
  const match = /^(\d+)(ms|s|m)?$/.exec(raw.trim());
  if (match === null) {
    throw new ConfigError(
      `--${name} must look like 30s, 2m, 500ms, or a whole number of seconds, got "${raw}"`,
    );
  }
  const value = Number(match[1] ?? '0');
  switch (match[2] ?? 's') {
    case 'ms':
      return value;
    case 'm':
      return value * 60_000;
    default:
      return value * 1_000;
  }
}

function parseOutput(raw: string | undefined): 'text' | 'json' {
  if (raw === undefined || raw === 'text') return 'text';
  if (raw === 'json') return 'json';
  throw new ConfigError(`--output must be "text" or "json", got "${raw}"`);
}

const HELP = `migo-loadgen — load generator for the Migo server

Drives many virtual clients through the real @migo/sdk path (REST register, gateway
handshake, end-to-end encrypted sends) and reports throughput, latency percentiles,
errors by class, and the gateway wire bytes each session spent.

USAGE
  migo-loadgen [options]

SCENARIOS (--scenario)
  messaging   pairs of clients hold a direct E2E conversation; senders stream sealed
              messages at the target rate (default)
  presence    every client flips presence Online/Away at the target rate
  connect     register and hold N concurrent gateway sessions for the duration
  fanout      one group conversation at the product member ceiling (256); one sender,
              every member measures send-to-deliver latency
  calls       pairs drive the full 1:1 call signaling lifecycle — invite, answer, SDP
              and ICE relays, end — with sealed placeholder offers
  voice-notes every client uploads valid WAV voice notes through the full
              ticket/PUT/commit lifecycle, closed-loop
  outage      pairs stream through the offline outbox while the runner restarts the
              node mid-run; the settle phase demands full delivery, no duplicates,
              and every session resumed (see tools/load/run-full.sh for the restart)

OPTIONS
  --scenario <name>          workload to run (default: messaging)
  --vus <n>                  number of virtual users (default: 10)
  --duration <t>             run length: 30s, 2m, 500ms, or seconds (default: 30s)
  --rate <n>                 per-VU operations per second; 0 = as fast as possible (default: 5)
  --connect-concurrency <n>  max simultaneous registrations while ramping up (default: 20)
  --api-url <url>            server REST base (default: $MIGO_API_URL or http://localhost:8080)
  --gateway-url <url>        realtime gateway the sessions dial; must name the same host as
                             --api-url and the path /ws (default: --api-url's port, /ws)
  --app-version <v>          version presented in the client hello (default: 0.1.0)
  --locale <l>               account locale (default: en-US)
  --country <c>              account country (default: ID)
  --prefix <s>               username prefix for generated accounts (default: loadgen)
  --passphrase <s>             passphrase for generated accounts (default: generated per run)
  --request-timeout-ms <n>   per-request timeout in milliseconds (default: 15000)
  --max-error-rate <f>       fail (exit 3) if the error fraction exceeds this 0..1 (default: 1)
  --output <text|json>       report format on stdout (default: text)
  --verbose                  per-VU diagnostics on stderr
  --quiet                    suppress progress; warnings and errors still show
  --help                     print this help and exit

EXIT CODES
  0 success   1 fatal error   2 bad usage   3 error-rate threshold exceeded
  4 byte budget exceeded (a scenario spent more than its per-user per-minute wire-byte
    budget plus the 10 percent headroom of brief section 171)

The generator registers fresh throwaway accounts, so the target server must allow
registration. It never reads or writes real user data.`;

export function helpText(): string {
  return HELP;
}
