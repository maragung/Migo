/**
 * `@migo/crypto`'s account root: one 32-byte secret, five isolated domains.
 *
 * The mirror of `server/crates/migo-account`'s `lib.rs`, re-exporting the same surface so that a
 * call site reads the same in both languages. One root secret is the whole account; it never signs
 * and never decrypts, it is only ever HKDF input under one of five immutable domain labels, and
 * each domain consumes its seed through the standard construction of that domain — FIPS 204 key
 * generation for {@link IdentityKey}, BIP-32/BIP-44 for {@link EvmWallet}, the founding device's
 * X3DH seeds for {@link foundingDeviceE2eeSeeds}, and Argon2id → XChaCha20-Poly1305 for the
 * {@link openContainer | .migo container}. Device credentials are the one thing that is *not*
 * derived — {@link DeviceCredential} is random per device, so a leaked root alone cannot
 * impersonate a registered device (ADR-0013).
 *
 * The three ports (Rust, TypeScript, Kotlin) agree byte for byte, which the conformance vectors in
 * `shared/protocol/vectors/crypto/` pin and `test/account.test.ts` checks.
 */

export { AccountError } from './errors.js';
export type { AccountErrorKind, AccountErrorDetail } from './errors.js';

export {
  MigoRoot,
  foundingDeviceE2eeSeeds,
  ROOT_LEN,
  DOMAIN_IDENTITY,
  DOMAIN_EVM,
  DOMAIN_E2EE,
  DOMAIN_BACKUP,
  DOMAIN_DEVICE,
  LABEL_E2EE_SIGNING,
  LABEL_E2EE_EXCHANGE,
} from './root.js';

export {
  IdentityKey,
  DeviceCredential,
  verifyIdentity,
  IDENTITY_ALGORITHM,
  KEY_VERSION_ONE,
  PUBLIC_KEY_LEN,
  SIGNATURE_LEN,
  SEED_LEN,
  CONTEXT_LOGIN,
  CONTEXT_ROTATE,
  CONTEXT_LOGIN_DEVICE,
} from './identity.js';

export { EvmWallet, eip55, EIP155_COIN_TYPE, EVM_BIP44_PATH } from './evm.js';

export {
  openContainer,
  sealContainer,
  sealContainerWith,
  AccountFile,
  ContainerParams,
  FORMAT_VERSION,
  CRYPTO_VERSION,
  KDF_ARGON2ID,
  SALT_LEN,
  NONCE_LEN,
  HEADER_LEN,
  MEMORY_KIB,
  TIME_COST,
  LANES,
  MIN_CREDENTIAL_BYTES,
  MAX_CREDENTIAL_BYTES,
} from './container.js';
