/**
 * A tiny leveled logger that writes to stderr.
 *
 * stdout is reserved for the run report (text or JSON) so the tool composes in a pipeline —
 * `migo-loadgen --output json > run.json` captures a clean document while progress still shows on
 * the terminal. Everything here is diagnostic and therefore goes to stderr. Warnings and errors
 * ignore the level: an operator running under `--quiet` still needs to see that something is wrong.
 */

export type LogLevel = 'quiet' | 'normal' | 'verbose';

const RANK: Record<LogLevel, number> = { quiet: 0, normal: 1, verbose: 2 };

export class Logger {
  readonly #level: LogLevel;

  constructor(level: LogLevel) {
    this.#level = level;
  }

  /** High-level phase progress. Shown at `normal` and `verbose`. */
  info(message: string): void {
    if (RANK[this.#level] >= RANK.normal) process.stderr.write(`${message}\n`);
  }

  /** Per-VU and per-transition detail. Shown only at `verbose`. */
  debug(message: string): void {
    if (RANK[this.#level] >= RANK.verbose) process.stderr.write(`  ${message}\n`);
  }

  /** Something the operator must not miss. Always shown, even under `--quiet`. */
  warn(message: string): void {
    process.stderr.write(`warning: ${message}\n`);
  }

  /** A failure. Always shown. */
  error(message: string): void {
    process.stderr.write(`error: ${message}\n`);
  }
}
