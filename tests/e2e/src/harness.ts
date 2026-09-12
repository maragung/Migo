/**
 * The node lifecycle for the end-to-end suite.
 *
 * One migod, started exactly the way `tools/2node/run.sh` starts its two: a per-run TOML
 * config handed over through `MIGO_CONFIG`, an explicit HTTP port, a fresh PostgreSQL
 * database, a shared Redis, a filesystem media directory, and a health wait that fails
 * with the log tail rather than with a timeout nobody can read. The pattern is reused
 * rather than reinvented because it is the one shape that has already stood a node up on
 * developer machines and in the smoke script; what this harness adds on top is the
 * {@link NodeHarness.restart} the durability scenario needs, and teardown that holds even
 * when a test has already failed.
 *
 * Nothing here mocks the seam under test. The process is the real `migod` binary, the
 * socket a client opens is a real WebSocket, the storage is a real PostgreSQL database
 * created for this run and dropped when it ends, and the media bytes land in a real
 * directory the node's HTTP listener serves them back out of. A suite that substitutes
 * any of those with a fake is a unit test wearing the name "end-to-end", and brief
 * section 177 was explicit that this directory exists to be the thing that does not.
 *
 * Port policy: the suite binds one port, on loopback only, defaulting to 29180. The
 * ports 8080, 18081, 18443 and 19992 belong to the production node that runs on the
 * machine this repository is developed on, and a test run that took any of them would
 * be a test run that broke production; the default is far outside that set, and the
 * pre-start bind check below refuses to continue when something else already holds the
 * port — without it, the health wait could pass against a stranger's server and the
 * whole suite would green-light the wrong binary.
 */

import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  closeSync,
  mkdirSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
  writeSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

/** How long the node has to answer /health before the run gives up and shows the log. */
const HEALTH_TIMEOUT_MS = 30_000;
/** How long a SIGTERM'd migod has to exit on its own before the harness escalates. */
const SHUTDOWN_GRACE_MS = 15_000;

/**
 * How much of the node's stderr the harness keeps in memory for failure messages.
 *
 * Enough for an anyhow error chain with its causes; the log file keeps everything,
 * so the buffer only has to hold what a person needs to see first.
 */
const STDERR_TAIL_BYTES = 16 * 1024;

/** The one port this suite binds. See the module doc for why this number. */
const DEFAULT_HTTP_PORT = 29180;

function requireEnv(name: string): string {
  const value = process.env[name];
  if (value === undefined || value === '') {
    throw new Error(
      `the end-to-end suite needs ${name}; it is a gate, so it fails rather than skips. ` +
        'CI provides it from the service containers; locally, run `make infra-up` and export ' +
        'MIGO_TEST_DATABASE_URL and MIGO_TEST_REDIS_URL the way tools/2node/README.md describes.',
    );
  }
  return value;
}

/** The parsed shape of a PostgreSQL connection URL: credentials, host, port, database. */
interface PostgresTarget {
  host: string;
  port: string;
  user: string;
  password: string;
  adminDatabase: string;
}

function parsePostgresUrl(url: string): PostgresTarget {
  const parsed = new URL(url);
  const database = parsed.pathname.replace(/^\//, '');
  return {
    host: parsed.hostname,
    port: parsed.port === '' ? '5432' : parsed.port,
    user: parsed.username === '' ? 'migo' : decodeURIComponent(parsed.username),
    password: decodeURIComponent(parsed.password),
    adminDatabase: database === '' ? 'postgres' : database,
  };
}

/** Runs one SQL statement as the superuser of the test cluster. Fails loudly. */
function psql(target: PostgresTarget, sql: string): void {
  const result = spawnSync(
    'psql',
    [
      '-h',
      target.host,
      '-p',
      target.port,
      '-U',
      target.user,
      '-d',
      target.adminDatabase,
      '-v',
      'ON_ERROR_STOP=1',
      '-c',
      sql,
    ],
    { env: { ...process.env, PGPASSWORD: target.password }, encoding: 'utf8' },
  );
  if (result.error !== undefined) {
    throw new Error(`psql could not be run (${result.error.message}); the suite needs the client`);
  }
  if (result.status !== 0) {
    throw new Error(`psql failed while running: ${sql}\n${result.stderr}`);
  }
}

/**
 * Resolves when the port is free to bind, rejects when something already holds it.
 *
 * Listens and closes immediately: a port nobody can bind is a port no stale server is
 * hiding on, and the tiny window is safe because the only other user of the port would
 * be the migod this harness starts a moment later.
 */
function checkPortFree(port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.once('error', (error: NodeJS.ErrnoException) => {
      if (error.code === 'EADDRINUSE') {
        reject(
          new Error(
            `port ${port} is already in use; the suite refuses to share a port with ` +
              'another server, because a green run against the wrong process proves nothing',
          ),
        );
        return;
      }
      reject(error);
    });
    probe.listen(port, '127.0.0.1', () => probe.close(() => resolve()));
  });
}

