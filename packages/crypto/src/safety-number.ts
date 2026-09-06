/**
 * Safety numbers: the readable form of a device's identity fingerprint, and the *pair* number two
 * people compare to verify a conversation (§47, §164).
 *
 * A safety number is not a key and protects nothing. What it is, is a *comparison surface*: two
 * people read the same string off their own screens — aloud, in a call, in person — and a match is
 * the out-of-band confirmation that no man-in-the-middle sits between the identity each device
 * actually holds. That only works if every client renders the same bytes as the same string, which
 * is why this module is a byte-for-byte port of the Android client's `SafetyNumber.kt` (the
 * definition's first home — the desktop client renders only an account's *own* fingerprint, and
 * this web port is its first adoption), and why the derivation constants are named and greppable
 * rather than inline. Any other client adopting the pair form must reproduce the salt, the label,
 * the sort, and the `own || peer` input order exactly.
 *
 * # The grouping, and the desktop discrepancy worth recording
 *
 * Thirty-two bytes become eight big-endian 32-bit words; each word is taken modulo 100,000 and
 * padded to five decimal digits; the eight blocks join with single spaces. Forty digits, eight
 * blocks — which is what the desktop client's code has always produced, though its doc comment
 * claims twelve blocks and sixty digits. This port matches the code, not the comment, because the
 * code is what a person may already have read aloud off a desktop machine, and a safety number that
 * quietly changes between clients is worse than no safety number at all.
 *
 * # The pair number, and why the sort is unsigned
 *
 * {@link pairFingerprint} hashes *both* sides' fingerprints in one input, sorted by unsigned byte
 * order, so the number is symmetric: both people derive the same string from their own side of the
 * conversation. The sort is an explicit unsigned byte walk (see {@link precedes}) rather than any
 * shortcut the language offers, because a fingerprint whose first differing byte is above `0x7f`
 * would otherwise sort differently on the two sides, and a pair number the two parties derive
 * differently is a verification that can never succeed.
 */

import { derive } from './kdf.js';
import { CryptoError } from './errors.js';

/** Length of an identity fingerprint: the input {@link safetyNumber} groups for display. */
export const FINGERPRINT_LEN = 32;

/**
 * The salt for the pair safety number. The same salt the contact fingerprint uses, so both live in
 * one KDF family; the label below is what keeps them from ever meaning the same thing.
 */
const PAIR_SALT = 'migo-fingerprint';

/** The HKDF label for the pair safety number. */
const PAIR_LABEL = 'migo-safety-number-v1';

/**
 * Groups a 32-byte identity fingerprint into the readable safety-number form: five-digit blocks
 * joined by single spaces.
 *
 * Four bytes become one big-endian word, the value is taken modulo 100,000, and the remainder is
 * padded to five decimal digits: thirty-two bytes, eight blocks, forty digits. The word is built
 * with plain unsigned arithmetic — `value * 256 + byte` — because every other spelling in
 * JavaScript (`<<` on a number wider than 31 bits, a `DataView` read) has a signed or
 * floating-point edge that would make one block in a million render differently from the other
 * clients, and a safety number that differs in one digit is a mismatch, whatever caused it.
 *
 * @throws {CryptoError} `BadLength` if the fingerprint is not exactly 32 bytes.
 */
export function safetyNumber(fingerprint: Uint8Array): string {
  if (fingerprint.length !== FINGERPRINT_LEN) {
    throw CryptoError.badLength('fingerprint', FINGERPRINT_LEN, fingerprint.length);
  }
  const blocks: string[] = [];
  for (let index = 0; index < FINGERPRINT_LEN; index += 4) {
    let value = 0;
    for (let offset = 0; offset < 4; offset += 1) {
      value = value * 256 + (fingerprint[index + offset] ?? 0);
    }
    blocks.push(String(value % 100_000).padStart(5, '0'));
  }
  return blocks.join(' ');
}

/**
 * The 32-byte fingerprint of a *pair* — this device's identity and one peer device's, in one value
 * neither side can influence alone.
 *
 * Each party's own fingerprint is fed in the same order on both sides: the two fingerprints are
 * sorted by unsigned byte order, so the number is symmetric and both people read the same string
 * off their own screens, which is the property that makes an aloud comparison meaningful. Sorting
 * the *fingerprints* (rather than the identity keys they came from) keeps this function about the
 * bytes it was handed and lets it stay agnostic of where they came from.
 *
 * @throws {CryptoError} `BadLength` if either fingerprint is not exactly 32 bytes.
 */
export function pairFingerprint(own: Uint8Array, peer: Uint8Array): Uint8Array {
  if (own.length !== FINGERPRINT_LEN) {
    throw CryptoError.badLength('fingerprint', FINGERPRINT_LEN, own.length);
  }
  if (peer.length !== FINGERPRINT_LEN) {
    throw CryptoError.badLength('fingerprint', FINGERPRINT_LEN, peer.length);
  }
  const input = new Uint8Array(FINGERPRINT_LEN * 2);
  if (precedes(peer, own)) {
    input.set(peer, 0);
    input.set(own, FINGERPRINT_LEN);
  } else {
    input.set(own, 0);
    input.set(peer, FINGERPRINT_LEN);
  }
  return derive(input, new TextEncoder().encode(PAIR_SALT), PAIR_LABEL, FINGERPRINT_LEN);
}

/** The pair safety number: {@link pairFingerprint} rendered in the shared grouping. */
export function pairSafetyNumber(own: Uint8Array, peer: Uint8Array): string {
  return safetyNumber(pairFingerprint(own, peer));
}

/**
 * Whether `left` sorts before `right` by unsigned byte order, with equality resolving to false.
 *
 * Written as an explicit walk because the language offers no correct shortcut: `Uint8Array` has no
 * lexical compare, and a generic comparator over `number` values would have to be built anyway. The
 * comparison is unsigned on purpose — a fingerprint whose first differing byte is above `0x7f` must
 * sort the same way on every client or the two parties would feed the KDF different inputs and
 * derive different "shared" numbers.
 */
function precedes(left: Uint8Array, right: Uint8Array): boolean {
  for (let index = 0; index < left.length; index += 1) {
    const lhs = left[index] ?? 0;
    const rhs = right[index] ?? 0;
    if (lhs !== rhs) {
      return lhs < rhs;
    }
  }
  return false;
}
