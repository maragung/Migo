'use client';

/**
 * A direct conversation's verification surface: the pair safety numbers, and the key-change warning
 * behind them (§47, §164).
 *
 * # What a read does, and deliberately does not do
 *
 * The read enumerates the peer's published device identities through the SDK (`peerIdentities`,
 * one bundle fetch per user, cached for the client's life) and derives one pair number per device —
 * this device's E2EE fingerprint and that peer device's, hashed together in a symmetric order, so
 * both people read the same string off their own screens.
 *
 * Against each device the read compares the fingerprint with the last one this conversation
 * *acknowledged* (the peer-identity store). Three outcomes, with the semantics the Android client
 * established and the brief demands:
 *
 *   - **First observation** — nothing changed, because nothing was ever seen. The fingerprint is
 *     recorded silently as the baseline.
 *   - **Same fingerprint** — no change, nothing written.
 *   - **Different fingerprint** — *changed*, and the store is deliberately NOT written. The warning
 *     stays on the screen until a person acknowledges it; the read that detected the change is the
 *     one party that must never clear it.
 *
 * Acknowledgment is a separate, explicit act ({@link useSafety}'s `acknowledge`): it re-reads the
 * (cached) identities and records every current fingerprint as the acknowledged one.
 *
 * # What a change means, honestly
 *
 * A changed identity is also what an honest reinstall looks like: a peer who reinstalled the app
 * publishes a fresh device identity, and their conversation with this device shows the same warning
 * an attacker-in-the-middle would. The warning therefore does not block the conversation — messages
 * still send and still decrypt — it refuses to let the change pass unremarked, which is the entire
 * requirement. What resolves the question is the out-of-band comparison the panel's explanation
 * invites: read the number aloud to the peer, and a mismatch means stop.
 */

import { useCallback, useEffect, useState } from 'react';

import { pairSafetyNumber } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { loadPeerIdentity, savePeerIdentity } from '@/lib/storage/peer-identity-store.js';

import { useMigo } from './use-migo.js';

/** One peer device's safety number, as a conversation's verification surface shows it. */
export interface PeerSafetyNumber {
  /** The peer device the number was derived with. */
  deviceId: Id;
  /** The number itself, already rendered. */
  number: string;
  /** True when the device's fingerprint changed since this conversation last acknowledged one. */
  changed: boolean;
}

/** One device's identity as a read observes it: the device it belongs to and its fingerprint. */
export interface SafetyObservation {
  deviceId: Id;
  fingerprint: Uint8Array;
}

/** The pure half of the read: the report to show, and the first observations to record silently. */
export function reconcileSafety(
  own: Uint8Array,
  observations: readonly SafetyObservation[],
  acknowledged: ReadonlyMap<Id, Uint8Array>,
): { report: PeerSafetyNumber[]; firstSeen: SafetyObservation[] } {
  const report: PeerSafetyNumber[] = [];
  const firstSeen: SafetyObservation[] = [];
  for (const observation of observations) {
    const stored = acknowledged.get(observation.deviceId);
    if (stored === undefined) {
      // Nothing was ever acknowledged for this device: this is the baseline, not a change.
      firstSeen.push(observation);
    }
    report.push({
      deviceId: observation.deviceId,
      number: pairSafetyNumber(own, observation.fingerprint),
      changed: stored !== undefined && !bytesEqual(stored, observation.fingerprint),
    });
  }
  return { report, firstSeen };
}

/**
 * Whether two byte strings are the same length and the same bytes.
 *
 * Exported because the security checkup's aggregate read asks the same question of the same
 * fingerprints, and two implementations of a comparison a warning stands on is one too many.
 */
export function bytesEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) {
    return false;
  }
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) {
      return false;
    }
  }
  return true;
}

/** The slice of the client the verification surface reads, so tests can supply a double. */
export interface SafetyClient {
  readonly keyStore: {
    /** This device's E2EE identity public key, whose fingerprint a pair number derives from. */
    publicIdentity(): { fingerprint(): Uint8Array };
  };
  /** The peer's published device identities (one enumeration per user, cached in the SDK). */
  peerIdentities(
    userId: Id,
  ): Promise<ReadonlyArray<{ deviceId: Id; identity: { fingerprint(): Uint8Array } }>>;
}

