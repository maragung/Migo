package com.migo.core.crypto

/** Length of an Ed25519 or X25519 public key. */
const val PUBLIC_KEY_LEN = 32

/** Length of the published identity: signing key followed by exchange key. */
const val IDENTITY_PUBLIC_LEN = PUBLIC_KEY_LEN * 2

/** Length of an Ed25519 signature. */
const val SIGNATURE_LEN = 64

/** Length of a key seed. */
const val SEED_LEN = 32

/** Length of libsodium's expanded Ed25519 secret key: `seed || public`. */
private const val ED25519_SECRET_LEN = 64

/**
 * Domain separator for a signed prekey.
 *
 * Signatures are always over a label plus the data. Without the label, a signature produced for one
 * purpose could be presented as a signature for another — the classic cross-protocol signature
 * confusion.
 */
private val PREKEY_DOMAIN = "migo-signed-prekey-v1".toByteArray(Charsets.UTF_8)

/** Salt for the contact fingerprint. 16 bytes, which is why HKDF-Extract here needs a keyed HMAC. */
private val FINGERPRINT_SALT = "migo-fingerprint".toByteArray(Charsets.UTF_8)

/**
 * The public half of a device identity.
 *
 * A Migo device has two long-term key pairs, not one: Ed25519 for signatures and X25519 for
 * Diffie-Hellman. Signal folds both into a single Curve25519 key via XEdDSA, which saves 32 bytes
 * per published identity and costs a birational map that has to be implemented correctly in four
 * languages. Two separate keys is the boring choice, and boring is the right default here: nothing
 * in this package is a novel construction, so there is nothing in it to get subtly wrong.
 *
 * The wire form is `signing || exchange`, 64 bytes, in that order — see `identity.rs`.
 */
