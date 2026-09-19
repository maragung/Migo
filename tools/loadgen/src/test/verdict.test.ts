/**
 * The verdict gate's whole job is to refuse a report that describes a run which did not happen, so
 * the cases below are the shapes a dishonest pass arrives in: a file that was never written, JSON
 * that never finished, a run that connected to nothing, a run that ended in a fifth of its window,
 * and a run that connected and then measured nothing. Each is asserted to fail with its own reason,
 * because the reason is what the CI log prints and "it failed" is not a diagnosis.
 *
 * The honest case is asserted as hard as the dishonest ones: a gate that fails everything is as
 * useless as one that passes everything, and the step shapes here are copied from run-full.sh's
 * defaults so a rule that would reject a real step shows up here rather than in a nightly.
 */

import assert from 'node:assert/strict';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import { judgeReport, readReport } from '../verdict.js';

/** A report shape the way `renderJson` writes it, with the fields the gate reads. */
function report(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    scenario: 'messaging',
    interrupted: false,
    requestedVus: 40,
    connectedCount: 40,
    durationMs: 120_000,
    targetDurationMs: 120_000,
    errorRate: 0,
    ok: true,
    operations: [
      { label: 'connect', ok: 40, errors: 0 },
      { label: 'send', ok: 6000, errors: 0 },
    ],
    ...overrides,
  };
}

test('a run that did the thing it names passes', () => {
  const verdict = judgeReport(report(), { step: 'msg-rate' });
  assert.equal(verdict.ok, true, verdict.detail);
  assert.match(verdict.detail, /40 session\(s\)/);
  assert.match(verdict.detail, /6040 operation\(s\)/);
});

test('a report with no session in it cannot pass, whatever else it says', () => {
  const verdict = judgeReport(report({ connectedCount: 0, durationMs: 0 }), { step: 'idle-10k' });
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'no-session-opened');
});

test('the step decides how many sessions are enough', () => {
  const document = report({ connectedCount: 9_500, requestedVus: 10_000 });
  assert.equal(judgeReport(document, { step: 'idle-10k', minConnected: 10_000 }).ok, false);
  assert.equal(
    judgeReport(document, { step: 'idle-10k', minConnected: 10_000 }).reason,
    'no-session-opened',
  );
  assert.equal(judgeReport(document, { step: 'idle-10k', minConnected: 9_000 }).ok, true);
});

test('a run that ended a fifth of the way into its window is not that experiment', () => {
  // The measured failure: 10000 VUs asked for a 60s hold, connect began, and the process was gone
  // 1.8 seconds later with exit status 0.
  const verdict = judgeReport(
    report({ connectedCount: 10_000, durationMs: 1_800, targetDurationMs: 60_000 }),
    {
      step: 'idle-10k',
    },
  );
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'ended-early');
  assert.match(verdict.detail, /1\.8s of its 60\.0s window/);
});

test('a tolerated shortfall of the hold is tolerated', () => {
  const document = report({ durationMs: 110_000, targetDurationMs: 120_000 });
  assert.equal(judgeReport(document, { step: 'msg-rate' }).ok, true);
  const just_under = report({ durationMs: 100_000, targetDurationMs: 120_000 });
  assert.equal(judgeReport(just_under, { step: 'msg-rate' }).reason, 'ended-early');
});

test('a run that connected and measured nothing is not a pass', () => {
  const verdict = judgeReport(
    report({
      operations: [
        { label: 'connect', ok: 40, errors: 0 },
        { label: 'send', ok: 0, errors: 0 },
      ],
    }),
    { step: 'msg-rate' },
  );
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'no-measurement');
});

test('an interrupted run is never a verdict on the window it was asked for', () => {
  const verdict = judgeReport(report({ interrupted: true }), { step: 'calls' });
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'interrupted');
});

test('the budget is re-read from the report, not taken on trust from the exit code', () => {
  const verdict = judgeReport(report({ errorRate: 0.2 }), { step: 'msg-rate', maxErrorRate: 0.05 });
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'over-budget');
});

test('a report whose shape changed is named as such rather than passed', () => {
  const verdict = judgeReport({ scenario: 'messaging' }, { step: 'msg-rate' });
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'not-a-report');
  assert.match(verdict.detail, /connectedCount, durationMs and targetDurationMs/);
});

test('an empty report file is read as the absence it is, not as a passing report', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'migo-verdict-'));
  const empty = join(dir, 'empty.json');
  await writeFile(empty, '');
  const read = await readReport(empty);
  assert.equal(read.kind, 'missing');
  if (read.kind === 'missing') assert.match(read.detail, /empty/);
});

test('a truncated report is not JSON, and not a pass either', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'migo-verdict-'));
  const truncated = join(dir, 'truncated.json');
  await writeFile(truncated, '{"scenario": "messaging", "connectedCount": 4');
  const read = await readReport(truncated);
  assert.equal(read.kind, 'missing');
  if (read.kind === 'missing') assert.match(read.detail, /not JSON/);
});

test('a report that was written is read back whole', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'migo-verdict-'));
  const path = join(dir, 'report.json');
  await writeFile(path, JSON.stringify(report()));
  const read = await readReport(path);
  assert.equal(read.kind, 'report');
  if (read.kind === 'report') {
    assert.equal(judgeReport(read.document, { step: 'msg-rate' }).ok, true);
  }
});

test('a missing report file says so instead of throwing', async () => {
  const read = await readReport(join(tmpdir(), 'migo-verdict-does-not-exist.json'));
  assert.equal(read.kind, 'missing');
});