/** Resolves when the child exits, or `false` after `graceMs` of it still running. */
function waitForExit(child: ChildProcess, graceMs: number): Promise<boolean> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve(true);
  }
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(false), graceMs);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve(true);
    });
  });
}

/**
 * One migod, its database, its media directory, and its logs.
 *
 * Created through {@link NodeHarness.create}, which performs every side effect that must
 * happen exactly once per run (the run directory, the database, the config file, the
 * last-resort kill hook), so that {@link start} and {@link restart} stay symmetric: both
 * spawn the same binary with the same environment, which is the property the restart
 * scenario's claim depends on.
 */
export class NodeHarness {
  /** The node identity, unique per run so the shared Redis namespaces never collide. */
  readonly nodeId: string;
  /** The one HTTP port: REST, gateway WebSocket, and media all ride it. */
  readonly httpPort: number;
  /** The REST origin, as a client would configure it. */
  readonly apiUrl: string;

  readonly #migodBin: string;
  readonly #runDir: string;
  readonly #logPath: string;
  readonly #configPath: string;
  readonly #databaseName: string;
  readonly #target: PostgresTarget;
  readonly #env: NodeJS.ProcessEnv;
  #child: ChildProcess | null = null;
  #logFd: number | null = null;
  #spawnError: Error | null = null;
  #stderrTail = '';

  private constructor(options: {
    nodeId: string;
    httpPort: number;
    migodBin: string;
    runDir: string;
    databaseName: string;
    storeUrl: string;
    target: PostgresTarget;
  }) {
    this.nodeId = options.nodeId;
    this.httpPort = options.httpPort;
    this.apiUrl = `http://127.0.0.1:${options.httpPort}`;
    this.#migodBin = options.migodBin;
    this.#runDir = options.runDir;
    this.#logPath = join(options.runDir, 'migod.log');
    this.#configPath = join(options.runDir, 'migod.toml');
    this.#databaseName = options.databaseName;
    this.#target = options.target;
    this.#env = {
      ...this.#inheritedEnvWithoutMigoKeys(),
      MIGO_CONFIG: this.#configPath,
      MIGO_NODE__ID: options.nodeId,
      MIGO_NODE__REGION: 'e2e',
      MIGO_NODE__COUNTRY: 'ID',
      MIGO_NODE__ROLES: 'api,gateway,room,game',
      MIGO_NODE__ENVIRONMENT: 'development',
      MIGO_HTTP__BIND: `127.0.0.1:${options.httpPort}`,
      MIGO_HTTP__PUBLIC_URL: this.apiUrl,
      MIGO_STORE__BACKEND: 'postgres',
      MIGO_STORE__URL: options.storeUrl,
      MIGO_CACHE__BACKEND: 'redis',
      MIGO_CACHE__URL: requireEnv('MIGO_TEST_REDIS_URL'),
      MIGO_MEDIA__BACKEND: 'filesystem',
      MIGO_MEDIA__LOCAL_DIR: join(options.runDir, 'media'),
      MIGO_AUTH__TOKEN_KEY: 'development-only-insecure-token-key',
      MIGO_AUTH__ALLOW_REGISTRATION: 'true',
      RUST_LOG: 'info',
    };