class IdentityPublic private constructor(
    private val signingBytes: ByteArray,
    private val exchangeBytes: ByteArray,
) {
    /**
     * The Ed25519 verifying key.
     *
     * Returns a copy. The reference implementations hold these as Rust arrays and JavaScript typed
     * arrays that callers treat as values; a Kotlin [ByteArray] is a reference, and handing out the
     * live one would let a caller mutate an identity another object already validated.
     */
    val signing: ByteArray get() = signingBytes.copyOf()

    /** The X25519 public key. Returns a copy, for the reason [signing] does. */
    val exchange: ByteArray get() = exchangeBytes.copyOf()

    companion object {
        /** Assembles an identity from its two halves, checking only their lengths. */
        fun fromParts(signing: ByteArray, exchange: ByteArray): IdentityPublic {
            if (signing.size != PUBLIC_KEY_LEN) {
                throw CryptoError.badLength("signing key", PUBLIC_KEY_LEN, signing.size)
            }
            if (exchange.size != PUBLIC_KEY_LEN) {
                throw CryptoError.badLength("exchange key", PUBLIC_KEY_LEN, exchange.size)
            }
            return IdentityPublic(signing.copyOf(), exchange.copyOf())
        }

        /**
         * Parses the 64-byte wire form, rejecting keys that are not usable points.
         *
         * The validation happens here, at parse time, rather than at first use. An invalid key that
         * is stored and only fails later produces a session that cannot be repaired.
         */
        fun parse(bytes: ByteArray): IdentityPublic {
            if (bytes.size != IDENTITY_PUBLIC_LEN) {
                throw CryptoError.badLength("identity public key", IDENTITY_PUBLIC_LEN, bytes.size)
            }
            val signing = bytes.copyOfRange(0, PUBLIC_KEY_LEN)
            val exchange = bytes.copyOfRange(PUBLIC_KEY_LEN, IDENTITY_PUBLIC_LEN)
            if (!isUsableSigningKey(signing)) throw CryptoError.invalidPublicKey()
            if (isSmallOrder(exchange)) throw CryptoError.invalidPublicKey()
            return IdentityPublic(signing, exchange)
        }
    }

    /** Serialises to the 64-byte wire form. */
    fun toBytes(): ByteArray {
        val out = ByteArray(IDENTITY_PUBLIC_LEN)
        System.arraycopy(signingBytes, 0, out, 0, PUBLIC_KEY_LEN)
        System.arraycopy(exchangeBytes, 0, out, PUBLIC_KEY_LEN, PUBLIC_KEY_LEN)
        return out
    }

    /**
     * Verifies a signature made by this identity over `label || message`.
     *
     * The failure kinds are ordered as the reference orders them, because the vectors name the kind
     * they expect: an unusable key is [CryptoErrorKind.InvalidPublicKey], a signature of the wrong
     * size is [CryptoErrorKind.BadLength], and only a well-formed signature that does not verify is
     * [CryptoErrorKind.BadSignature].
     */
    fun verify(label: ByteArray, message: ByteArray, signature: ByteArray) {
        if (!isUsableSigningKey(signingBytes)) throw CryptoError.invalidPublicKey()
        if (signature.size != SIGNATURE_LEN) {
            throw CryptoError.badLength("signature", SIGNATURE_LEN, signature.size)
        }
        val signed = concatBytes(label, message)
        val verified = Sodium.sign.cryptoSignVerifyDetached(
            signature,
            signed,
            signed.size,
            signingBytes,
        )
        if (!verified) throw CryptoError.badSignature()
    }

    /**
     * The 32-byte fingerprint users compare when verifying a contact in person.
     *
     * Derived from the full identity rather than one half, so a mismatch in either key shows up.
     * The client renders it as safety numbers.
     */
    fun fingerprint(): ByteArray =
        Kdf.derive(toBytes(), FINGERPRINT_SALT, "migo-fingerprint-v1", 32)

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is IdentityPublic) return false
        return signingBytes.contentEquals(other.signingBytes) &&
            exchangeBytes.contentEquals(other.exchangeBytes)
    }

    override fun hashCode(): Int = 31 * signingBytes.contentHashCode() + exchangeBytes.contentHashCode()

    /** Public material, so the hex is safe to print — and useful in a log about a peer. */
    override fun toString(): String =
        "IdentityPublic(signing: ${hexOf(signingBytes)}, exchange: ${hexOf(exchangeBytes)})"
}

/**
 * The private half of a device identity. Never leaves the device.
 *
 * Held as the two 32-byte seeds rather than as expanded key material, matching
 * `IdentitySecret::from_seeds`: the seeds are what the device's encrypted store writes, and the
 * expanded Ed25519 secret key is rebuilt for each signature and zeroed immediately after. There is
 * no serialisation here to anything but that store — no server-side escrow, and therefore no
 * request an administrator or a court can serve on Migo that produces someone's plaintext.
 */
