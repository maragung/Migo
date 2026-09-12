/**
 * The gate that decides whether client-side sync work may run right now.
 *
 * Section 158 asks that sync running when the application goes to the background be stopped
 * neatly rather than left burning battery — and resumed the moment the page is visible again.
 * "Neatly" is the operative word: a send already in flight is allowed to finish (tearing it up
 * mid-request is exactly how a message is believed sent and never lands), and no subscription
 * is dropped, because subscriptions are session state and the gateway holds them across a
 * hidden tab. What stops is the *next* unit of sync work: the outbox drain parks between
 * entries and does not resume until the gate opens.
 *
 * The gate is deliberately dumb — an open/closed flag and a waiter — so anything with a
 * "should I keep going?" question can consult it without knowing what visibility is. The
 * client wires it to `document.visibilitychange` when there is a document (and can be driven
 * by hand through {@link MigoClient.setPageVisible} when there is not, which is how a test or
 * a non-browser host controls it).
 */

/** A handler notified on every gate transition. */
type Listener = (open: boolean) => void;

/**
 * Whether client-side sync work may run.
 *
 * One instance per client. Open by default: a foreground page, a host with no visibility
 * notion at all, and a client constructed before the application says anything are all
 * "allowed to sync", because a gate that starts closed would silently swallow the first
 * sends of a session that never hid in the first place.
 */
export class SyncGate {
  #open = true;
  readonly #listeners = new Set<Listener>();

  /** Whether sync work may run right now. */
  get open(): boolean {
    return this.#open;
  }

  /**
   * Opens or closes the gate, notifying listeners only on a real transition.
   *
   * Re-asserting the current state is a no-op by design: a visibilitychange event that
   * fires for a state the gate already knows about is common, and waking every waiter to
   * tell them what they already know would turn the parked drain into a busy loop.
   */
  setOpen(open: boolean): void {
    if (this.#open === open) {
      return;
    }
    this.#open = open;
    for (const listener of [...this.#listeners]) {
      try {
        listener(open);
      } catch {
        // A listener that throws must not stop the others from hearing the transition.
      }
    }
  }

  /**
   * Resolves when the gate is open — immediately if it already is.
   *
   * This is how the outbox drain parks: between entries it awaits the gate, so a closed
   * gate costs no timer, no poll, and no battery, and the drain wakes the instant the page
   * becomes visible again.
   */
  wait(): Promise<void> {
    if (this.#open) {
      return Promise.resolve();
    }
    return new Promise<void>((resolve) => {
      const unsubscribe = this.onChange((open) => {
        if (open) {
          unsubscribe();
          resolve();
        }
      });
    });
  }

  /** Registers a transition listener. Returns an unsubscribe function. */
  onChange(listener: Listener): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }
}
