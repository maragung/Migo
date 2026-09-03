/**
 * The closed set of cryptographic failures.
 *
 * One class, one `kind`, and the kind strings are the Rust `CryptoError` variant names —
 * `shared/protocol/vectors/crypto/*.json` names the expected failure for a case, and both
 * implementations have to answer with the same word or the vector is not testing agreement.
 *
 * Nothing here carries key material, plaintext, ciphertext or a tag. The reasoning is the
 * same as in `@migo/wire`: these errors are produced while processing attacker-supplied
 * bytes, they end up in logs, and a log line is not a place to put a decryption failure's
 * inputs. It also removes the temptation to write an error message that distinguishes
 * "wrong tag" from "wrong key", which is a padding-oracle by another name.
 */

/** Variant names, shared with the Rust crate. */
export type CryptoErrorKind =
  | 'BadLength'
  | 'InvalidPublicKey'
  | 'BadSignature'
  | 'DecryptionFailed'
  | 'NoSession'
  | 'ChainGapTooLarge'
  | 'KeyAlreadyUsed'
  | 'MalformedHeader'
  | 'PassphraseHash'
  | 'InvalidPrekeyBundle';

/** Numbers and static strings only. */
export type CryptoErrorDetail = Readonly<Record<string, number | string>>;

/** A cryptographic failure. */
export class CryptoError extends Error {
  readonly kind: CryptoErrorKind;
  readonly detail: CryptoErrorDetail;

  constructor(kind: CryptoErrorKind, message: string, detail: CryptoErrorDetail = {}) {
    super(message);
    this.name = 'CryptoError';
    this.kind = kind;
    this.detail = detail;
  }

  /**
   * A length that is structurally wrong — a 31-byte key, a 12-byte XChaCha nonce.
   *
   * `what` is a static string chosen here, never caller text, and the numbers are lengths.
   * Lengths are safe to state: they are visible to whoever supplied the value.
   */
  static badLength(what: string, expected: number, actual: number): CryptoError {
    return new CryptoError('BadLength', `${what} must be ${expected} bytes, got ${actual}`, {
      what,
      expected,
      actual,
    });
  }

  /** A public key that is not a valid point, or is a small-order point. */
  static invalidPublicKey(): CryptoError {
    return new CryptoError('InvalidPublicKey', 'public key is not usable');
  }

  /** A MAC or signature did not verify. */
  static badSignature(): CryptoError {
    return new CryptoError('BadSignature', 'signature does not verify');
  }

  /**
   * An AEAD open failed.
   *
   * Deliberately one error for every cause: wrong key, wrong nonce, edited ciphertext,
   * edited associated data. Telling them apart is exactly the information an attacker
   * wants, and a receiver has the same action in every case — drop the message.
   */
  static decryptionFailed(): CryptoError {
    return new CryptoError('DecryptionFailed', 'message failed to decrypt');
  }

  /** No ratchet session for this peer and device. */
  static noSession(): CryptoError {
    return new CryptoError('NoSession', 'no session for this peer');
  }

  /** A chain gap larger than the skipped-key window allows. */
  static chainGapTooLarge(): CryptoError {
    return new CryptoError('ChainGapTooLarge', 'chain gap is too large to close');
  }

  /** A one-time prekey or message key that has already been consumed. */
  static keyAlreadyUsed(): CryptoError {
    return new CryptoError('KeyAlreadyUsed', 'key has already been used');
  }

  /** A ratchet header that does not parse. */
  static malformedHeader(): CryptoError {
    return new CryptoError('MalformedHeader', 'ratchet header is malformed');
  }

  /** Passphrase hashing failed. */
  static passphraseHash(): CryptoError {
    return new CryptoError('PassphraseHash', 'passphrase hashing failed');
  }

  /** A prekey bundle that is incomplete or badly signed. */
  static invalidPrekeyBundle(): CryptoError {
    return new CryptoError('InvalidPrekeyBundle', 'prekey bundle is not usable');
  }
}