class IdentitySecret private constructor(
    private val signingSeed: ByteArray,
    private val exchangeSeed: ByteArray,
) {
    companion object {
        /** Generates a new device identity from the platform CSPRNG. */
        fun generate(): IdentitySecret =
            IdentitySecret(Csprng.bytes(SEED_LEN), Csprng.bytes(SEED_LEN))

        /**
         * Rebuilds an identity from its two seeds.
         *
         * The order is `signing`, then `exchange`, matching the public wire form.
         */
        fun fromSeeds(signingSeed: ByteArray, exchangeSeed: ByteArray): IdentitySecret {
            if (signingSeed.size != SEED_LEN) {
                throw CryptoError.badLength("signing seed", SEED_LEN, signingSeed.size)
            }
            if (exchangeSeed.size != SEED_LEN) {
                throw CryptoError.badLength("exchange seed", SEED_LEN, exchangeSeed.size)
            }
            return IdentitySecret(signingSeed.copyOf(), exchangeSeed.copyOf())
        }
    }

    /** The public half, for publishing to the server. */
    fun public(): IdentityPublic {
        val publicKey = ByteArray(PUBLIC_KEY_LEN)
        val secretKey = ByteArray(ED25519_SECRET_LEN)
        val ok = Sodium.sign.cryptoSignSeedKeypair(publicKey, secretKey, signingSeed)
        secretKey.fill(0)
        check(ok) { "ed25519 key derivation failed" }
        return IdentityPublic.fromParts(publicKey, x25519PublicKey(exchangeSeed))
    }

    /** Signs `label || message`. */
    fun sign(label: ByteArray, message: ByteArray): ByteArray {
        val publicKey = ByteArray(PUBLIC_KEY_LEN)
        val secretKey = ByteArray(ED25519_SECRET_LEN)
        check(Sodium.sign.cryptoSignSeedKeypair(publicKey, secretKey, signingSeed)) {
            "ed25519 key derivation failed"
        }
        val signed = concatBytes(label, message)
        val signature = ByteArray(SIGNATURE_LEN)
        val ok = Sodium.sign.cryptoSignDetached(
            signature,
            signed,
            signed.size.toLong(),
            secretKey,
        )
        secretKey.fill(0)
        check(ok) { "ed25519 signing failed" }
        return signature
    }

    /** Diffie-Hellman between this identity and a peer's X25519 public key. */
    fun diffieHellman(peer: ByteArray): ByteArray = x25519(exchangeSeed, peer)

    /** Exposes the signing seed, for writing to the device's encrypted store. */
    fun exposeSigningSeed(): ByteArray = signingSeed.copyOf()

    /** Exposes the exchange seed, for writing to the device's encrypted store. */
    fun exposeExchangeSeed(): ByteArray = exchangeSeed.copyOf()

    override fun toString(): String = "IdentitySecret(***)"
}

/**
 * An X25519 key pair used as a prekey or a ratchet key.
 *
 * Holds the seed rather than a clamped scalar, as `x25519-dalek`'s `StaticSecret` and `@noble`'s
 * key handling do. libsodium's `crypto_scalarmult` and `crypto_scalarmult_base` clamp internally,
 * so the same seed yields the same public key and the same shared secret in all four
 * implementations.
 */
class KeyPair private constructor(
    private val secretSeed: ByteArray,
    private val publicKey: ByteArray,
) {
    companion object {
        /** Generates a fresh pair from the platform CSPRNG. */
        fun generate(): KeyPair = fromSeed(Csprng.bytes(SEED_LEN))

        /** Rebuilds a pair from its 32-byte seed. */
        fun fromSeed(seed: ByteArray): KeyPair {
            if (seed.size != SEED_LEN) {
                throw CryptoError.badLength("key seed", SEED_LEN, seed.size)
            }
            val held = seed.copyOf()
            return KeyPair(held, x25519PublicKey(held))
        }
    }

    /** The public half. A copy, so a caller cannot rewrite a key this pair has already published. */
    fun public(): ByteArray = publicKey.copyOf()

    /** Diffie-Hellman with a peer's public key. */
    fun diffieHellman(peer: ByteArray): ByteArray = x25519(secretSeed, peer)

    /** Exposes the seed, for the device's encrypted store. */
    fun exposeSeed(): ByteArray = secretSeed.copyOf()

    /** The public half only. A seed must never reach a log line. */
    override fun toString(): String = "KeyPair(${hexOf(publicKey)})"
}

/**
 * A prekey with the signature that binds it to an identity.
 *
 * [verify] is the check that makes the server untrusted. The server chooses which bundle to serve,
 * so without it the server could substitute a prekey it controls and read everything sent to that
 * device. With it, a substituted prekey fails verification on the sender's device before any
 * message is composed. It is not optional and there is no code path here that skips it.
 */
