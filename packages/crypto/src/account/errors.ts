/**
 * The closed set of account-root failures.
 *
 * The mirror of {@link module:errors}'s `CryptoError`, for the account crate rather than the
 * message crate: one class, one `kind`, and the kind strings are the Rust `AccountError` variant
 * names in `server/crates/migo-account/src/error.rs`. A conformance vector that expects a failure
 * names it by that word, so both implementations answer with the same string or the vector is not
 * testing agreement.
 *
 * The policy is deliberately vague about *why* a credential or a container failed: a reader that
 * distinguishes "wrong recovery credential" from "file was tampered with" hands an attacker a free
 * oracle for how far a guess got. A wrong credential, a flipped byte, and a truncated file all
 * surface as {@link AccountError.openFailed} — §182. The only distinct failures are the ones that
 * name a *remedy*: a newer version means "update the app", an unknown KDF id means "this file is
 * from a future build", and parameters out of range mean the header was never written by this code.
 */

/** Variant names, shared with the Rust crate. */
export type AccountErrorKind =
  | 'BadLength'
  | 'NotAContainer'
  | 'UnsupportedVersion'
  | 'UnknownKdf'
  | 'OpenFailed'
  | 'BadSignature'
  | 'InvalidDerivation'
  | 'KdfOutOfRange'
  | 'ChainMismatch'
  | 'NotATransaction'
  | 'MalformedRlp'
  | 'BadAddress'
  | 'AddressChecksumFailed';

/** Numbers and static strings only — never a secret, a credential, or a decrypted byte. */
export type AccountErrorDetail = Readonly<Record<string, number | string>>;

/** An account-root operation failed. */
export class AccountError extends Error {
  readonly kind: AccountErrorKind;
  readonly detail: AccountErrorDetail;

  constructor(kind: AccountErrorKind, message: string, detail: AccountErrorDetail = {}) {
    super(message);
    this.name = 'AccountError';
    this.kind = kind;
    this.detail = detail;
  }

  /**
   * A key, seed, signature, or container had the wrong length.
   *
   * `what` is a static string chosen at the call site, never caller text, and the numbers are
   * lengths — safe to state, because they are visible to whoever supplied the value.
   */
  static badLength(what: string, expected: number, actual: number): AccountError {
    return new AccountError('BadLength', `${what} must be ${expected} bytes, got ${actual}`, {
      what,
      expected,
      actual,
    });
  }

  /** A `.migo` container is not one: wrong magic, or shorter than a header. */
  static notAContainer(): AccountError {
    return new AccountError('NotAContainer', 'not a Migo account container');
  }

  /**
   * The container's format or crypto version is newer than this build understands.
   *
   * Named precisely because the honest remedy is "update the app", not "try another credential".
   */
  static unsupportedVersion(found: number, supported: number): AccountError {
    return new AccountError(
      'UnsupportedVersion',
      `container version ${found} is not supported (this build reads ${supported})`,
      { found, supported },
    );
  }

  /** The container's key-derivation id is not one this build implements. */
  static unknownKdf(found: number): AccountError {
    return new AccountError('UnknownKdf', `unknown key derivation id ${found}`, { found });
  }

  /**
   * Decryption failed: wrong recovery credential, tampered file, or both.
   *
   * Deliberately one error for every cause, and the caller is told neither which nor how far a
   * guess got — the container reader cannot distinguish them, so it must not pretend to.
   */
  static openFailed(): AccountError {
    return new AccountError('OpenFailed', 'container could not be opened');
  }

  /** An ML-DSA signature did not verify, or a key or signature did not decode. */
  static badSignature(): AccountError {
    return new AccountError('BadSignature', 'identity signature verification failed');
  }

  /**
   * A BIP-32 derivation step produced an invalid scalar (zero or ≥ the curve order).
   *
   * BIP-32 assigns this probability ~2^-127, so in practice it means the input was not a seed this
   * construction should be applied to.
   */
  static invalidDerivation(): AccountError {
    return new AccountError('InvalidDerivation', 'invalid derivation step');
  }

  /** The Argon2id parameters in a header are outside the range this build will spend memory on. */
  static kdfOutOfRange(): AccountError {
    return new AccountError('KdfOutOfRange', 'container KDF parameters are out of range');
  }

  /**
   * An RPC-observed chain id does not match the configured network. The transaction was never
   * built: a chain-id mismatch is the replay/confusion case, and the honest response is to close
   * the session, not to pick one of the two ids.
   */
  static chainMismatch(configured: number, observed: number): AccountError {
    return new AccountError(
      'ChainMismatch',
      `chain id mismatch: configured ${configured}, RPC reported ${observed}`,
      { configured, observed },
    );
  }

  /** Bytes handed to the transaction parser are not an EIP-1559 envelope at all. */
  static notATransaction(): AccountError {
    return new AccountError('NotATransaction', 'not an EIP-1559 transaction');
  }

  /**
   * A raw transaction or RLP item is structurally broken or non-canonical. The parser is
   * deliberately strict — trailing bytes, non-minimal integers, and redundant length prefixes are
   * all refused, because it parses bytes that arrived over a network.
   *
   * `what` is a static string chosen at the call site, never caller text.
   */
  static malformedRlp(what: string): AccountError {
    return new AccountError('MalformedRlp', `malformed RLP: ${what}`, { what });
  }

  /** A recipient string is not an address: wrong length or not hex. */
  static badAddress(): AccountError {
    return new AccountError('BadAddress', 'not a valid address');
  }

  /**
   * A mixed-case address string's EIP-55 checksum does not match its contents. Reported distinctly
   * from {@link AccountError.badAddress} because the user's remedy is "fix the typo", not "the app
   * is broken".
   */
  static addressChecksumFailed(): AccountError {
    return new AccountError('AddressChecksumFailed', 'address checksum failed');
  }
}
