#!/usr/bin/env node
/**
 * Entry point.
 *
 * stdout carries exactly one thing — the report (text or JSON) — so the tool pipes cleanly. Progress
 * and diagnostics go to stderr via the {@link Logger}. The exit code is the machine-readable verdict:
 * 0 success, 1 a fatal error (or nothing connected), 2 bad usage, 3 the error budget was exceeded,
 * 4 the scenario's wire-byte budget was exceeded by more than the §171 headroom, 5 the run never
 * finished — see below.
 *
 * # Why 5 exists
 *
 * `main()` sets `process.exitCode` and returns; it never calls `process.exit()`. So the process ends
 * when the event loop empties, and if the run is still awaiting something when that happens, the
 * code that would have been set is never set: Node exits 0, having written no report at all. A
 * harness that trusts the exit code reads that as a pass, and a harness that reads the report reads
 * an empty file as one too. The `beforeExit` hook below is the answer — it fires exactly when the
 * loop drains, which is precisely the moment an unfinished run has to admit it, and it turns a bare
 * zero into code 5 plus a stderr sentence naming the phase that never settled. It deliberately does
 * *not* hold the process open: a keep-alive would hide the stall behind an infinite wait instead,
 * and a wait has to be timed out before anybody learns anything.
 */

import { helpText, parseArgs } from './config.js';
import type { ParseResult } from './config.js';
import { Logger } from './logger.js';
import { PhaseTracker, stallMessage } from './phase.js';
import { isOk, isWithinByteBudget, renderJson, renderText } from './report.js';
import { run } from './runner.js';
import { scenarioNames } from './scenarios.js';

/** The run never finished: the event loop drained with a phase still in flight, so nothing was measured. */
const EXIT_STALLED = 5;

async function main(tracker: PhaseTracker): Promise<number> {
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
    const outcome = await run(config, log, tracker);
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

const tracker = new PhaseTracker();
let finished = false;

// The last word on every run. `beforeExit` fires when the loop has nothing left to do and Node is
// about to end the process; the promise chain above has had its microtasks by then, so a `finished`
// flag still false means `main` never returned — the run is stuck on an await that will not settle.
// Guarded against a second firing (writing to a piped stderr can itself schedule work), and it
// never schedules anything: the process ends here, with a code that says so.
process.on('beforeExit', () => {
  if (finished) return;
  finished = true;
  process.stderr.write(`${stallMessage(tracker.phase)}\n`);
  process.exitCode = EXIT_STALLED;
});

main(tracker)
  .then((code) => {
    finished = true;
    process.exitCode = code;
  })
  .catch((error: unknown) => {
    finished = true;
    process.stderr.write(`fatal: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
