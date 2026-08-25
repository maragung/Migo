/**
 * Public surface of the load generator, for embedding it in a larger harness (a test suite that
 * asserts on percentiles, a CI gate that fails on error rate) rather than shelling out to the CLI.
 * The CLI in `main.ts` is a thin wrapper over exactly these pieces.
 */

export { parseArgs, helpText, ConfigError } from './config.js';
export type { Config, ParseResult } from './config.js';
export { Logger } from './logger.js';
export type { LogLevel } from './logger.js';
export { run } from './runner.js';
export { renderText, renderJson, isOk, computeErrorRate } from './report.js';
export type { RunOutcome } from './report.js';
export { getScenario, scenarioNames } from './scenarios.js';
export type { Scenario, Workload } from './scenarios.js';
export { VirtualUser } from './virtual-user.js';
export type { VirtualUserDeps } from './virtual-user.js';
export { RunContext, sleep } from './run-context.js';
export { runPool } from './pool.js';
export { Metrics, LatencyDigest, classifyError } from './stats.js';
export type { DigestSnapshot, OperationSnapshot } from './stats.js';
