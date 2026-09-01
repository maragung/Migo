/**
 * The `.migo` container: the whole account, portable and encrypted.
 *
 * # What the file is
 *
 * One root secret is the entire account, so one sealed blob is the entire backup: the root, a
 * format version, and a creation timestamp, encrypted under a key derived from a *recovery
 * credential* the user chose — not their password, not their e-mail, not their Google account
 * (§182). The file is named `.migo`, copied to a cloud drive or a USB stick by the user, and holds
 * only ciphertext: a container in a cloud bucket is Argon2id work for whoever steals the bucket,
 * and nothing else.
 *
 * # The header
 *
 * ```text
 * "MIGOACCT1"  9 bytes magic
 * u16 BE       format version (1)
 * u16 BE       crypto version (1)
 * u8           KDF id (1 = Argon2id)
 * u32 BE       Argon2id memory cost, KiB
 * u32 BE       Argon2id time cost, passes
 * u32 BE       Argon2id lanes
 * 16 bytes     Argon2id salt
 * 24 bytes     XChaCha20-Poly1305 nonce
 * remainder    sealed body: ciphertext || tag
 * ```
 *
 * 66 bytes of header, and the whole header is the AEAD's associated data: editing the salt,
 * lowering the stored cost, or swapping a nonce between files breaks the tag before any of it is
 * used. The Argon2id parameters ride in the file (big-endian, a cross-port format both ports read
 * the same way) so raising the cost for new containers never locks out an old one.
 *
 * # The key schedule
 *
 * The recovery credential is stretched by Argon2id at the header's parameters, then the 32-byte
 * result goes through HKDF under the `MIGO/BACKUP/V1` label before it encrypts anything. The extra
 * HKDF step costs nothing and keeps the promise that the root's own domain labels are the only
 * derivations in the system: the backup key is a *cousin* of the Argon2 output, not the Argon2
 * output itself, so a hypothetical weakness in one never lands directly on the other.
 *
 * # One error for everything
 *
 * A wrong credential, a tampered byte, and a truncated file all fail with
 * {@link AccountError.openFailed} — the container reader cannot distinguish them, so it must not
 * tell the caller which happened (§182). The only distinct errors are the ones that name a
 * *remedy*: a newer format version means "update the app", an unknown KDF id means "this file is
 * from a future build", and parameters out of range mean the header was never written by this code.
 *
 * # Async, where Rust is not
 *
 * Argon2id comes from `hash-wasm`, whose hashing is `Promise`-based, so {@link sealContainer} and
 * {@link openContainer} are async. Nothing else here is: the header is built synchronously and the
 * AEAD is the same synchronous XChaCha20-Poly1305 the rest of `@migo/crypto` uses.
 */

import { equalBytes, hexToBytes } from '@noble/ciphers/utils.js';
import { bytesToHex } from '@noble/ciphers/utils.js';
import { argon2id } from 'hash-wasm';

import { NONCE_LEN as AEAD_NONCE_LEN, open, sealWithNonce, SymmetricKey } from '../aead.js';
import { derive } from '../kdf.js';
import { randomBytes } from '../random.js';
import { AccountError } from './errors.js';
import { DOMAIN_BACKUP, MigoRoot, ROOT_LEN } from './root.js';

const ENCODER = new TextEncoder();

/** The container magic. The trailing digit is the format generation. */
const MAGIC = ENCODER.encode('MIGOACCT1');
/** The format version this build writes and reads. */
export const FORMAT_VERSION = 1;
/** The crypto version this build writes and reads. Bumped when the key schedule or AEAD changes. */
export const CRYPTO_VERSION = 1;
/** Argon2id, the only KDF id this build understands. */
export const KDF_ARGON2ID = 1;

/** Argon2id salt length in bytes. */
export const SALT_LEN = 16;
/** The AEAD nonce length in bytes (XChaCha20-Poly1305). */
export const NONCE_LEN = AEAD_NONCE_LEN;
/** Total header length: magic, two versions, KDF id, three cost words, salt, nonce. */
export const HEADER_LEN = MAGIC.length + 2 + 2 + 1 + 4 + 4 + 4 + SALT_LEN + NONCE_LEN;

/** Argon2id memory cost for new containers, in KiB: 64 MiB, matching the desktop vault. */
export const MEMORY_KIB = 64 * 1024;
/** Argon2id passes for new containers. */
export const TIME_COST = 3;
/** Argon2id lanes for new containers. */
export const LANES = 1;

