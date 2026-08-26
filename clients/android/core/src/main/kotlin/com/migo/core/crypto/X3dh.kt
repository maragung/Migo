package com.migo.core.crypto

/** Length of the shared secret X3DH produces. */
const val SHARED_SECRET_LEN = 32

/**
 * The 32 `0xff` bytes X3DH prepends to its Diffie-Hellman material.
 *
 * Signal's spec calls this `F`. It exists so that the concatenation X3DH hashes can never be
 * confused with a raw X25519 output: `0xff` repeated 32 times is not a valid Curve25519 field
 * element, so no DH result can collide with the prefix and no other protocol's transcript can be
 * reinterpreted as an X3DH one.
 */
private val F_PREFIX = ByteArray(32) { 0xff.toByte() }

/** A one-time prekey the server hands out exactly once. */
class OneTimePrekey(
    /** Identifier the publisher assigned, as an unsigned 32-bit value. */
    val keyId: Long,
    publicKey: ByteArray,
) {
    private val publicKeyBytes: ByteArray

    init {
        requireU32(keyId, "one-time prekey id")
        if (publicKey.size != PUBLIC_KEY_LEN) {
            throw CryptoError.badLength("one-time prekey", PUBLIC_KEY_LEN, publicKey.size)
        }
        publicKeyBytes = publicKey.copyOf()
    }

    /** The X25519 public key. */
    val publicKey: ByteArray get() = publicKeyBytes.copyOf()

    override fun toString(): String =
        "OneTimePrekey(key_id: $keyId, public_key: ${hexOf(publicKeyBytes)})"
}

/**
 * Everything the server publishes for a device, so a sender can start a session without it online.
 *
 * The one-time prekey is nullable because the server runs out. A bundle without one still produces a
 * working session, with weaker replay resistance for the very first message — the trade Signal makes
 * for the same reason, and better than refusing to deliver.
 */
class PrekeyBundle(
    val identity: IdentityPublic,
    val signedPrekey: SignedPrekey,
    val oneTimePrekey: OneTimePrekey?,
) {
    /**
     * Checks that the signed prekey really came from [identity].
     *
     * Called by [X3dh.initiate] before any key is derived. A caller that skips it has handed the
     * server the ability to substitute a prekey it controls.
     */
    fun verify() = signedPrekey.verify(identity)

    override fun toString(): String =
        "PrekeyBundle(identity: $identity, signed_prekey: $signedPrekey, " +
            "one_time_prekey: ${oneTimePrekey ?: "none"})"
}

/**
 * What the initiator sends alongside its first ciphertext.
 *
 * All of it is public. The responder needs it to reconstruct the same four Diffie-Hellman outputs,
 * and the prekey ids are here so the responder knows which of its own keys to use — a responder that
 * guessed would derive a different secret and simply fail to decrypt.
 */
class InitialMessage(
    val identity: IdentityPublic,
    ephemeralKey: ByteArray,
    /** Which signed prekey of the responder's this message was built against. */
    val signedPrekeyId: Long,
    /** Which one-time prekey, or null when the bundle had none left. */
    val oneTimePrekeyId: Long?,
) {
    private val ephemeralBytes: ByteArray

    init {
        requireU32(signedPrekeyId, "signed prekey id")
        oneTimePrekeyId?.let { requireU32(it, "one-time prekey id") }
        if (ephemeralKey.size != PUBLIC_KEY_LEN) {
            throw CryptoError.badLength("ephemeral key", PUBLIC_KEY_LEN, ephemeralKey.size)
        }
        ephemeralBytes = ephemeralKey.copyOf()
    }

    /** The initiator's ephemeral X25519 public key. */
    val ephemeralKey: ByteArray get() = ephemeralBytes.copyOf()

    override fun toString(): String =
        "InitialMessage(identity: $identity, ephemeral_key: ${hexOf(ephemeralBytes)}, " +
            "signed_prekey_id: $signedPrekeyId, one_time_prekey_id: ${oneTimePrekeyId ?: "none"})"
}

/**
 * The output of X3DH: one shared secret, and the associated data both sides authenticate under it.
 *
 * The associated data binds both identities into every message of the session. Without it, a message
 * could be replayed into a different pair of devices that happened to derive the same secret — which
 * an attacker who controls prekey distribution can arrange.
 */
class SessionSeed internal constructor(
    private val sharedSecret: ByteArray,
    private val associatedDataBytes: ByteArray,
) {
    /** The 128 bytes of public identity material every message in this session authenticates. */
    val associatedData: ByteArray get() = associatedDataBytes.copyOf()

    /** Borrows the shared secret. The greppable audit point for this secret leaving the type. */
    fun exposeSharedSecret(): ByteArray = sharedSecret.copyOf()

    /** Zeroes the shared secret once a ratchet has consumed it. */
    fun destroy() {
        sharedSecret.fill(0)
    }

    override fun toString(): String =
        "SessionSeed(shared_secret: ***, associated_data_len: ${associatedDataBytes.size})"
}

/** An initiated session: the seed, the message to send, and the ephemeral pair it used. */
class Initiation(
    val seed: SessionSeed,
    val message: InitialMessage,
    /** Kept so the caller can seed its ratchet from the same pair rather than a fresh one. */
    val ephemeral: KeyPair,
)

