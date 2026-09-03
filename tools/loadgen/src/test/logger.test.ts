/**
 * The logger's two jobs: keep stdout clean for the report, and never let a secret reach the
 * terminal. The channel discipline is load-bearing — `--output json > run.json` only yields a clean
 * document if every diagnostic goes to stderr — so it is asserted, not assumed, by capturing both
 * streams. Redaction is asserted through the real `write` path rather than by calling `redact`
 * directly, which is what proves the logger is actually wired to it.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { Logger } from '../logger.js';

interface Captured {
  readonly stderr: string;
  readonly stdout: string;
}

/** Run `fn` with both standard streams captured instead of written. */
function capture(fn: () => void): Captured {
  // Bound rather than referenced bare: a stream's `write` needs its own `this`, and restoring an
  // unbound reference would leave a method that throws the moment the next test writes anything.
  const originalErr = process.stderr.write.bind(process.stderr);
  const originalOut = process.stdout.write.bind(process.stdout);
  let stderr = '';
  let stdout = '';
  process.stderr.write = (chunk: unknown) => {
    stderr += String(chunk);
    return true;
  };
  process.stdout.write = (chunk: unknown) => {
    stdout += String(chunk);
    return true;
  };
  try {
    fn();
  } finally {
    process.stderr.write = originalErr;
    process.stdout.write = originalOut;
  }
  return { stderr, stdout };
}

test('a secret handed to the logger is redacted before it is written', () => {
  const log = new Logger('normal');
  const { stderr } = capture(() => log.warn('registered with passphrase=hunter2 for the run'));
  assert.ok(!stderr.includes('hunter2'), 'the secret must not reach stderr');
  assert.ok(stderr.includes('[redacted]'));
  assert.ok(stderr.startsWith('warning: '));
});

test('a credential embedded in a logged URL is stripped', () => {
  const log = new Logger('normal');
  const { stderr } = capture(() => log.error('could not read http://svc:t0pS3cret@host/v1/config'));
  assert.ok(!stderr.includes('t0pS3cret'));
  assert.ok(stderr.startsWith('error: '));
});

test('everything the logger writes goes to stderr, never stdout', () => {
  const log = new Logger('verbose');
  const { stderr, stdout } = capture(() => {
    log.info('phase');
    log.debug('detail');
    log.warn('careful');
    log.error('broken');
  });
  assert.equal(stdout, '', 'stdout is reserved for the report');
  assert.ok(stderr.length > 0);
});

test('quiet suppresses info and debug but never warn or error', () => {
  const log = new Logger('quiet');
  assert.equal(capture(() => log.info('phase')).stderr, '');
  assert.equal(capture(() => log.debug('detail')).stderr, '');
  assert.ok(capture(() => log.warn('careful')).stderr.includes('careful'));
  assert.ok(capture(() => log.error('broken')).stderr.includes('broken'));
});

test('debug is shown only at verbose; info at normal and verbose', () => {
  assert.equal(capture(() => new Logger('normal').debug('detail')).stderr, '');
  assert.ok(capture(() => new Logger('verbose').debug('detail')).stderr.includes('detail'));
  assert.ok(capture(() => new Logger('normal').info('phase')).stderr.includes('phase'));
});
