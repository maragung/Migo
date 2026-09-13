'use client';

/**
 * The caller's live $MIG coin balance.
 *
 * The wallet's money-side facts are the server's arithmetic, so this hook never holds a number it
 * computed: it reads the balance once when the session is ready and re-reads it whenever the
 * server says the wallet moved — an `ECONOMY_EVENT` on the caller's own user topic, published
 * after every spend this account makes from any device. The event is a cue, not a fact: it names
 * the kind of movement but never the resulting balance, so the refresh is a full
 * `getBalance` round trip, never local arithmetic applied to a number the server did not vouch
 * for.
 *
 * A failed read leaves the previous balance standing rather than zeroing it — a wallet that
 * failed to load is not an empty one, and the difference matters to whoever is about to spend.
 */

import { useEffect, useState } from 'react';

import { useMigo } from '@/lib/migo/use-migo.js';

/** The caller's coin balance, or null while the first read is still in flight or has failed. */
export function useBalance(): number | null {
  const { client, resetNonce } = useMigo();
  const [balance, setBalance] = useState<number | null>(null);

  useEffect(() => {
    if (!client) {
      return;
    }
    // A reset rebuilds the session: the balance of the world before it is not a fact about the
    // world after it, so the read starts over rather than trusting the number it happens to hold.
    setBalance(null);
    let cancelled = false;
    const read = (): void => {
      client.economy
        .getBalance()
        .then((wallet) => {
          if (!cancelled) {
            setBalance(wallet.balance);
          }
        })
        .catch(() => {});
    };
    read();
    // The live tick: any spend this account made — here, on the phone, on another browser — is
    // answered by an ECONOMY_EVENT on our own user topic, and the only honest response is to
    // re-read what the server now says we hold.
    const off = client.economy.onEconomyEvent(() => read());
    return () => {
      cancelled = true;
      off();
    };
  }, [client, resetNonce]);

  return balance;
}
