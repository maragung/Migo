#!/usr/bin/env node
/**
 * Entry point.
 *
 * stdout carries exactly one thing — the report (text or JSON) — so the tool pipes cleanly. Progress
 * and diagnostics go to stderr via the {@link Logger}. The exit code is the machine-readable verdict:
 * 0 success, 1 a fatal error (or nothing connected), 2 bad usage, 3 the error budget was exceeded,
 * 4 the scenario's wire-byte budget was exceeded by more than the §171 headroom.
 *
 * A run also holds one ref'd timer for its whole life (see {@link KEEP_ALIVE_MS}). Without it a run
 * has a fifth ending that is not a verdict at all: Node's built-in WebSocket does not on its own keep
 * the event loop alive, and a load run holds almost nothing else once its REST bootstrap is done — so
 * the loop can drain in the middle of an `await`, Node leaves with the default exit code of 0, and a
 * harness reading only the exit status records a pass for a run that never happened. That is not
 * hypothetical: the `connect` scenario reported exit 0 in a quarter of a second against a thirty
 * second hold, having written no report, for as long as the gate has existed.
 */

import { writeSync } from 'node:fs';

import { helpText, parseArgs } from './config.js';
import type { ParseResult } from './config.js';
import { Logger } from './logger.js';
import { isOk, isWithinByteBudget, renderJson, renderText } from './report.js';
import { run } from './runner.js';
import { scenarioNames } from './scenarios.js';

/**
 * How often the run's keep-alive timer wakes.
 *
 * The interval is the point, not the tick: an outstanding ref'd timer is what stops the loop draining
 * out from under a pending `await`. One second is slow enough to cost nothing over a run of minutes
 * and fast enough that a run which genuinely hangs is noticed by whatever bounds it — the job's own
 * `timeout-minutes` — rather than being mistaken for a finish.
 */
const KEEP_ALIVE_MS = 1_000;

/** Set once a verdict exists, so the exit hook can tell a finish from a drained loop. */
let settled = false;

/**
 * The last word, when the loop drains before the run does.
 *
 * The write is synchronous and unconditional because this path is reached exactly when nothing
 * asynchronous can be trusted: a buffered stream write needs the very event loop that just emptied.
 * `writeSync(2, …)` cannot itself be abandoned, and naming the still-active resources turns "it
 * exited without saying anything" into a sentence someone can act on.
 */
function reportUnsettledExit(code: number): void {
  if (settled) return;
  writeSync(
    2,
    `fatal: the event loop drained before the run finished (exit ${code}); still-active resources: ` +
      `${JSON.stringify(process.getActiveResourcesInfo())}\n`,
  );
  process.exitCode = 1;
}

async function runMain(): Promise<number> {
  let parsed: ParseResult;
  try {
    parsed = parseArgs(process.argv.slice(2), process.env);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`error: ${message}\n\n${helpText()}\n`);
    return 2;
  }

  if (parsed.help) {
    process.stdout.write(`${helpText()}\n`);
    return 0;
  }

  const config = parsed.config;
  const log = new Logger(config.logLevel);

  if (!scenarioNames().includes(config.scenario)) {
    log.error(
      `unknown scenario "${config.scenario}"; choose one of: ${scenarioNames().join(', ')}`,
    );
    return 2;
  }

  try {
    const outcome = await run(config, log);
    const report = config.output === 'json' ? renderJson(outcome) : renderText(outcome);
    process.stdout.write(`${report}\n`);
    if (outcome.connectedCount === 0) return 1;
    if (!isOk(outcome)) return 3;
    // The error budget keeps precedence so an existing gate's exit code never changes meaning;
    // the report names both verdicts either way.
    if (!isWithinByteBudget(outcome)) return 4;
    return 0;
  } catch (error) {
    log.error(error instanceof Error ? error.message : String(error));
    return 1;
  }
}

async function main(): Promise<number> {
  // Held across the report write as well as the run: the report is the only thing stdout carries,
  // and it must not be the last write standing on an empty loop.
  const keepAlive = setInterval(() => {}, KEEP_ALIVE_MS);
  try {
    return await runMain();
  } finally {
    clearInterval(keepAlive);
  }
}

process.on('exit', reportUnsettledExit);

main()
  .then((code) => {
    settled = true;
    process.exitCode = code;
  })
  .catch((error: unknown) => {
    settled = true;
    process.stderr.write(`fatal: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
