/**
 * Which part of a run is in flight, and what it means when none of them finishes.
 *
 * A run is a chain of awaits, and the failure this module exists for is the one that leaves no
 * trace: an await that never settles while the event loop has nothing left to do. Node does not
 * treat that as an error — it treats it as a program that has finished its work, exits, and
 * leaves `process.exitCode` exactly as it found it, which is zero. Nothing is written to stdout,
 * so a harness reading the exit code sees success and a harness reading the report sees a file
 * that was never written; between the two, a run that opened no session at all passes as a run
 * that did what it claimed.
 *
 * The defence is not to hold the process open — a keep-alive turns a silent lie into a silent
 * hang, which is worse, because a hang has to be timed out before anybody learns anything. The
 * defence is to name the phase: when the loop drains with a run still in flight, the last phase
 * the run entered is the await that never settled, and saying so turns an unexplained exit into
 * a diagnosis. {@link stallMessage} is that sentence, and {@link PhaseTracker} is what holds the
 * phase while the run moves through it.
 */

/** The stages of a run, in the order {@link import('./runner.js').run} enters them. */
export type RunPhase =
  'building' | 'connecting' | 'preparing' | 'steady-state' | 'settling' | 'disconnecting' | 'done';

/**
 * The phase a run is in, readable from outside the run.
 *
 * One mutable cell rather than a callback, because the reader is an exit hook that runs at an
 * arbitrary moment with no run in scope: `process.on('beforeExit')` fires while the run's own
 * promise is still pending, so the only thing that can answer "what was it waiting for?" is
 * state the run left behind. The run writes it on every transition and reads nothing from it —
 * the tracker never steers a run, so a phase that is read a moment late is a stale label, never
 * a changed behaviour.
 */
export class PhaseTracker {
  #phase: RunPhase = 'building';

  /** Records the phase the run is entering. Monotonic by construction: the run only goes forward. */
  set(phase: RunPhase): void {
    this.#phase = phase;
  }

  /** The phase the run last entered, or `building` when it never got as far as connecting. */
  get phase(): RunPhase {
    return this.#phase;
  }

  /** Whether the run reached its end. A drain after this is an ordinary exit, not a stall. */
  get finished(): boolean {
    return this.#phase === 'done';
  }
}

/** What each phase was waiting on, in the words a reader of the log needs. */
const WAITING_ON: Record<RunPhase, string> = {
  building:
    'building its virtual users — a synchronous loop, so a drain here means construction threw ' +
    'without settling the run',
  connecting: 'opening sessions (register + gateway handshake) for its virtual users',
  preparing: "wiring up the scenario's shared state (accounts, pairs, conversations)",
  'steady-state': 'holding the workload window and driving its workloads',
  settling: "the scenario's settle phase — in-flight deliveries and integrity verdicts",
  disconnecting: 'closing its sessions',
  done: 'nothing — the run finished',
};

/**
 * The sentence a stalled run prints about itself.
 *
 * It names the phase, says what that phase was doing, and states the one fact that makes it a
 * stall rather than a slow run: there is no pending I/O left. A reader who does not know Node's
 * exit rule cannot tell "the loop drained" from "the machine is slow", and the difference decides
 * whether the answer is a longer timeout or a bug hunt — so the message says it outright.
 */
export function stallMessage(phase: RunPhase): string {
  return (
    `loadgen: the run never finished. It was ${WAITING_ON[phase]}, and the event loop is now ` +
    'empty — no socket, no timer, and no pending I/O is keeping this process alive. That means ' +
    'an awaited step will never settle, not that the run is slow: whatever it is waiting on had ' +
    'already happened, or will never be signalled. No report was written, so this run measured ' +
    'nothing and must not be read as a pass.'
  );
}