/** Shortest recovery credential accepted (bytes). Length is the rule; composition rules push people towards dictionary words. */
export const MIN_CREDENTIAL_BYTES = 8;
/** Longest recovery credential accepted (bytes), so a pasted file cannot turn one open into a minute of hashing. */
export const MAX_CREDENTIAL_BYTES = 1024;

/**
 * The Argon2id parameters, read from a header or chosen for a new container.
 *
 * The Rust reference is a `Copy` struct with an associated `CURRENT` constant and a `validate`
 * method; this is the same shape as a class, with {@link ContainerParams.current} for the constant.
 */
export class ContainerParams {
  /** Memory cost, KiB. */
  readonly memoryKib: number;
  /** Time cost, passes. */
  readonly timeCost: number;
  /** Lanes. */
  readonly lanes: number;

  constructor(memoryKib: number, timeCost: number, lanes: number) {
    this.memoryKib = memoryKib;
    this.timeCost = timeCost;
    this.lanes = lanes;
  }

  /** The parameters new containers are sealed with. */
  static current(): ContainerParams {
    return new ContainerParams(MEMORY_KIB, TIME_COST, LANES);
  }

  /**
   * Rejects parameters this build will not spend memory on, and returns itself so a caller can
   * write `params.validate()` in an expression.
   *
   * A stored cost is attacker-controlled input in the sense that anyone who can write the file can
   * set it. The tag over the header already stops a silent downgrade, but a floor here means a
   * hostile container naming 4 GiB of Argon2 memory is refused *before* the allocation.
   *
   * @throws {AccountError} `KdfOutOfRange` outside 8 MiB..4 GiB of memory, or passes/lanes outside
   * `1..=16`.
   */
  validate(): ContainerParams {
    const sane =
      this.memoryKib >= 8 * 1024 &&
      this.memoryKib <= 4 * 1024 * 1024 &&
      this.timeCost >= 1 &&
      this.timeCost <= 16 &&
      this.lanes >= 1 &&
      this.lanes <= 16;
    if (!sane) {
      throw AccountError.kdfOutOfRange();
    }
    return this;
  }
}

/**
 * The decrypted container payload: everything a new device needs to become the account again.
 *
 * Deliberately small. The root is the account; metadata exists so a future reader can tell what it
 * is holding without decrypting the whole history of format changes. Wallet addresses and device
 * lists are *not* here — they are functions of the root or live on the server, and duplicating
 * them into the backup would create a second copy that can drift from the first.
 */
export class AccountFile {
  /** The account payload format version. */
  readonly version: number;
  /** When this container was sealed, Unix seconds. Display material, not security material. */
  readonly createdAt: number;
  /** The root secret, hex-encoded: 64 characters. The only secret in the file. */
  readonly root: string;
  /**
   * The account's server-side id, when the sealing device knew it.
   *
   * Deliberately the *last* field and deliberately optional — containers sealed before the field
   * existed, and the conformance vectors that pin this file's bytes, serialise exactly the three
   * fields above, and {@link AccountFile.toJsonBytes} omits it when absent so those bytes do not
   * move. A restoring device that finds it absent cannot run the add-device ceremony and says so,
   * rather than guessing at an account.
   */
  readonly accountId: string | undefined;

  private constructor(
    version: number,
    createdAt: number,
    root: string,
    accountId: string | undefined,
  ) {
    this.version = version;
    this.createdAt = createdAt;
    this.root = root;
    this.accountId = accountId;
  }

  /** Builds a payload for `root`, stamped `now` (Unix seconds), with no account id. */
  static forRoot(root: MigoRoot, now: number): AccountFile {
    return new AccountFile(FORMAT_VERSION, now, bytesToHex(root.asBytes()), undefined);
  }

  /** Names the account this container restores, returning a copy with the id set. */
  forAccount(accountId: string): AccountFile {
    return new AccountFile(this.version, this.createdAt, this.root, accountId);
  }

  /**
   * The root secret.
   *
   * @throws {AccountError} `BadLength` if the hex does not decode to 32 bytes, which for a payload
   * that passed the AEAD tag means the container was written by something else that shares the
   * format.
   */
  rootSecret(): MigoRoot {
    let decoded: Uint8Array;
    try {
      decoded = hexToBytes(this.root);
    } catch {
      throw AccountError.badLength('container root', ROOT_LEN, Math.floor(this.root.length / 2));
    }
    return MigoRoot.fromBytes(decoded);
  }

