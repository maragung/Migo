package com.migo.core.crypto

import org.bouncycastle.crypto.digests.KeccakDigest

/**
 * Keccak-256, the original Keccak padding, not SHA3-256.
 *
 * Ethereum's address scheme predates the SHA-3 standard's changed padding by two years, so every
 * EVM tool in existence hashes with the original. Using SHA3-256 here would derive
 * plausible-looking addresses that no chain and no wallet agrees with — the exact failure the
 * conformance vectors exist to catch.
 */
object Keccak {
    /** The digest length, bytes. */
    const val DIGEST_LEN = 32

    /** Hashes [input] in one call. */
    fun digest256(input: ByteArray): ByteArray {
        val digest = KeccakDigest(256)
        digest.update(input, 0, input.size)
        val out = ByteArray(DIGEST_LEN)
        digest.doFinal(out, 0)
        return out
    }
}
