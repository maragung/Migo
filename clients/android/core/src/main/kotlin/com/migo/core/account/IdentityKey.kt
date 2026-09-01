package com.migo.core.account

import com.migo.core.crypto.Csprng
import com.migo.core.crypto.MlDsa

/**
 * The algorithm name recorded beside every identity public key. A string, not an enum, because
 * algorithm agility means the *next* algorithm is data the schema already holds, not a migration.
 */
const val IDENTITY_ALGORITHM = "ML-DSA-65"

/** The key format version this build generates. */
const val KEY_VERSION_ONE: Int = 1

/** The ML-DSA context for login challenge signatures. */
const val CONTEXT_LOGIN = "migo-auth-login-v1"

/** The ML-DSA context for identity rotation approvals. */
const val CONTEXT_ROTATE = "migo-auth-rotate-v1"

/** The ML-DSA context for device-credential signatures in the login ceremony. */
const val CONTEXT_LOGIN_DEVICE = "migo-auth-device-v1"

/**
 * The account identity signing key: what the `MIGO/IDENTITY/V1` domain seed becomes.
 *
 * Holds the seed and only the seed — FIPS 204 key generation *is* what a seed is for, so the
 * expanded signing key is reconstructed on demand and never stored beside it. The seed is a
 * copy the caller cannot reach after construction, and [seed] exists only so a container can
 * seal the identity alongside the root it was derived from.
 */
class IdentityKey private constructor(private val seedBytes: ByteArray) {
    companion object {
        /** Derives the identity key from a root secret. */
        fun fromRoot(root: MigoRoot): IdentityKey = fromSeed(root.domainSeed(AccountDomains.IDENTITY))

        /** Reconstructs the identity key from its 32-byte seed. */
        fun fromSeed(seed: ByteArray): IdentityKey {
            if (seed.size != MlDsa.SEED_LEN) {
                throw AccountError.badLength("identity seed", MlDsa.SEED_LEN, seed.size)
            }
            return IdentityKey(seed.copyOf())
        }
    }

    /** The seed, for sealing into a container. */
    fun seed(): ByteArray = seedBytes.copyOf()

    /** The encoded public key (1952 bytes), the only form the server ever stores. */
    fun publicKey(): ByteArray = MlDsa.publicKey(seedBytes)

    /**
     * Signs a challenge payload under the login context.
     *
     * The payload is the server's canonical challenge bytes, signed exactly as received — this
     * client never re-encodes a challenge, so two implementations cannot disagree about what was
     * signed.
     */
    fun signLogin(payload: ByteArray): ByteArray = MlDsa.sign(seedBytes, payload, CONTEXT_LOGIN)

    /** Signs under the rotation context. */
    fun signRotate(payload: ByteArray): ByteArray = MlDsa.sign(seedBytes, payload, CONTEXT_ROTATE)

    override fun toString(): String = "IdentityKey(<ML-DSA-65>)"
}

/**
 * Verifies an identity signature against a public key.
 *
 * The public key is the server's stored form; the context must be the one the signature was made
 * under, which is why callers pass one of the constants above rather than reaching for a string
 * of their own. A wrong-length key or signature is a structural refusal; a key that will not
 * parse, a forged signature, and a signature under the wrong context are one boolean refusal.
 */
fun verifyIdentity(publicKey: ByteArray, payload: ByteArray, context: String, signature: ByteArray) {
    if (!MlDsa.verify(publicKey, payload, context, signature)) {
        throw AccountError.badSignature()
    }
}

/**
 * A per-device signing credential, generated from a random seed on the device it belongs to.
 *
 * Same algorithm and wire forms as the identity key; what differs is the origin of the seed,
 * which is the whole point — the login challenge requires both the account identity signature
 * *and* the device credential signature, so a root secret that leaks from a backup alone has the
 * account half of the ceremony and none of the device half.
 */
class DeviceCredential private constructor(private val seedBytes: ByteArray) {
    companion object {
        /** Generates a fresh credential from the CSPRNG. */
        fun generate(): DeviceCredential = fromSeed(Csprng.bytes(MlDsa.SEED_LEN))

        /** Reconstructs a credential from its stored seed. */
        fun fromSeed(seed: ByteArray): DeviceCredential {
            if (seed.size != MlDsa.SEED_LEN) {
                throw AccountError.badLength("device credential seed", MlDsa.SEED_LEN, seed.size)
            }
            return DeviceCredential(seed.copyOf())
        }
    }

    /** The seed, for the device vault. */
    fun seed(): ByteArray = seedBytes.copyOf()

    /** The encoded public key (1952 bytes) registered on the device row. */
    fun publicKey(): ByteArray = MlDsa.publicKey(seedBytes)

    /**
     * Signs a login challenge under the device context. Login challenges are signed by both
     * keys, each under its own context, so one signature can never be stood in for the other.
     */
    fun signLogin(payload: ByteArray): ByteArray = MlDsa.sign(seedBytes, payload, CONTEXT_LOGIN_DEVICE)

    override fun toString(): String = "DeviceCredential(<ML-DSA-65>)"
}
