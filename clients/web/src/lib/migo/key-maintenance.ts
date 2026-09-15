/**
 * The inbound key maintenance the web client runs after every event that mutates its key store.
 *
 * Two inbound facts consume this device's cryptographic material, and both arrive on the messaging
 * domain:
 *
 *   - a first message from a new peer, which spends one of this device's one-time prekeys
 *     establishing the pairwise session;
 *   - an accepted sender-key distribution ({@link MigoClient}'s section 163 wiring now also feeds
 *     redistributions in through `GROUP_KEY_DISTRIBUTE`), whose pairwise reply commits a ratchet
 *     and mutates the session store the same way.
 *
 * Either one leaves the persisted key-store snapshot stale and the one-time prekey pool one
 * lighter, so both owe the same response: persist the snapshot, and top the pool up (a replenish
 * that crosses the publish threshold republishes the bundle, which mutates the store again — hence
 * the second persist when it reports it published).
 *
 * Structural rather than importing {@link MigoClient} directly: the provider passes the client it
 * just built, and the test passes a fake with the same two listeners and the same replenish —
 * everything this helper needs, and nothing it does not.
 */

/** The messaging surface the maintenance runs against; {@link MigoClient} satisfies this structurally. */
export interface KeyMaintenanceTarget {
  readonly messaging: {
    /** Fires once per accepted inbound message. */
    onMessage(handler: () => void): () => void;
    /** Fires once per accepted inbound key distribution (a spent prekey or a committed session). */
    onKeyExchange(handler: () => void): () => void;
  };
  /** Tops the one-time prekey pool up; resolves `true` when it republished the bundle. */
  replenishPrekeys(): Promise<boolean>;
}

/**
 * Subscribes the maintenance to both inbound facts and returns the one unsubscribe that undoes
 * both. Persisting is scheduled (the caller's scheduler may coalesce), replenishment is
 * fire-and-forget, and a failed replenish is swallowed — it is best-effort and retried on the
 * next inbound event, exactly as before.
 */
export function wireKeyMaintenance(
  target: KeyMaintenanceTarget,
  schedulePersist: () => void,
): () => void {
  const maintain = (): void => {
    schedulePersist();
    void target
      .replenishPrekeys()
      .then((published) => {
        if (published) {
          schedulePersist();
        }
      })
      .catch(() => {
        // Replenishment is best-effort; a failure is retried on the next inbound event.
      });
  };
  const unsubscribes = [
    target.messaging.onMessage(maintain),
    target.messaging.onKeyExchange(maintain),
  ];
  return () => {
    for (const unsubscribe of unsubscribes) {
      unsubscribe();
    }
  };
}
