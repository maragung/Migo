/**
 * Key derivation.
 *
 * HKDF-SHA256 ([RFC 5869]) everywhere, and never a bare hash. `SHA256(secret || label)` is a
 * length-extension hazard and gives no domain separation; HKDF's extract-then-expand
 * structure is designed for exactly this job and is what every reviewed protocol uses.
 *
 * Every derivation in Migo passes a distinct `info` label. Two different keys must never come
 * from the same input material with the same label, because that is how a key that protects
 * one thing ends up protecting another. The labels are constants in this module rather than
 * string literals at call sites, so the full set is greppable in one place — and the same set,
 * character for character, as `server/crates/migo-crypto/src/kdf.rs`. A label that differs
 * between the two implementations does not fail to compile; it silently makes every session
 * between a web client and the server undecryptable, which is why
 * `shared/protocol/vectors/crypto/kdf.json` names each one and the test resolves it through
 * these constants.
 *
 * ## Why the labels are strings here and byte slices in Rust
 *
 * Rust can hand out a `&'static [u8]` that nothing can write to. A `Uint8Array` constant can
 * be modified by any caller that receives it — including by accident — and JavaScript has no
 * way to freeze the contents of one. A `string` constant is genuinely immutable, so that is
 * what the labels are, and {@link derive} encodes them as UTF-8 at the call site. Raw bytes
 * are still accepted, for the RFC vectors whose `info` is not printable text.
 *
 * [RFC 5869]: https://www.rfc-editor.org/rfc/rfc5869
 */

import { hkdf } from '@noble/hashes/hkdf.js';
import { sha256 } from '@noble/hashes/sha2.js';

/** Label for the X3DH shared secret. */
export const LABEL_X3DH = 'migo-x3dh-v1';
/** Label for a Double Ratchet root-key step. */
export const LABEL_RATCHET_ROOT = 'migo-ratchet-root-v1';
/** Label for a Double Ratchet chain-key step. */
export const LABEL_RATCHET_CHAIN = 'migo-ratchet-chain-v1';
/** Label for a per-message key derived from a chain key. */
export const LABEL_MESSAGE_KEY = 'migo-message-key-v1';
/** Label for a group sender-key chain step. */
export const LABEL_SENDER_CHAIN = 'migo-sender-chain-v1';
/** Label for a group sender-key per-message key. */
export const LABEL_SENDER_MESSAGE = 'migo-sender-message-v1';
/** Label for the key that encrypts a client-side backup. */
export const LABEL_BACKUP = 'migo-backup-v1';
/** Label for deriving a device-storage key from a recovery key. */
export const LABEL_RECOVERY = 'migo-recovery-v1';

/** A derivation label: one of the constants above, or raw `info` bytes. */
export type Label = string | Uint8Array;

const ENCODER = new TextEncoder();

/** Encodes a label for HKDF's `info` parameter. */
export function labelBytes(label: Label): Uint8Array {
  return typeof label === 'string' ? ENCODER.encode(label) : label;
}

/**
 * Derives `length` bytes from `secret` under `label`.
 *
 * `salt` is `null` rather than an omitted argument so that every call site states which it
 * means. RFC 5869 defines an absent salt as HashLen zero bytes, and the ratchet steps pass the
 * previous root key as salt rather than a random value, so both spellings are load-bearing and
 * neither should be reachable by forgetting a parameter.
 */
export function derive(
  secret: Uint8Array,
  salt: Uint8Array | null,
  label: Label,
  length: number,
): Uint8Array {
  return hkdf(sha256, secret, salt ?? undefined, labelBytes(label), length);
}

/**
 * Derives two keys at once from a single extract step.
 *
 * The ratchet needs a new root key and a new chain key from the same DH output. Deriving them
 * from one expansion, at different offsets, is the standard construction; running HKDF twice
 * with different labels would also work but costs an extra extract for no benefit.
 *
 * This is one expansion of `firstLength + secondLength` bytes, split — not two expansions.
 * Two `derive` calls would make the shorter output a *prefix* of the longer one, so the two
 * halves would share bytes. The vectors assert the concatenation, which is what stops a later
 * "simplification" into two calls from passing.
 */
export function derivePair(
  secret: Uint8Array,
  salt: Uint8Array | null,
  label: Label,
  firstLength: number,
  secondLength: number,
): { first: Uint8Array; second: Uint8Array } {
  const combined = derive(secret, salt, label, firstLength + secondLength);
  // `slice` copies, so clearing the combined buffer cannot reach into the results.
  const first = combined.slice(0, firstLength);
  const second = combined.slice(firstLength);
  combined.fill(0);
  return { first, second };
}