/**
 * Reads a direct conversation's safety numbers: one per device the peer currently publishes.
 *
 * `conversationId` and `peerUserId` are `null` for a surface with nothing to verify (a non-direct
 * conversation, a note to self), in which case the hook stays idle — `numbers` `null`, no read. A
 * failed read lands on `failure` with a `retry` to offer, because a number shown before the read
 * lands would be a number invented on the spot, and a read that failed silently would be a
 * verification surface that quietly verifies nothing.
 */
export function useSafety(
  conversationId: Id | null,
  peerUserId: Id | null,
): {
  /** The report, or `null` while the read is in flight or there is nothing to verify. */
  numbers: PeerSafetyNumber[] | null;
  /** Why the read failed, when it did; the numbers stay `null` rather than going stale. */
  failure: string | null;
  /** Whether any peer device's identity changed since this conversation acknowledged it. */
  changed: boolean;
  /** Records every current fingerprint as the acknowledged one — the person's act, never the read's. */
  acknowledge: () => void;
  /** Re-runs the read, for the retry a failed verification surface owes. */
  retry: () => void;
} {
  const { client } = useMigo();
  const [numbers, setNumbers] = useState<PeerSafetyNumber[] | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  // Bumped by `retry` so the effect re-runs without remounting the surface.
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (client === null || conversationId === null || peerUserId === null) {
      setNumbers(null);
      setFailure(null);
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      try {
        const identities = await client.peerIdentities(peerUserId);
        if (cancelled) {
          return;
        }
        // One store read per device, gathered: the acknowledged map the comparison needs in full
        // before any conclusion is drawn, the same two passes the pure function takes.
        const observations: SafetyObservation[] = identities.map((peer) => ({
          deviceId: peer.deviceId,
          fingerprint: peer.identity.fingerprint(),
        }));
        const acknowledged = new Map<Id, Uint8Array>();
        for (const observation of observations) {
          const stored = await loadPeerIdentity(conversationId, observation.deviceId);
          if (stored !== undefined) {
            acknowledged.set(observation.deviceId, stored);
          }
        }
        if (cancelled) {
          return;
        }
        const own = client.keyStore.publicIdentity().fingerprint();
        const { report, firstSeen } = reconcileSafety(own, observations, acknowledged);
        // The baseline write happens after the comparison, in the same read that decided it was a
        // baseline: a device first seen is recorded silently, and a changed one is left exactly as
        // the store holds it — unacknowledged, and therefore still warning.
        for (const observation of firstSeen) {
          await savePeerIdentity(conversationId, observation.deviceId, observation.fingerprint);
        }
        if (!cancelled) {
          setNumbers(report);
          setFailure(null);
        }
      } catch (cause) {
        if (!cancelled) {
          setNumbers(null);
          setFailure(friendlyError(cause));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, conversationId, peerUserId, attempt]);

  const acknowledge = useCallback((): void => {
    const active = client;
    if (active === null || conversationId === null || peerUserId === null) {
      return;
    }
    void (async (): Promise<void> => {
      try {
        // The identities come from the SDK's per-run cache, so an acknowledgment costs no prekeys;
        // the trade is that a peer who rotated *again* between the report and this call has that
        // newer key acknowledged unseen — a seconds-wide window that closes on the next read.
        const identities = await active.peerIdentities(peerUserId);
        for (const peer of identities) {
          await savePeerIdentity(conversationId, peer.deviceId, peer.identity.fingerprint());
        }
        setNumbers((prev) =>
          prev === null
            ? prev
            : prev.map((entry) => ({
                ...entry,
                changed: false,
              })),
        );
      } catch (cause) {
        setFailure(friendlyError(cause));
      }
    })();
  }, [client, conversationId, peerUserId]);

  const retry = useCallback((): void => {
    setAttempt((value) => value + 1);
  }, []);

  const changed = numbers !== null && numbers.some((entry) => entry.changed);
  return { numbers, failure, changed, acknowledge, retry };
}
