/**
 * The orchestrator: turns a {@link Config} into a finished {@link RunOutcome}.
 *
 * The shape of a run is always the same. Preflight the server's /v1/config (so we fail fast on a
 * server that forbids the registration this tool depends on). Build the VUs. Connect them under a
 * concurrency cap — an open-ended phase, timed but not deadlined. Let the scenario wire up shared
 * state. Then set the deadline and race every workload against it, holding the whole duration even
 * when a scenario has no steady-state op. Finally disconnect and hand back the metrics. Ctrl-C at any
 * point flips the run to a graceful stop and still reports what was gathered.
 */

import type { Config } from './config.js';
import type { Logger } from './logger.js';
import { runPool } from './pool.js';
import type { RunOutcome } from './report.js';
import { RunContext, sleep } from './run-context.js';
import { getScenario } from './scenarios.js';
import type { Workload } from './scenarios.js';
import { classifyError, Metrics } from './stats.js';
import { VirtualUser } from './virtual-user.js';

/** How often the steady-state hold wakes to notice the deadline or an interrupt. */
const HOLD_POLL_MS = 200;

interface ServerConfig {
  readonly allowRegistration: boolean | undefined;
  readonly passphraseMinLength: number | undefined;
}

export async function run(config: Config, log: Logger): Promise<RunOutcome> {
  const scenario = getScenario(config.scenario);
  if (scenario === undefined) throw new Error(`unknown scenario "${config.scenario}"`);
  if (config.vus < scenario.minVus) {
    throw new Error(`scenario "${scenario.name}" needs at least ${scenario.minVus} virtual users`);
  }

  const server = await preflight(config, log);
  if (server.allowRegistration === false) {
    throw new Error(
      'the target server has registration disabled (allow_registration=false); the load generator ' +
        'registers throwaway accounts and cannot run against it',
    );
  }

  const metrics = new Metrics();
  const passphrase = buildPassphrase(config, server.passphraseMinLength);
  const runTag = makeRunTag();
  const onEventError = (error: unknown): void => metrics.recordError('event', classifyError(error));

  log.info(`building ${config.vus} virtual users for scenario "${scenario.name}"`);
  const vus = Array.from(
    { length: config.vus },
    (_unused, index) => new VirtualUser(index, { config, passphrase, runTag, onEventError }),
  );

  // Open-ended deadline for the connect phase; the real one is set just before steady state.
  const ctx = new RunContext(metrics, log, config.ratePerSec, Number.MAX_SAFE_INTEGER);
  const releaseSignals = installSignalHandlers(ctx, log);

  try {
    log.info(`connecting with concurrency ${config.connectConcurrency}...`);
    await runPool(vus, config.connectConcurrency, async (vu) => {
      if (ctx.interrupted) return;
      const started = performance.now();
      try {
        await vu.start();
        metrics.latency('connect').record(performance.now() - started);
        metrics.recordOk('connect');
        log.debug(`VU ${vu.index} connected`);
      } catch (error) {
        metrics.recordError('connect', classifyError(error));
        log.debug(`VU ${vu.index} failed to connect: ${describe(error)}`);
      }
    });
    const connectedCount = vus.filter((vu) => vu.connected).length;
    log.info(`connected ${connectedCount}/${config.vus}`);

    let durationMsActual = 0;
    if (connectedCount > 0 && !ctx.interrupted) {
      log.info('preparing scenario...');
      await scenario.prepare(vus, ctx);

      const workloads = scenario.workloads(vus);
      const startedAt = performance.now();
      ctx.setDeadline(startedAt + config.durationMs);
      log.info(
        `running for ${(config.durationMs / 1000).toFixed(0)}s across ${workloads.length} active workload(s)...`,
      );
      await Promise.all([
        holdUntilDeadline(ctx),
        ...workloads.map((workload) => driveSafely(workload, ctx)),
      ]);
      durationMsActual = performance.now() - startedAt;
    } else if (connectedCount === 0) {
      log.warn('no virtual users connected; skipping the workload');
    }

    log.info('disconnecting...');
    await teardown(vus, config);

    return {
      config,
      scenarioName: scenario.name,
      requestedVus: config.vus,
      connectedCount,
      durationMsActual,
      interrupted: ctx.interrupted,
      metrics,
    };
  } finally {
    releaseSignals();
  }
}

