#!/usr/bin/env node
/**
 * Reads a loadgen run's own report back and refuses one that did not happen.
 *
 * `run_step` used to decide a step's verdict from its exit status alone, and an exit status is not a
 * verdict: loadgen leaves with 0 both when a run finished and when its event loop drained mid-`await`
 * and Node exited with the default code, having written nothing to the report file at all. The second
 * ending was invisible for as long as the harness has existed — every step "passed", steps that
 * declared ninety to a hundred and twenty seconds finished in one to seventeen, and the report files
 * were empty — so the full-scale job was measuring nothing and reporting success for it.
 *
 * Three questions the exit status cannot answer, asked of the document the run wrote:
 *
 *   - did it finish, rather than merely stop (an empty or unparseable report, or `interrupted`)?
 *   - did it hold the duration it was asked for (a run far shorter than `--duration`)?
 *   - did the sessions it claims actually open (fewer connected than `--vus` asked for)?
 *
 * Usage: node check-report.mjs <report.json> [--expect-vus N] [--requested-ms N]
 * Exits 0 when the run is real, 1 (with a reason on stderr) when it is not.
 */

import { readFileSync } from 'node:fs';

/** How much of the requested duration a run must hold before it counts as having held it. */
const DURATION_FLOOR = 0.8;

function parseArgs(argv) {
  const options = { path: '', expectVus: 0, requestedMs: 0 };
  const rest = [];
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--expect-vus') {
      index += 1;
      options.expectVus = Number(argv[index] ?? '0');
    } else if (arg === '--requested-ms') {
      index += 1;
      options.requestedMs = Number(argv[index] ?? '0');
    } else {
      rest.push(arg);
    }
  }
  options.path = rest[0] ?? '';
  return options;
}

function seconds(ms) {
  return `${(ms / 1000).toFixed(1)}s`;
}

function main() {
  const { path, expectVus, requestedMs } = parseArgs(process.argv.slice(2));
  const fail = (reason) => {
    process.stderr.write(`  !! ${reason}\n`);
    return 1;
  };

  if (path === '') return fail('no report path was given');

  let raw = '';
  try {
    raw = readFileSync(path, 'utf8').trim();
  } catch (error) {
    return fail(`the report at ${path} could not be read: ${error.message}`);
  }
  if (raw === '') {
    return fail('loadgen wrote no report; the run ended without finishing and cannot be trusted');
  }

  let doc;
  try {
    doc = JSON.parse(raw);
  } catch (error) {
    return fail(`the report is not JSON: ${error.message}`);
  }

  if (doc.interrupted === true) return fail('the run was interrupted before it finished');

  if (expectVus > 0 && doc.connectedCount < expectVus) {
    return fail(`only ${doc.connectedCount} of the ${expectVus} sessions asked for connected`);
  }

  if (requestedMs > 0 && doc.durationMs < requestedMs * DURATION_FLOOR) {
    return fail(
      `the run lasted ${seconds(doc.durationMs)} of the ${seconds(requestedMs)} it was asked for`,
    );
  }

  return 0;
}

process.exitCode = main();