/**
 * X3DH — asynchronous session setup.
 *
 * Two devices agree on a shared secret when only one of them is online. The initiator fetches a
 * prekey bundle the responder published earlier and derives the secret from four Diffie-Hellman
 * outputs; the responder reconstructs the same four from the initial message whenever it comes back.
 *
 * Which four, and why each one:
 *
 * * `DH1 = IK_A x SPK_B` — authenticates the initiator to the responder.
 * * `DH2 = EK_A x IK_B` — authenticates the responder to the initiator.
 * * `DH3 = EK_A x SPK_B` — supplies forward secrecy, because `EK_A` is discarded after use.
 * * `DH4 = EK_A x OPK_B` — replay resistance for the first message, since the server hands each
 *   one-time prekey out exactly once. Absent when the responder has run out.
 *
 * `DH1` alone would let anyone who compromised `SPK_B` impersonate the initiator forever; `DH3`
 * alone would give forward secrecy with no authentication at all. The set is what makes the result
 * both authenticated and forward-secret, and dropping any of the first three is not an optimisation.
 *
 * This mirrors `server/crates/migo-crypto/src/x3dh.rs` and `packages/crypto/src/x3dh.ts`, including
 * the order the outputs are concatenated in: the concatenation is hashed, so a different order is a
 * different secret and a silent interoperability failure.
 */
object X3dh {
    /**
     * Starts a session against a published bundle.
     *
     * [ephemeral] defaults to a fresh pair and is a parameter only so a test vector can pin it.
     */
    fun initiate(
        identity: IdentitySecret,
        bundle: PrekeyBundle,
        ephemeral: KeyPair = KeyPair.generate(),
    ): Initiation {
        // Before any key material: a bundle whose signature does not check out is not usable, and
        // deriving from it first would mean the failure surfaces after the work rather than before.
        bundle.verify()

        val signedPrekey = bundle.signedPrekey.publicKey
        val dh1 = identity.diffieHellman(signedPrekey)
        val dh2 = ephemeral.diffieHellman(bundle.identity.exchange)
        val dh3 = ephemeral.diffieHellman(signedPrekey)
        val oneTime = bundle.oneTimePrekey
        val dh4 = oneTime?.let { ephemeral.diffieHellman(it.publicKey) }

        val material =
            if (dh4 == null) concatBytes(F_PREFIX, dh1, dh2, dh3)
            else concatBytes(F_PREFIX, dh1, dh2, dh3, dh4)
        val sharedSecret = Kdf.derive(material, null, Kdf.LABEL_X3DH, SHARED_SECRET_LEN)
        zeroAll(dh1, dh2, dh3, material)
        dh4?.fill(0)

        return Initiation(
            SessionSeed(sharedSecret, x3dhAssociatedData(identity.public(), bundle.identity)),
            InitialMessage(
                identity.public(),
                ephemeral.public(),
                bundle.signedPrekey.keyId,
                oneTime?.keyId,
            ),
            ephemeral,
        )
    }

    /**
     * Reconstructs the same secret on the responder's side.
     *
     * A message that names a one-time prekey while the caller supplies none — or the reverse — is
     * [CryptoErrorKind.NoSession] rather than a session derived from three outputs instead of four.
     * Silently proceeding would produce a secret neither side agrees on, and a decryption failure
     * several steps later that says nothing about the cause.
     */
    fun respond(
        identity: IdentitySecret,
        signedPrekey: KeyPair,
        oneTimePrekey: KeyPair?,
        message: InitialMessage,
    ): SessionSeed {
        if ((message.oneTimePrekeyId != null) != (oneTimePrekey != null)) {
            throw CryptoError.noSession()
        }
        val ephemeralKey = message.ephemeralKey
        val dh1 = signedPrekey.diffieHellman(message.identity.exchange)
        val dh2 = identity.diffieHellman(ephemeralKey)
        val dh3 = signedPrekey.diffieHellman(ephemeralKey)
        val dh4 = oneTimePrekey?.diffieHellman(ephemeralKey)

        val material =
            if (dh4 == null) concatBytes(F_PREFIX, dh1, dh2, dh3)
            else concatBytes(F_PREFIX, dh1, dh2, dh3, dh4)
        val sharedSecret = Kdf.derive(material, null, Kdf.LABEL_X3DH, SHARED_SECRET_LEN)
        zeroAll(dh1, dh2, dh3, material)
        dh4?.fill(0)

        return SessionSeed(
            sharedSecret,
            x3dhAssociatedData(message.identity, identity.public()),
        )
    }
}

/**
 * The 128 bytes bound into every message of a session: initiator identity, then responder identity.
 *
 * Fixed width and fixed order. Both sides build it from the same two identities in the same roles,
 * which is why [X3dh.respond] passes the message's identity first and its own second.
 */
internal fun x3dhAssociatedData(initiator: IdentityPublic, responder: IdentityPublic): ByteArray {
    val out = ByteArray(IDENTITY_PUBLIC_LEN * 2)
    System.arraycopy(initiator.toBytes(), 0, out, 0, IDENTITY_PUBLIC_LEN)
    System.arraycopy(responder.toBytes(), 0, out, IDENTITY_PUBLIC_LEN, IDENTITY_PUBLIC_LEN)
    return out
}

/** Zeroes every buffer given. Intermediate DH outputs have no reason to outlive the derivation. */
private fun zeroAll(vararg buffers: ByteArray) {
    for (buffer in buffers) buffer.fill(0)
}