/** Read the server's public config so we can fail fast and size the passphrase. Never fatal on its own. */
async function preflight(config: Config, log: Logger): Promise<ServerConfig> {
  const url = `${trimTrailingSlash(config.apiUrl)}/v1/config`;
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(config.requestTimeoutMs) });
    if (!response.ok) {
      log.warn(`GET /v1/config returned HTTP ${response.status}; proceeding with defaults`);
      return { allowRegistration: undefined, passphraseMinLength: undefined };
    }
    const body: unknown = await response.json();
    return {
      allowRegistration: readBoolean(body, 'allow_registration'),
      passphraseMinLength: readNumber(body, 'passphrase_min_length'),
    };
  } catch (error) {
    log.warn(`could not read ${url}: ${describe(error)}; proceeding with defaults`);
    return { allowRegistration: undefined, passphraseMinLength: undefined };
  }
}

/** Run one workload to completion, folding any escape into a tallied 'workload' error. */
function driveSafely(workload: Workload, ctx: RunContext): Promise<void> {
  return workload(ctx).catch((error: unknown) => {
    ctx.metrics.recordError('workload', classifyError(error));
  });
}

/** Hold the run open until its deadline, even when the scenario has no steady-state loop. */
async function holdUntilDeadline(ctx: RunContext): Promise<void> {
  while (!ctx.deadlineReached()) await sleep(HOLD_POLL_MS);
}

async function teardown(vus: readonly VirtualUser[], config: Config): Promise<void> {
  const connected = vus.filter((vu) => vu.connected);
  await runPool(connected, Math.max(1, config.connectConcurrency), (vu) => vu.stop());
}

function installSignalHandlers(ctx: RunContext, log: Logger): () => void {
  const handler = (signal: NodeJS.Signals): void => {
    log.warn(`received ${signal}; stopping and reporting partial results`);
    ctx.interrupt();
  };
  process.on('SIGINT', handler);
  process.on('SIGTERM', handler);
  return () => {
    process.off('SIGINT', handler);
    process.off('SIGTERM', handler);
  };
}

/** A dev-only throwaway passphrase long enough for typical policy, with all four character classes. */
function buildPassphrase(config: Config, minLength: number | undefined): string {
  if (config.passphrase !== undefined) return config.passphrase;
  const target = Math.max(minLength ?? 8, 16);
  const seed = 'Loadgen!aA1';
  let passphrase = seed;
  while (passphrase.length < target) passphrase += seed;
  return passphrase.slice(0, target);
}

/** A short per-run tag so usernames from repeated runs never collide on a taken name. */
function makeRunTag(): string {
  const time = Date.now().toString(36);
  const random = Math.floor(Math.random() * 36 ** 4)
    .toString(36)
    .padStart(4, '0');
  return `${time}${random}`;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null
    ? (value as Record<string, unknown>)
    : undefined;
}

function readBoolean(value: unknown, key: string): boolean | undefined {
  const field = asRecord(value)?.[key];
  return typeof field === 'boolean' ? field : undefined;
}

function readNumber(value: unknown, key: string): number | undefined {
  const field = asRecord(value)?.[key];
  return typeof field === 'number' ? field : undefined;
}

function describe(error: unknown): string {
  if (error instanceof Error) return error.message !== '' ? error.message : error.name;
  return String(error);
}

function trimTrailingSlash(value: string): string {
  return value.endsWith('/') ? value.slice(0, -1) : value;
}
