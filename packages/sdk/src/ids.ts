/**
 * Client-side identifier minting.
 *
 * Most ids a client handles come from the server, already formed. Two do not: the `message_id` on
 * a `MessageSend` and the `action_id` shape of a `GameAction` are chosen by the client so a send can
 * be retried idempotently — the server dedupes on the id it was given rather than minting a second
 * message for a resend. That makes the client a minter, and a minter has to produce the exact 128-bit
 * layout {@link Id} promises: six bytes of big-endian Unix milliseconds then ten random bytes, so ids
 * sort by creation time as both bytes and text and collide only on a 2^80 accident within one
 * millisecond. This mirrors `migo-wire`'s `Id::generate` on the Rust side byte for byte.
 *
 * The randomness comes from the platform CSPRNG (`globalThis.crypto.getRandomValues`), which is
 * present on every target this SDK runs on — browsers, and Node 22 where Web Crypto is global. There
 * is deliberately no `Math.random` fallback: a predictable id is a correctness bug (a guessable
 * `message_id` lets one client's retry collide with another's) and silently degrading to it would
 * hide that.
 */

import { idFromBytes } from '@migo/wire';
import type { Id } from '@migo/wire';

/** Bytes of Unix-millisecond time prefix, matching {@link Id}'s sortable layout. */
const TIME_BYTES = 6;
/** Bytes of the identifier as a whole. */
const ID_BYTES = 16;

/** The platform CSPRNG, resolved once. Throws if the environment has no Web Crypto. */
function randomFill(into: Uint8Array): void {
  const webcrypto = (globalThis as { crypto?: Crypto }).crypto;
  if (webcrypto?.getRandomValues === undefined) {
    throw new TypeError(
      'no Web Crypto available to mint an id; this environment cannot be a client',
    );
  }
  webcrypto.getRandomValues(into);
}

/**
 * Mints a fresh, time-ordered {@link Id}.
 *
 * The time prefix is read from the wall clock at call time; the remaining ten bytes are random. Two
 * ids minted in the same millisecond differ in their random tail, and ids minted in different
 * milliseconds order by time.
 */
export function newId(): Id {
  const bytes = new Uint8Array(ID_BYTES);
  // Big-endian milliseconds in the first six bytes. Number is exact well past year 10000, so the
  // shift arithmetic below stays within safe-integer range.
  let ms = Date.now();
  for (let i = TIME_BYTES - 1; i >= 0; i -= 1) {
    bytes[i] = ms & 0xff;
    ms = Math.floor(ms / 256);
  }
  const random = new Uint8Array(ID_BYTES - TIME_BYTES);
  randomFill(random);
  bytes.set(random, TIME_BYTES);
  return idFromBytes(bytes);
}
