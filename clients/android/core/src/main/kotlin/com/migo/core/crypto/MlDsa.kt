package com.migo.core.crypto

import org.bouncycastle.crypto.params.ParametersWithContext
import org.bouncycastle.pqc.crypto.mldsa.MLDSAParameters
import org.bouncycastle.pqc.crypto.mldsa.MLDSAPrivateKeyParameters
import org.bouncycastle.pqc.crypto.mldsa.MLDSAPublicKeyParameters
import org.bouncycastle.pqc.crypto.mldsa.MLDSASigner

/**
 * ML-DSA-65 (FIPS 204), the account identity algorithm.
 *
 * Key generation is seed-based, which is the whole portability story: the 32-byte seed is the
 * FIPS 204 keygen input, the public key is a pure function of it, and every port that feeds the
 * same seed to a conformant implementation derives the same identity and produces the same
 * deterministic signature bytes. The conformance vectors pin those bytes; if this wrapper ever
 * disagrees with the Rust reference, the vector test is what says so.
 *
 * The context string is not optional here. Every signature is made under a caller-named context,
 * mixed into the message digest by the standard, so a login challenge signature can never be
 * replayed as a rotation approval. Callers pass the constants from `com.migo.core.account`, not
 * strings of their own choosing.
 */
object MlDsa {
    /** Seed length for every ML-DSA parameter set, FIPS 204 Algorithm 6's `xi`. */
    const val SEED_LEN = 32

    /** ML-DSA-65 public key length: 32 bytes of rho plus 1920 bytes of t1. */
    const val PUBLIC_KEY_LEN = 1952

    /** ML-DSA-65 signature length. */
    const val SIGNATURE_LEN = 3309

    /** The encoded public key the seed expands to. */
    fun publicKey(seed: ByteArray): ByteArray {
        requireSeed(seed)
        return privateKeyOf(seed).publicKeyParameters.encoded
    }

    /**
     * Signs `payload` under `context`, deterministically.
     *
     * No randomness enters the signature (FIPS 204 permits deterministic signing), which is what
     * lets the vectors pin the bytes and lets a re-sign on a restored device reproduce them.
     */
    fun sign(seed: ByteArray, payload: ByteArray, context: String): ByteArray {
        requireSeed(seed)
        val signer = MLDSASigner()
        // ParametersWithContext rather than ParametersWithRandom: the context rides beside the
        // key, and the absence of a random source is the deterministic mode itself.
        signer.init(true, ParametersWithContext(privateKeyOf(seed), context.toByteArray(Charsets.UTF_8)))
        signer.update(payload, 0, payload.size)
        return signer.generateSignature()
    }

    /**
     * Verifies a signature against an encoded public key under the same context it was made in.
     *
     * A wrong-length key or signature is a structural [CryptoError.badLength] refusal; anything
     * else — a garbage key that will not parse, a forged or edited signature, a signature made
     * under a different context — is one boolean refusal with no hint about which half failed.
     */
    fun verify(publicKey: ByteArray, payload: ByteArray, context: String, signature: ByteArray): Boolean {
        if (publicKey.size != PUBLIC_KEY_LEN) {
            throw CryptoError.badLength("identity public key", PUBLIC_KEY_LEN, publicKey.size)
        }
        if (signature.size != SIGNATURE_LEN) {
            throw CryptoError.badLength("identity signature", SIGNATURE_LEN, signature.size)
        }
        val signer = MLDSASigner()
        signer.init(
            false,
            ParametersWithContext(
                MLDSAPublicKeyParameters(MLDSAParameters.ml_dsa_65, publicKey.copyOf()),
                context.toByteArray(Charsets.UTF_8),
            ),
        )
        signer.update(payload, 0, payload.size)
        return signer.verifySignature(signature)
    }

    private fun privateKeyOf(seed: ByteArray): MLDSAPrivateKeyParameters =
        MLDSAPrivateKeyParameters(MLDSAParameters.ml_dsa_65, seed.copyOf())

    private fun requireSeed(seed: ByteArray) {
        if (seed.size != SEED_LEN) {
            throw CryptoError.badLength("ML-DSA seed", SEED_LEN, seed.size)
        }
    }
}