class SignedPrekey(
    /** Identifier the publisher assigned, as an unsigned 32-bit value. */
    val keyId: Long,
    publicKey: ByteArray,
    signature: ByteArray,
) {
    private val publicKeyBytes: ByteArray
    private val signatureBytes: ByteArray

    init {
        requireU32(keyId, "prekey id")
        if (publicKey.size != PUBLIC_KEY_LEN) {
            throw CryptoError.badLength("prekey public key", PUBLIC_KEY_LEN, publicKey.size)
        }
        if (signature.size != SIGNATURE_LEN) {
            throw CryptoError.badLength("prekey signature", SIGNATURE_LEN, signature.size)
        }
        publicKeyBytes = publicKey.copyOf()
        signatureBytes = signature.copyOf()
    }

    /** The X25519 public key. */
    val publicKey: ByteArray get() = publicKeyBytes.copyOf()

    /** The Ed25519 signature over the domain label, the key id, and the key. */
    val signature: ByteArray get() = signatureBytes.copyOf()

    companion object {
        /** Signs [pair] with [identity]. */
        fun create(identity: IdentitySecret, keyId: Long, pair: KeyPair): SignedPrekey {
            val publicKey = pair.public()
            return SignedPrekey(
                keyId,
                publicKey,
                identity.sign(PREKEY_DOMAIN, prekeySignedBytes(keyId, publicKey)),
            )
        }
    }

    /**
     * Verifies that this prekey was signed by [identity].
     *
     * Every failure becomes [CryptoErrorKind.InvalidPrekeyBundle], as in the reference: the caller's
     * response to a bundle it cannot trust is the same whatever the reason, and collapsing the kinds
     * keeps a caller from branching on a distinction that does not exist.
     */
    fun verify(identity: IdentityPublic) {
        try {
            identity.verify(PREKEY_DOMAIN, prekeySignedBytes(keyId, publicKeyBytes), signatureBytes)
        } catch (_: CryptoError) {
            throw CryptoError.invalidPrekeyBundle()
        }
    }

    override fun toString(): String =
        "SignedPrekey(key_id: $keyId, public_key: ${hexOf(publicKeyBytes)})"
}

/**
 * The bytes covered by a prekey signature: key id (big-endian) then key.
 *
 * The id is inside the signature so that a valid signature cannot be moved onto a different id and
 * cause the two sides to disagree about which prekey was used.
 */
internal fun prekeySignedBytes(keyId: Long, publicKey: ByteArray): ByteArray {
    val out = ByteArray(4 + PUBLIC_KEY_LEN)
    putU32Be(out, 0, keyId)
    System.arraycopy(publicKey, 0, out, 4, PUBLIC_KEY_LEN)
    return out
}

/** `crypto_scalarmult_base`: the X25519 public key for a seed. */
internal fun x25519PublicKey(seed: ByteArray): ByteArray {
    val publicKey = ByteArray(PUBLIC_KEY_LEN)
    check(Sodium.dh.cryptoScalarMultBase(publicKey, seed)) { "x25519 base point multiply failed" }
    return publicKey
}

/**
 * `crypto_scalarmult`: the X25519 shared secret between [secretSeed] and [peer].
 *
 * Small-order peers are rejected before the multiply rather than after. `x25519-dalek` returns an
 * all-zero shared secret for them, and an all-zero secret that is silently accepted means both
 * sides derive the same key from nothing — indistinguishable from a working session until someone
 * notices the ciphertext is decryptable by anyone. Rejecting the input is clearer than checking the
 * output, and it is the check the reference makes.
 */
internal fun x25519(secretSeed: ByteArray, peer: ByteArray): ByteArray {
    if (peer.size != PUBLIC_KEY_LEN || isSmallOrder(peer)) throw CryptoError.invalidPublicKey()
    val shared = ByteArray(PUBLIC_KEY_LEN)
    if (!Sodium.dh.cryptoScalarMult(shared, secretSeed, peer)) {
        // libsodium additionally refuses an all-zero result, which the small-order table above
        // should already have caught. Reaching here means an input it rejects and the table does
        // not, and the answer is the same either way.
        shared.fill(0)
        throw CryptoError.invalidPublicKey()
    }
    return shared
}