  /**
   * The compact JSON bytes serde produces, byte for byte: keys `version`, `created_at`, `root`,
   * and `account_id` only when present, in that order, with no whitespace.
   */
  toJsonBytes(): Uint8Array {
    const object: { version: number; created_at: number; root: string; account_id?: string } = {
      version: this.version,
      created_at: this.createdAt,
      root: this.root,
    };
    // `skip_serializing_if = "Option::is_none"`: the field is written only when set, so the
    // three-field form the vectors pin is not moved by one byte.
    if (this.accountId !== undefined) {
      object.account_id = this.accountId;
    }
    return ENCODER.encode(JSON.stringify(object));
  }

  /**
   * Parses the payload, requiring the three mandatory fields and accepting the optional account id.
   *
   * Unknown keys are ignored, matching serde's default and the forward-compatibility rule every
   * port follows. A structural problem throws {@link AccountError.openFailed}, because a payload
   * that decrypted but is not a readable account is indistinguishable from a wrong credential as
   * far as any caller needs to know — the root hex is validated separately, by
   * {@link AccountFile.rootSecret}.
   */
  static fromJsonBytes(bytes: Uint8Array): AccountFile {
    let parsed: unknown;
    try {
      parsed = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
    } catch {
      throw AccountError.openFailed();
    }
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
      throw AccountError.openFailed();
    }
    const object = parsed as Record<string, unknown>;

    const version = object.version;
    if (
      typeof version !== 'number' ||
      !Number.isInteger(version) ||
      version < 0 ||
      version > 0xffff
    ) {
      throw AccountError.openFailed();
    }
    const createdAt = object.created_at;
    if (typeof createdAt !== 'number' || !Number.isInteger(createdAt) || createdAt < 0) {
      throw AccountError.openFailed();
    }
    const root = object.root;
    if (typeof root !== 'string') {
      throw AccountError.openFailed();
    }
    const rawAccountId = object.account_id;
    let accountId: string | undefined;
    if (rawAccountId === undefined || rawAccountId === null) {
      accountId = undefined;
    } else if (typeof rawAccountId === 'string') {
      accountId = rawAccountId;
    } else {
      throw AccountError.openFailed();
    }
    return new AccountFile(version, createdAt, root, accountId);
  }
}

/**
 * Seals an account into container bytes with fresh salt and nonce.
 *
 * @throws {AccountError} `BadLength` if the credential is outside the accepted byte range, and
 * whatever {@link sealContainerWith} reports otherwise.
 */
export async function sealContainer(credential: string, file: AccountFile): Promise<Uint8Array> {
  const salt = randomBytes(SALT_LEN);
  const nonce = randomBytes(NONCE_LEN);
  return sealContainerWith(credential, file, ContainerParams.current(), salt, nonce);
}

/**
 * Seals with caller-supplied salt and nonce: the deterministic form the conformance vectors use.
 * Application code wants {@link sealContainer}, whose random salt and nonce make every container
 * unique even for the identical account and credential.
 *
 * @throws {AccountError} `BadLength` for a bad credential or a salt/nonce of the wrong width,
 * `KdfOutOfRange` for parameters out of range.
 */
export async function sealContainerWith(
  credential: string,
  file: AccountFile,
  params: ContainerParams,
  salt: Uint8Array,
  nonce: Uint8Array,
): Promise<Uint8Array> {
  checkCredential(credential);
  const validated = params.validate();
  if (salt.length !== SALT_LEN) {
    throw AccountError.badLength('container salt', SALT_LEN, salt.length);
  }
  if (nonce.length !== NONCE_LEN) {
    throw AccountError.badLength('container nonce', NONCE_LEN, nonce.length);
  }

  const header = buildHeader(validated, salt, nonce);
  const key = await containerKey(credential, salt, validated);
  const plaintext = file.toJsonBytes();
  // `sealWithNonce` returns nonce || ciphertext || tag, which is the body this format stores:
  // readers hand the whole body to `open`.
  const body = sealWithNonce(key, nonce, header, plaintext);
  key.destroy();
  plaintext.fill(0);

  const out = new Uint8Array(header.length + body.length);
  out.set(header, 0);
  out.set(body, header.length);
  return out;
}

/**
 * Opens a container: verifies the header, derives the key, decrypts, and returns the account.
 *
 * @throws {AccountError} `NotAContainer` for a file that is not one (wrong magic or shorter than a
 * header). `UnsupportedVersion` for a format or crypto version this build does not read.
 * `UnknownKdf` for a KDF id this build does not implement. `KdfOutOfRange` for header parameters
 * out of range. `OpenFailed` for everything else — wrong credential, tampered bytes, or a payload
 * that is not an account — without saying which. `BadLength` if the decrypted root hex is not 32
 * bytes.
 */