    // The last-resort kill: if the test process dies without reaching destroy() — an
    // uncaught error, a SIGKILL from the CI runner's timeout — the migod would otherwise
    // outlive the run and hold the port. Registered once, self-removing, and best-effort,
    // because at 'exit' there is no async left to wait on.
    const kill = (): void => {
      this.#child?.kill('SIGKILL');
    };
    process.once('exit', kill);
  }

  /**
   * The process environment minus every `MIGO_*` key, for handing to the node.
   *
   * migod's `Config::load()` does not pick variables by name: it reads the whole
   * environment and turns every `MIGO_*` key into a config path (`MIGO_SECTION__FIELD`).
   * CI hands this suite `MIGO_TEST_DATABASE_URL`, `MIGO_TEST_REDIS_URL`, and
   * `MIGO_TEST_REQUIRE_BACKENDS`, which that grammar reads as a `test.database.url`
   * table the schema does not have — and every config section is
   * `deny_unknown_fields`, so the process exits 1 before it logs anything else. The
   * same trap catches any future `MIGO_TEST_*` or `MIGO_WHATEVER` the runner grows,
   * so nothing with the prefix is inherited at all: the node's own configuration is
   * set explicitly by the assignment that makes this call, and ambient `MIGO_` keys
   * are never configuration.
   */
  #inheritedEnvWithoutMigoKeys(): NodeJS.ProcessEnv {
    const inherited: NodeJS.ProcessEnv = {};
    for (const [key, value] of Object.entries(process.env)) {
      if (key.startsWith('MIGO_')) {
        continue;
      }
      inherited[key] = value;
    }
    return inherited;
  }

  /**
   * Prepares everything a run needs: the run directory, the database, and the config.
   *
   * The database is created empty and unique per run (migod migrates it on startup), and
   * dropped again in {@link destroy} with `WITH (FORCE)` so a teardown that races the
   * node's own last connections still completes instead of hanging on "database is being
   * accessed by other users".
   */
  static create(): NodeHarness {
    const databaseUrl = requireEnv('MIGO_TEST_DATABASE_URL');
    const target = parsePostgresUrl(databaseUrl);
    const runId = `${Date.now().toString(36)}${randomBytes(3).toString('hex')}`;
    const migodBin =
      process.env.MIGOD_BIN ??
      join(import.meta.dirname, '..', '..', '..', 'server', 'target', 'debug', 'migod');
    const httpPort = Number.parseInt(process.env.E2E_HTTP_PORT ?? '', 10) || DEFAULT_HTTP_PORT;

    const runDir = join(tmpdir(), `migo-e2e-${runId}`);
    mkdirSync(join(runDir, 'media'), { recursive: true });

    // A fresh database per run, so every scenario starts from zero state without the
    // suite ever sharing rows with a previous (or concurrent) run.
    const databaseName = `migo_e2e_${runId}`;
    psql(target, `CREATE DATABASE "${databaseName}"`);

    const storeUrl = new URL(databaseUrl);
    storeUrl.pathname = `/${databaseName}`;

    // The per-run config file, same shape and same reason as tools/2node's: generous
    // rate limits and a one-token registration cost, because the production defaults
    // are tuned for the public internet and would lock a localhost suite out after its
    // first request. Everything else stays at the development defaults.
    //
    // The signing key is the one non-default a restart demands. Without it the node
    // derives an ephemeral secret, warns that tokens will not survive a restart, and
    // means it: after a restart every grant ever issued is unsigned-by-the-new-key and
    // the resume path the durability scenario asserts cannot exist. Production
    // configures a real key; this run generates its own — random per run, so a key
    // never leaves the run directory it was born in, but stable across the restart,
    // which is the property under test.
    const config = [
      '[node]',
      `signing_key = "${randomBytes(32).toString('base64')}"`,
      '',
      '[rate_limit]',
      'user_burst = 1000',
      'user_refill_per_second = 500',
      'anonymous_burst = 1000',
      'anonymous_refill_per_second = 500',
      'bot_burst = 1000',
      'bot_refill_per_second = 500',
      '',
      '[auth]',
      'registration_cost = 1',
      '',
    ].join('\n');
    const configPath = join(runDir, 'migod.toml');
    writeFileSync(configPath, config);

    const harness = new NodeHarness({
      nodeId: `e2e-node-${runId}`,
      httpPort,
      migodBin,
      runDir,
      databaseName,
      storeUrl: storeUrl.toString(),
      target,
    });
    writeFileSync(harness.#logPath, '');
    return harness;
  }

  /** Spawns the node and resolves once /health answers. */
  async start(): Promise<void> {
    if (this.#child !== null) {
      return;
    }
    await checkPortFree(this.httpPort);
    this.#logFd = openSync(this.#logPath, 'a');
    this.#stderrTail = '';
    const child = spawn(this.#migodBin, [], {
      env: this.#env,
      // Stdout goes straight to the log file; stderr is piped so the harness can both
      // tee it into the same log and keep the last of it in memory. The in-memory copy
      // is what a startup failure quotes: when migod refuses its configuration it says
      // so on stderr and exits before anything else, and a failure message that carries
      // only an exit code sends every future config regression back to CI to be seen.
      stdio: ['ignore', this.#logFd, 'pipe'],
    });
    this.#child = child;
    this.#spawnError = null;
    // A missing or unrunnable binary reports here rather than as an unhandled 'error'
    // event that would kill the test process before it could name the cause.
    child.once('error', (error: Error) => {
      this.#spawnError = error;
    });
    child.stderr?.on('data', (chunk: Buffer) => {
      const text = chunk.toString('utf8');
      this.#stderrTail = (this.#stderrTail + text).slice(-STDERR_TAIL_BYTES);
      if (this.#logFd !== null) {
        try {
          writeSync(this.#logFd, chunk);
        } catch {
          // The log file is best-effort; the in-memory tail still holds the bytes.
        }
      }
    });
    child.once('exit', () => {
      if (this.#child === child) {
        this.#releaseLog();
      }
    });
    await this.waitHealthy();
  }

  /** Stops the node: SIGTERM first, SIGKILL only after the grace period. */
  async stop(): Promise<void> {
    const child = this.#child;
    if (child === null) {
      return;
    }
    this.#child = null;
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
      if (!(await waitForExit(child, SHUTDOWN_GRACE_MS))) {
        child.kill('SIGKILL');
        await waitForExit(child, SHUTDOWN_GRACE_MS);
      }
    }
    this.#releaseLog();
  }

  /** Stops the node and starts it again with the same config, database, and media. */
  async restart(): Promise<void> {
    await this.stop();
    await this.start();
  }

  /** Everything a run owns, in the reverse order it was created. Idempotent. */
  async destroy(): Promise<void> {
    await this.stop();
    // WITH (FORCE): the node's pool connections may not have closed yet, and a plain
    // DROP would fail on them. Forced, the drop always completes.
    psql(this.#target, `DROP DATABASE IF EXISTS "${this.#databaseName}" WITH (FORCE)`);
    rmSync(this.#runDir, { recursive: true, force: true });
  }

  /** Polls /health until the node answers, failing with the log tail if it never does. */
  async waitHealthy(): Promise<void> {
    const deadline = Date.now() + HEALTH_TIMEOUT_MS;
    for (;;) {
      const child = this.#child;
      if (this.#spawnError !== null) {
        throw new Error(
          `migod could not be started at ${this.#migodBin}: ${this.#spawnError.message}. ` +
            'Build it first — make test-e2e does, or see tools/2node/run.sh for the manual path.',
        );
      }
      if (child !== null && child.exitCode !== null) {
        throw new Error(
          `migod exited with code ${child.exitCode} during startup\n` +
            `--- migod stderr ---\n${this.stderrTail()}\n--- migod log tail ---\n${this.logTail()}`,
        );
      }
      try {
        const response = await fetch(`${this.apiUrl}/health`);
        if (response.ok) {
          return;
        }
      } catch {
        // Not listening yet — the ordinary first few rounds.
      }
      if (Date.now() > deadline) {
        throw new Error(
          `migod did not answer /health on ${this.apiUrl} within ${HEALTH_TIMEOUT_MS}ms\n` +
            `--- migod stderr ---\n${this.stderrTail()}\n--- migod log tail ---\n${this.logTail()}`,
        );
      }
      await sleep(200);
    }
  }

  /**
   * The last of what the node wrote to stderr, verbatim.
   *
   * This, not the log file, is the authoritative failure text: a refused configuration
   * is printed by the process before it has emitted a single log line, and the pipe the
   * harness holds delivers those bytes straight into memory, where no read-back race
   * can lose them.
   */
  stderrTail(): string {
    return this.#stderrTail === '' ? '(migod wrote nothing to stderr)' : this.#stderrTail.trimEnd();
  }

  /** The last 40 lines of the node's log, for failure messages a person can act on. */
  logTail(): string {
    try {
      return readFileSync(this.#logPath, 'utf8')
        .split('\n')
        .filter((line) => line !== '')
        .slice(-40)
        .join('\n');
    } catch {
      return '(no log could be read)';
    }
  }

  #releaseLog(): void {
    if (this.#logFd !== null) {
      closeSync(this.#logFd);
      this.#logFd = null;
    }
  }
}