/**
 * Rejects the known small-order X25519 points.
 *
 * The complete list from RFC 7748 section 6.1 and Curve25519 analysis, in the same order as
 * `identity.rs` and `identity.ts` — seven entries, and a table that is short by one is a table that
 * accepts the point it is missing.
 */
internal fun isSmallOrder(publicKey: ByteArray): Boolean {
    if (publicKey.size != PUBLIC_KEY_LEN) return false
    var matched = 0
    for (candidate in SMALL_ORDER) {
        matched = matched or (if (bytesEqualConstantTime(publicKey, candidate)) 1 else 0)
    }
    return matched != 0
}

/**
 * Whether an Ed25519 public key can be used to verify a signature.
 *
 * libsodium exposes no bare "decompress this point" call, so the check borrows
 * `crypto_sign_ed25519_pk_to_curve25519`, which fails on exactly the keys that cannot verify —
 * plus, additionally, small-order points and points off the main subgroup. That makes this check
 * marginally stricter than `VerifyingKey::from_bytes` and `@noble`'s `isValidPublicKey`, which
 * accept a small-order signing key and then fail every verification against it. The divergence is
 * unreachable for real identities: `crypto_sign_seed_keypair` and its Rust and JavaScript
 * equivalents only ever produce main-subgroup points, so no key any implementation publishes is
 * refused here. It is recorded rather than papered over because a vector that deliberately feeds a
 * small-order signing key would see `InvalidPublicKey` on this client and `BadSignature` on the
 * others.
 */
internal fun isUsableSigningKey(signing: ByteArray): Boolean {
    if (signing.size != PUBLIC_KEY_LEN) return false
    val scratch = ByteArray(PUBLIC_KEY_LEN)
    val usable = Sodium.sign.convertPublicKeyEd25519ToCurve25519(scratch, signing)
    scratch.fill(0)
    return usable
}

/** Constant-time byte comparison: no early return, so timing does not leak which byte differed. */
private fun bytesEqualConstantTime(a: ByteArray, b: ByteArray): Boolean {
    if (a.size != b.size) return false
    var diff = 0
    for (i in a.indices) diff = diff or (a[i].toInt() xor b[i].toInt())
    return diff == 0
}

/**
 * Builds a point from the decimal literals the reference tables use.
 *
 * Written as [Int] because Kotlin's `byteArrayOf` takes `Byte`, and a literal above 127 is not a
 * `Byte`: transcribing the reference's `224, 235, 122, ...` would need every entry rewritten as a
 * negative number, which is exactly the kind of hand transformation a table like this must not
 * carry. The digits below are the reference's digits.
 */
private fun pointOf(vararg bytes: Int): ByteArray = ByteArray(bytes.size) { bytes[it].toByte() }

/** A point that is `0xff` everywhere except its first and last byte. Three of the seven. */
private fun nearPrimePoint(first: Int): ByteArray {
    val point = ByteArray(PUBLIC_KEY_LEN) { 0xff.toByte() }
    point[0] = first.toByte()
    point[PUBLIC_KEY_LEN - 1] = 0x7f
    return point
}

private val SMALL_ORDER: Array<ByteArray> = arrayOf(
    ByteArray(PUBLIC_KEY_LEN),
    ByteArray(PUBLIC_KEY_LEN).also { it[0] = 1 },
    pointOf(
        224, 235, 122, 124, 59, 65, 184, 174, 22, 86, 227, 250, 241, 159, 196, 106,
        218, 9, 141, 235, 156, 50, 177, 253, 134, 98, 5, 22, 95, 73, 184, 0,
    ),
    pointOf(
        95, 156, 149, 188, 163, 80, 140, 36, 177, 208, 177, 85, 156, 131, 239, 91,
        4, 68, 92, 196, 88, 28, 142, 134, 216, 34, 78, 221, 208, 159, 17, 87,
    ),
    nearPrimePoint(236),
    nearPrimePoint(237),
    nearPrimePoint(238),
)
