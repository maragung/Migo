/**
 * Wire-byte accounting for a load run — brief section 171's loadgen half of the bandwidth budget.
 *
 * The counting itself is not re-implemented here. The SDK transport already keeps the §171
 * per-session counters (`WireBytes` in @migo/sdk): every byte successfully written to the gateway
 * socket at its post-compression wire size (frame headers included, HELLO and ACK included) and
 * every byte read off the socket at the incoming record's outer envelope size. The counters are
 * client-local, span the transport's lifetime, and survive a reconnect — counting the reconnect's
 * own handshake too, because those bytes are a real cost the client paid. A virtual user builds
 * exactly one transport per run, so its final reading is precisely the session §171 calls a
 * session, and this module turns those per-VU readings into the figure the brief asks loadgen to
 * report: bytes per user per minute, per scenario.
 *
 * Scope, stated honestly: the counters cover the gateway socket only. The REST bootstrap
 * (registration, key publication, token refresh) rides plain HTTP and is not part of WireBytes,
 * so the budgets below govern the realtime path the scenarios actually load.
 */

import type { WireBytes } from '@migo/sdk';

/**
 * How far past its budget a scenario may land before the run fails. Section 171 fixes this at 10
 * percent, so a budget is a regression gate with measured jitter allowed for, not a tight pin.
 */
export const BYTE_BUDGET_HEADROOM = 0.1;

/** A run's wire bytes reduced to the per-user, per-minute figure §171 asks loadgen to report. */
export interface WireByteSummary {
  /** Virtual users whose readings the summary covers (the ones that held a session). */
  readonly users: number;
  /** The steady-state window the bytes are normalized over, in minutes. */
  readonly minutes: number;
  readonly sentBytes: number;
  readonly receivedBytes: number;
  readonly totalBytes: number;
  readonly perUserBytes: number;
  readonly bytesPerUserPerMinute: number;
}

/**
 * Reduces per-VU wire-byte readings to the summary figure.
 *
 * Pure arithmetic on its inputs: no clock, no randomness, so the report built on top stays a
 * deterministic function of the run. A run with no users or no elapsed steady window has no
 * meaningful rate and reads as zero rather than dividing by zero — an interrupted run's verdict
 * comes from the interrupt flag and the error budget, not from an infinite byte rate.
 */
export function summarizeWireBytes(
  readings: readonly WireBytes[],
  durationMsActual: number,
): WireByteSummary {
  let sentBytes = 0;
  let receivedBytes = 0;
  for (const reading of readings) {
    sentBytes += reading.sent;
    receivedBytes += reading.received;
  }
  const users = readings.length;
  const minutes = durationMsActual / 60_000;
  const totalBytes = sentBytes + receivedBytes;
  const perUserBytes = users === 0 ? 0 : totalBytes / users;
  const bytesPerUserPerMinute = users === 0 || minutes <= 0 ? 0 : perUserBytes / minutes;
  return {
    users,
    minutes,
    sentBytes,
    receivedBytes,
    totalBytes,
    perUserBytes,
    bytesPerUserPerMinute,
  };
}

/** One scenario's byte budget, with the doc anchor it was derived from. */
export interface ScenarioByteBudget {
  readonly scenario: string;
  /** Gateway wire bytes per connected user per minute this scenario may spend. */
  readonly bytesPerUserPerMinute: number;
  /** The brief section the number derives from, and the reasoning behind the headroom. */
  readonly anchor: string;
}

/**
 * Per-scenario byte budgets, gateway wire bytes per user per minute.
 *
 * These are regression gates, not tight pins: each is set generously above what the scenario
 * measures at its default shape (the CI runner's fixed virtual-user count, default `--rate 5`,
 * a duration of at least 30 s), so the gate fires when something regressed — a handshake that
 * grew a field, a heartbeat that sped up, a fan-out that stopped coalescing — and not on jitter.
 * Where section 56 states a per-event or per-session target the number is derived from it; the
 * derivation is written out per budget. A scenario added without a budget here runs ungated, so
 * adding a scenario means adding its budget in the same change, the same way §171 demands a frame
 * measurement for every new opcode.
 */
const BUDGETS: readonly ScenarioByteBudget[] = [
  {
    scenario: 'connect',
    bytesPerUserPerMinute: 8 * 1024,
    anchor:
      'section 56 session budgets: handshake (HELLO + AUTHENTICATE + Welcome) at most 512 bytes, ' +
      'an idle session at most 8 KB per hour of heartbeat (PING 11 + Pong 17 measured, one pair ' +
      'per negotiated 30 s interval is about 56 B/min); at CI shape (sessions held 30 s) the ' +
      'one-time handshake and initial subscribe amortize to roughly 1.2 KB/user/min, so 8 KiB/min ' +
      'is several times the measured figure',
  },
  {
    scenario: 'presence',
    bytesPerUserPerMinute: 64 * 1024,
    anchor:
      'section 56 per-event budgets: the client PresenceUpdate at most 16 bytes, the fan-out ' +
      'PresenceEvent at most 32, each request acked within 10; at the default 5 flips/s that is ' +
      '300 operations per user per minute and well under 100 B/op on the wire, so 64 KiB/min ' +
      'assumes the default --rate and leaves roughly double the estimated spend',
  },
  {
    scenario: 'messaging',
    bytesPerUserPerMinute: 256 * 1024,
    anchor:
      'section 56 text-message budget: at most 96 bytes of overhead plus ciphertext (a short ' +
      'loadgen message seals to roughly 90 bytes of envelope on top of the plaintext), every send ' +
      'acked within 10 bytes, the receiving side watermark receipt 24; at the default 5 sends/s ' +
      'each side of a pair spends roughly 60 KB/user/min plus the one-time conversation setup and ' +
      'sender-key distribution, so 256 KiB/min leaves about four times headroom',
  },
];

const BUDGETS_BY_SCENARIO = new Map(BUDGETS.map((budget) => [budget.scenario, budget]));

/** The byte budget for `scenario`, or undefined when the scenario has none (and runs ungated). */
export function scenarioByteBudget(scenario: string): ScenarioByteBudget | undefined {
  return BUDGETS_BY_SCENARIO.get(scenario);
}

/** The gate's answer for one scenario: what was measured, what was allowed, and whether it broke. */
export interface ByteBudgetVerdict {
  readonly budget: ScenarioByteBudget | undefined;
  readonly measuredBytesPerUserPerMinute: number;
  /** The budget grown by the §171 headroom; null when the scenario has no budget. */
  readonly limitBytesPerUserPerMinute: number | null;
  /**
   * True only when a budget exists and the measurement is strictly past budget + 10 percent:
   * landing exactly on the limit passes, because §171 fails a scenario that exceeds the budget
   * by *more than* 10 percent.
   */
  readonly exceeded: boolean;
}

export function byteBudgetVerdict(
  scenarioName: string,
  summary: WireByteSummary,
): ByteBudgetVerdict {
  const budget = scenarioByteBudget(scenarioName);
  const measured = summary.bytesPerUserPerMinute;
  if (budget === undefined) {
    return {
      budget,
      measuredBytesPerUserPerMinute: measured,
      limitBytesPerUserPerMinute: null,
      exceeded: false,
    };
  }
  const limit = budget.bytesPerUserPerMinute * (1 + BYTE_BUDGET_HEADROOM);
  return {
    budget,
    measuredBytesPerUserPerMinute: measured,
    limitBytesPerUserPerMinute: limit,
    exceeded: measured > limit,
  };
}
