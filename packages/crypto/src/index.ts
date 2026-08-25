/**
 * `@migo/crypto`: the client half of Migo's cryptographic primitives.
 *
 * Everything here is from audited libraries and matches `server/crates/migo-crypto` byte for byte.
 * Three low-level primitives:
 *
 * * {@link kdf} — HKDF-SHA256, with one label per derivation.
 * * {@link aead} — XChaCha20-Poly1305 over `nonce || ciphertext || tag`.
 * * {@link mac} — HMAC-SHA256 for tokens and cursors the server signs for itself.
 *
 * And the four modules that build the end-to-end message protocol on top of them:
 *
 * * {@link identity} — the long-term Ed25519 and X25519 keys, and the signed prekeys they vouch for.
 * * {@link x3dh} — asynchronous session setup, so a message can be sent to an offline device.
 * * {@link ratchet} — the Double Ratchet: a fresh key per message, forward secrecy, self-healing.
 * * {@link senderKey} — group messaging at O(1) per message instead of once per recipient.
 *
 * Nothing here is written from scratch. ADR-0003 allows audited implementations only, and the
 * reason is narrow and specific: a hand-rolled primitive that is wrong still produces
 * random-looking bytes, still round-trips against itself, and still passes every test a team
 * would think to write. The failure surfaces years later, in someone else's cryptanalysis paper,
 * against messages that were already sent.
 *
 * The modules are re-exported as namespaces so that call sites read the same as the Rust ones —
 * `kdf.derive(...)`, `x3dh.initiate(...)`, `ratchet.RatchetSession.initiator(...)` — while the
 * types named in signatures across the client ({@link SymmetricKey}, {@link IdentitySecret},
 * {@link RatchetSession}, {@link CryptoError}, and the rest) are also available directly.
 */

export * as kdf from './kdf.js';
export * as mac from './mac.js';
export * as aead from './aead.js';
export * as identity from './identity.js';
export * as x3dh from './x3dh.js';
export * as ratchet from './ratchet.js';
export * as senderKey from './sender-key.js';

export { CryptoError } from './errors.js';
export type { CryptoErrorKind, CryptoErrorDetail } from './errors.js';
export { MacKey, TAG_LEN as MAC_TAG_LEN, MIN_TAG_LEN as MAC_MIN_TAG_LEN } from './mac.js';
export { SymmetricKey, KEY_LEN, NONCE_LEN, TAG_LEN as AEAD_TAG_LEN } from './aead.js';
export type { Label } from './kdf.js';

export {
  IdentityPublic,
  IdentitySecret,
  KeyPair,
  SignedPrekey,
  PUBLIC_KEY_LEN,
  IDENTITY_PUBLIC_LEN,
  SIGNATURE_LEN,
  SEED_LEN,
} from './identity.js';

export { PrekeyBundle, SessionSeed, initiate, respond } from './x3dh.js';
export type { OneTimePrekey, InitialMessage, Initiation } from './x3dh.js';

export { RatchetSession, RatchetHeader, MAX_CHAIN_GAP, MAX_SKIPPED_KEYS } from './ratchet.js';

export {
  SenderKeyState,
  ReceiverKeyState,
  SenderKeyDistribution,
  SenderKeyHeader,
  MAX_MESSAGES_PER_CHAIN,
} from './sender-key.js';
export type { SenderKeyMessage } from './sender-key.js';
