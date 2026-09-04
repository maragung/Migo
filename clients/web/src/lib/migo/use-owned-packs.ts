/**
 * The account's purchased packs, as the composer's picker reads them.
 *
 * One `ENTITLEMENTS` read per session, cached in module state because every chat window mounts
 * the same picker and the answer is per-account, not per-conversation. A second client (another
 * tab) buying a pack does not refresh this list until reload — the honest trade for not polling
 * a spend the user just watched complete in the store tab.
 */

import { useEffect, useState } from 'react';

import type { MigoClient } from '@migo/sdk';

/** The owned-SKU set the module holds between mounts. */
let cached: Set<string> | null = null;

/** Reads the caller's entitlements as a SKU set, `null` while the read is in flight. */
export function useOwnedPacks(client: MigoClient | null): ReadonlySet<string> | null {
  const [owned, setOwned] = useState<ReadonlySet<string> | null>(cached);

  useEffect(() => {
    if (client === null || cached !== null) {
      return;
    }
    let cancelled = false;
    client.economy
      .getEntitlements()
      .then((items) => {
        const set = new Set(items.map((entry) => entry.sku));
        cached = set;
        if (!cancelled) {
          setOwned(set);
        }
      })
      .catch(() => {
        // No entitlements rather than wrong ones: the picker shows the free set alone, which is
        // exactly what an account with no purchases renders.
        cached = new Set();
        if (!cancelled) {
          setOwned(cached);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  return owned;
}
