#!/usr/bin/env node
/**
 * Entry point.
 *
 * stdout carries exactly one thing — the report (text or JSON) — so the tool pipes cleanly. Progress
 * and diagnostics go to stderr via the {@link Logger}. The exit code is the machine-readable verdict:
 * 0 success, 1 a fatal error (or nothing connected), 2 bad usage, 3 the error budget was exceeded,
 * 4 the scenario's wire-byte budget was exceeded by more than the §171 headroom.
 */

import { helpText, parseArgs } from './config.js';
import type { ParseResult } from './config.js';
import { Logger } from './logger.js';
import { isOk, isWithinByteBudget, renderJson, renderText } from './report.js';
import { run } from './runner.js';
import { scenarioNames } from './scenarios.js';

async function main(): Promise<number> {
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

main()
  .then((code) => {
    process.exitCode = code;
  })
  .catch((error: unknown) => {
    process.stderr.write(`fatal: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
