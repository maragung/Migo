package com.migo.core.account

/**
 * Variant names for account-root failures, shared with the Rust crate's `AccountError`.
 *
 * The split mirrors the reference implementation's reasoning: the errors that carry a *remedy*
 * (a newer format version means "update the app", an unknown KDF means "this file is from a
 * future build") are named, and everything an attacker could grind against — a wrong recovery
 * credential, a tampered byte, a truncated file — is one refusal, [OpenFailed], with no hint
 * about which failed.
 */
enum class AccountErrorKind {
    BadLength,
    InvalidDerivation,
    BadSignature,
    NotAContainer,
    UnsupportedVersion,
    UnknownKdf,
    KdfOutOfRange,
    OpenFailed,
    ChainMismatch,
    NotATransaction,
    MalformedRlp,
    BadAddress,
    AddressChecksumFailed,
}

/**
 * An account-root failure.
 *
 * Same hygiene as [com.migo.core.crypto.CryptoError]: no key material, no plaintext, no caller
 * text in the message, nothing that distinguishes a wrong credential from an edited container.
 */
class AccountError private constructor(
    val kind: AccountErrorKind,
    message: String,
    /** Numbers and static strings only. */
    val detail: Map<String, Any>,
) : Exception(message) {
    companion object {
        /** A length that is structurally wrong — a 31-byte root, a 17-byte salt. */
        fun badLength(what: String, expected: Int, actual: Int): AccountError =
            AccountError(
                AccountErrorKind.BadLength,
                "$what must be $expected bytes, got $actual",
                mapOf("what" to what, "expected" to expected, "actual" to actual),
            )

        /** A BIP-32 step landed on an invalid scalar. Unreachable in practice (2^-127). */
        fun invalidDerivation(): AccountError =
            AccountError(AccountErrorKind.InvalidDerivation, "the derivation is invalid", emptyMap())

        /** A signature did not verify or did not decode. */
        fun badSignature(): AccountError =
            AccountError(AccountErrorKind.BadSignature, "signature does not verify", emptyMap())

        /** Too short for a header, or the magic is wrong. */
        fun notAContainer(): AccountError =
            AccountError(AccountErrorKind.NotAContainer, "this file is not a .migo container", emptyMap())

        /** A container from a build newer than this one. */
        fun unsupportedVersion(found: Int, supported: Int): AccountError =
            AccountError(
                AccountErrorKind.UnsupportedVersion,
                "the container was written by a newer build (version $found, this build reads $supported)",
                mapOf("found" to found, "supported" to supported),
            )

        /** A KDF identifier this build does not implement. */
        fun unknownKdf(found: Int): AccountError =
            AccountError(
                AccountErrorKind.UnknownKdf,
                "the container uses a key derivation this build does not know (id $found)",
                mapOf("found" to found),
            )

        /** Header parameters this build refuses to spend memory on. */
        fun kdfOutOfRange(): AccountError =
            AccountError(
                AccountErrorKind.KdfOutOfRange,
                "the container's key-derivation parameters are out of range",
                emptyMap(),
            )

        /**
         * Wrong credential, tampered bytes, truncated file, or a payload that is not an account.
         * Deliberately one error for all of them.
         */
        fun openFailed(): AccountError =
            AccountError(AccountErrorKind.OpenFailed, "the container did not open", emptyMap())

        /**
         * An RPC-observed chain id does not match the configured network. The transaction was
         * never built: a chain-id mismatch is the replay/confusion case, and the honest response
         * is to close the session, not to pick one of the two ids.
         */
        fun chainMismatch(configured: Long, observed: Long): AccountError =
            AccountError(
                AccountErrorKind.ChainMismatch,
                "chain id mismatch: configured $configured, RPC reported $observed",
                mapOf("configured" to configured, "observed" to observed),
            )

        /** Bytes handed to the transaction parser are not an EIP-1559 envelope at all. */
        fun notATransaction(): AccountError =
            AccountError(AccountErrorKind.NotATransaction, "not an EIP-1559 transaction", emptyMap())

        /**
         * A raw transaction or RLP item is structurally broken or non-canonical. The parser is
         * deliberately strict — trailing bytes, non-minimal integers and redundant length
         * prefixes are all refused, because it parses bytes that arrived over a network.
         */
        fun malformedRlp(what: String): AccountError =
            AccountError(AccountErrorKind.MalformedRlp, "malformed RLP: $what", mapOf("what" to what))

        /** A recipient string is not an address: wrong length or not hex. */
        fun badAddress(): AccountError =
            AccountError(AccountErrorKind.BadAddress, "not a valid address", emptyMap())

        /**
         * A mixed-case address string's EIP-55 checksum does not match its contents. Distinct from
         * [badAddress] because the user's remedy is "fix the typo", not "the app is broken".
         */
        fun addressChecksumFailed(): AccountError =
            AccountError(AccountErrorKind.AddressChecksumFailed, "address checksum failed", emptyMap())
    }
}
