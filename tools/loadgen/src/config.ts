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
  readonly password: string | undefined;
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
    password: values.get('password'),
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
and errors by class.

USAGE
  migo-loadgen [options]

SCENARIOS (--scenario)
  messaging   pairs of clients hold a direct E2E conversation; senders stream sealed
              messages at the target rate (default)
  presence    every client flips presence Online/Away at the target rate
  connect     register and hold N concurrent gateway sessions for the duration

OPTIONS
  --scenario <name>          workload to run (default: messaging)
  --vus <n>                  number of virtual users (default: 10)
  --duration <t>             run length: 30s, 2m, 500ms, or seconds (default: 30s)
  --rate <n>                 per-VU operations per second; 0 = as fast as possible (default: 5)
  --connect-concurrency <n>  max simultaneous registrations while ramping up (default: 20)
  --api-url <url>            server REST base (default: $MIGO_API_URL or http://localhost:8080)
  --gateway-url <url>        realtime gateway (default: derived from --api-url, path /ws)
  --app-version <v>          version presented in the client hello (default: 0.1.0)
  --locale <l>               account locale (default: en-US)
  --country <c>              account country (default: ID)
  --prefix <s>               username prefix for generated accounts (default: loadgen)
  --password <s>             password for generated accounts (default: generated per run)
  --request-timeout-ms <n>   per-request timeout in milliseconds (default: 15000)
  --max-error-rate <f>       fail (exit 3) if the error fraction exceeds this 0..1 (default: 1)
  --output <text|json>       report format on stdout (default: text)
  --verbose                  per-VU diagnostics on stderr
  --quiet                    suppress progress; warnings and errors still show
  --help                     print this help and exit

EXIT CODES
  0 success   1 fatal error   2 bad usage   3 error-rate threshold exceeded

The generator registers fresh throwaway accounts, so the target server must allow
registration. It never reads or writes real user data.`;

export function helpText(): string {
  return HELP;
}