export async function openContainer(credential: string, bytes: Uint8Array): Promise<AccountFile> {
  checkCredential(credential);
  if (bytes.length < HEADER_LEN) {
    throw AccountError.notAContainer();
  }
  const header = bytes.subarray(0, HEADER_LEN);
  const body = bytes.subarray(HEADER_LEN);
  if (!equalBytes(header.subarray(0, MAGIC.length), MAGIC)) {
    throw AccountError.notAContainer();
  }

  const view = new DataView(header.buffer, header.byteOffset, header.byteLength);
  let cursor = MAGIC.length;
  const formatVersion = view.getUint16(cursor, false);
  cursor += 2;
  const cryptoVersion = view.getUint16(cursor, false);
  cursor += 2;
  if (formatVersion !== FORMAT_VERSION || cryptoVersion !== CRYPTO_VERSION) {
    // A container from a future build: refuse rather than guess at what its fields mean. Guessing
    // wrong at this layer can corrupt the only copy of someone's account.
    throw AccountError.unsupportedVersion(Math.max(formatVersion, cryptoVersion), FORMAT_VERSION);
  }
  const kdfId = header[cursor] ?? 0;
  cursor += 1;
  if (kdfId !== KDF_ARGON2ID) {
    throw AccountError.unknownKdf(kdfId);
  }
  const memoryKib = view.getUint32(cursor, false);
  cursor += 4;
  const timeCost = view.getUint32(cursor, false);
  cursor += 4;
  const lanes = view.getUint32(cursor, false);
  cursor += 4;
  const params = new ContainerParams(memoryKib, timeCost, lanes).validate();
  const salt = header.subarray(cursor, cursor + SALT_LEN);
  // The header's nonce is advisory for a reader that parses it field by field; the body carries
  // the authoritative copy as its prefix, and the two must agree or the tag fails — swapping a
  // header between files is the attack that arrangement closes.

  const key = await containerKey(credential, salt, params);
  let plaintext: Uint8Array;
  try {
    plaintext = open(key, header, body);
  } catch {
    key.destroy();
    // Every cause collapses to one error: wrong key, wrong nonce, edited ciphertext, edited header.
    throw AccountError.openFailed();
  }
  key.destroy();

  let file: AccountFile;
  try {
    file = AccountFile.fromJsonBytes(plaintext);
  } catch {
    plaintext.fill(0);
    throw AccountError.openFailed();
  }
  plaintext.fill(0);
  // A payload that decrypted but carries a root that is not 32 bytes is a `BadLength`, propagated
  // rather than folded into `OpenFailed`: the tag passed, so this is a container from something
  // else that shares the format, not a wrong-credential guess.
  file.rootSecret();
  return file;
}

/** Builds the 66-byte header. */
function buildHeader(params: ContainerParams, salt: Uint8Array, nonce: Uint8Array): Uint8Array {
  const header = new Uint8Array(HEADER_LEN);
  const view = new DataView(header.buffer, header.byteOffset, header.byteLength);
  let offset = 0;
  header.set(MAGIC, offset);
  offset += MAGIC.length;
  view.setUint16(offset, FORMAT_VERSION, false);
  offset += 2;
  view.setUint16(offset, CRYPTO_VERSION, false);
  offset += 2;
  header[offset] = KDF_ARGON2ID;
  offset += 1;
  view.setUint32(offset, params.memoryKib, false);
  offset += 4;
  view.setUint32(offset, params.timeCost, false);
  offset += 4;
  view.setUint32(offset, params.lanes, false);
  offset += 4;
  header.set(salt, offset);
  offset += SALT_LEN;
  header.set(nonce, offset);
  return header;
}

/** Argon2id, then HKDF under the backup domain label. */
async function containerKey(
  credential: string,
  salt: Uint8Array,
  params: ContainerParams,
): Promise<SymmetricKey> {
  let stretched: Uint8Array;
  try {
    stretched = await argon2id({
      password: ENCODER.encode(credential),
      salt,
      iterations: params.timeCost,
      parallelism: params.lanes,
      memorySize: params.memoryKib,
      hashLength: 32,
      outputType: 'binary',
    });
  } catch {
    throw AccountError.openFailed();
  }
  const derived = derive(stretched, null, DOMAIN_BACKUP, 32);
  stretched.fill(0);
  const key = SymmetricKey.fromBytes(derived);
  derived.fill(0);
  return key;
}

/** Enforces the recovery-credential byte-length window, both ends. */
function checkCredential(credential: string): void {
  const length = ENCODER.encode(credential).length;
  if (length < MIN_CREDENTIAL_BYTES || length > MAX_CREDENTIAL_BYTES) {
    throw AccountError.badLength('recovery credential', MIN_CREDENTIAL_BYTES, length);
  }
}
