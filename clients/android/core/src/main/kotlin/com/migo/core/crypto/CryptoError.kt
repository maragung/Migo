package com.migo.core.crypto

/**
 * Variant names, shared with the Rust crate and the TypeScript client.
 *
 * `shared/protocol/vectors/crypto/*.json` names the expected failure for a case by one of these
 * words, and all three implementations have to answer with the same one or the vector is not
 * testing agreement.
 */
enum class CryptoErrorKind {
    BadLength,
    InvalidPublicKey,
    BadSignature,
    DecryptionFailed,
    NoSession,
    ChainGapTooLarge,
    KeyAlreadyUsed,
    MalformedHeader,
    PasswordHash,
    InvalidPrekeyBundle,
}

/**
 * A cryptographic failure.
 *
 * Nothing here carries key material, plaintext, ciphertext or a tag. These errors are produced
 * while processing attacker-supplied bytes, they end up in logs, and a log line is not a place to
 * put a decryption failure's inputs. It also removes the temptation to write a message that
 * distinguishes "wrong tag" from "wrong key", which is a padding oracle by another name.
 */
class CryptoError private constructor(
    val kind: CryptoErrorKind,
    message: String,
    /** Numbers and static strings only — never caller text, never secret bytes. */
    val detail: Map<String, Any>,
) : Exception(message) {
    companion object {
        /** A length that is structurally wrong — a 31-byte key, a 12-byte XChaCha nonce. */
        fun badLength(what: String, expected: Int, actual: Int): CryptoError =
            CryptoError(
                CryptoErrorKind.BadLength,
                "$what must be $expected bytes, got $actual",
                mapOf("what" to what, "expected" to expected, "actual" to actual),
            )

        /** A public key that is not a valid point, or is a small-order point. */
        fun invalidPublicKey(): CryptoError =
            CryptoError(CryptoErrorKind.InvalidPublicKey, "public key is not usable", emptyMap())

        /** A MAC or signature did not verify. */
        fun badSignature(): CryptoError =
            CryptoError(CryptoErrorKind.BadSignature, "signature does not verify", emptyMap())

        /**
         * An AEAD open failed.
         *
         * Deliberately one error for every cause: wrong key, wrong nonce, edited ciphertext,
         * edited associated data. Telling them apart is exactly what an attacker wants, and a
         * receiver's action is the same in every case — drop the message.
         */
        fun decryptionFailed(): CryptoError =
            CryptoError(CryptoErrorKind.DecryptionFailed, "message failed to decrypt", emptyMap())

        /** No ratchet session for this peer and device. */
        fun noSession(): CryptoError =
            CryptoError(CryptoErrorKind.NoSession, "no session for this peer", emptyMap())

        /** A chain gap larger than the skipped-key window allows. */
        fun chainGapTooLarge(): CryptoError =
            CryptoError(CryptoErrorKind.ChainGapTooLarge, "chain gap is too large to close", emptyMap())

        /** A one-time prekey or message key that has already been consumed. */
        fun keyAlreadyUsed(): CryptoError =
            CryptoError(CryptoErrorKind.KeyAlreadyUsed, "key has already been used", emptyMap())

        /** A ratchet or sender-key header that does not parse. */
        fun malformedHeader(): CryptoError =
            CryptoError(CryptoErrorKind.MalformedHeader, "header is malformed", emptyMap())

        /** Password hashing failed. */
        fun passwordHash(): CryptoError =
            CryptoError(CryptoErrorKind.PasswordHash, "password hashing failed", emptyMap())

        /** A prekey bundle that is incomplete or badly signed. */
        fun invalidPrekeyBundle(): CryptoError =
            CryptoError(CryptoErrorKind.InvalidPrekeyBundle, "prekey bundle is not usable", emptyMap())
    }
}
