package com.migo.core.crypto

import com.migo.core.wire.Id
import com.migo.core.wire.idToBytes

/** Length of a call's media key material. */
const val CALL_KEY_LEN = 32

/**
 * The media key of an encrypted call, and its rotation — the Kotlin port of the Rust crate's
 * `CallKeyState` (section 163), carried decision for decision because the sealed bytes cross
 * clients: a rotation this build seals is adopted by the web and desktop builds, so any divergence
 * in the binding or the join-distribution layout is a call that cannot re-key, not a style
 * difference.
 *
 * A call's key is *derived*, not minted and sent: both seats already hold a secret no server has
 * ever seen, and HKDF under [Kdf.LABEL_CALL_KEY] (the call id as salt) turns it into a media key
 * neither party had to transmit. Deriving instead of distributing is what keeps the call's
 * confidentiality anchored to the session's.
 *
 * # Rotation
 *
 * A group call's membership changes, and section 163 requires that a participant who leaves cannot
 * decrypt what follows and one who joins cannot decrypt what came before. So the key rotates:
 * [rotate] mints fresh material for the next epoch and returns it *sealed under the current key* —
 * those bytes are the `sealed_key_material` of a `CallKeyUpdate`, and only a device holding the
 * current epoch can open them. [adopt] accepts such an update and refuses any epoch that does not
 * advance, the same replay-and-rollback refusal the message ratchets live by.
 *
 * A participant who joins mid-call receives the current key by their own path:
 * [sealedJoinDistribution] seals the current epoch and key under a wrapping key derived from the
 * secret this device shares with *that joiner* (HKDF under [Kdf.LABEL_CALL_JOIN], the call id as
 * salt), and the joiner opens it with [fromJoinDistribution]. The caller is expected to rotate on
 * the join, so what the joiner receives is a key that did not exist while they were outside the
 * call — pre-join media stays sealed because the frames before the rotation are bound to an older
 * epoch. After that first key, the joiner rides the same rotations as everyone else.
 *
 * # What this class is not
 *
 * It is the key, its derivation, and its rotation. It does not touch media itself, and it does not
 * decide *when* roster movement triggers a rotation — that is the group-call domain's business.
 */
class CallKeyState private constructor(
    /** The call this key belongs to, bound into every AEAD binding and derivation salt. */
    private val callId: Id,
    private var epoch: Long,
    private var key: ByteArray,
) {
    /** The epoch this state's key belongs to. Zero until the first rotation. */
    fun epoch(): Long = epoch

    /**
     * Rotates: mints the next epoch's key and returns it sealed under the current one.
     *
     * The sealed bytes are what a `CallKeyUpdate` carries as `sealed_key_material`; its `epoch`
     * field is the value [epoch] reports after this call. State advances only after the sealing
     * succeeds, so a failure leaves the call on the old key rather than between keys.
     */
    fun rotate(): ByteArray {
        val next = Csprng.bytes(CALL_KEY_LEN)
        val sealed = Aead.seal(SymmetricKey.fromBytes(key), binding(epoch + 1L), next)
        key.fill(0)
        key = next
        epoch += 1L
        return sealed
    }

    /**
     * Adopts a distributed update, moving to [epoch].
     *
     * The update must open under the *current* key and be bound to exactly [epoch], and the epoch
     * must advance: an update that repeats or rolls back the epoch is a replay of an old key and is
     * refused with [CryptoError.keyAlreadyUsed]. As everywhere in this package, the state moves
     * only after the new material is verified, so a bad update cannot destroy a working key.
     */
    fun adopt(epoch: Long, sealed: ByteArray) {
        if (epoch <= this.epoch) {
            // Reuse would let a replayed update re-install an old key and re-open the media it
            // sealed, which is the call-side face of the rule the message ratchets enforce with the
            // same error.
            throw CryptoError.keyAlreadyUsed()
        }
        val material = Aead.open(SymmetricKey.fromBytes(key), binding(epoch), sealed)
        if (material.size != CALL_KEY_LEN) {
            material.fill(0)
            throw CryptoError.badLength("call key material", CALL_KEY_LEN, material.size)
        }
        key.fill(0)
        key = material
        this.epoch = epoch
    }

    /**
     * Seals one media frame under the current key.
     *
     * The associated data binds the call and the epoch, so a frame cannot be lifted into another
     * call, and a frame from before a rotation cannot be presented as one from after it even to a
     * device that kept the old key.
     */
    fun sealFrame(frame: ByteArray): ByteArray =
        Aead.seal(SymmetricKey.fromBytes(key), binding(epoch), frame)

    /** Opens a media frame sealed under the current key. */
    fun openFrame(sealed: ByteArray): ByteArray =
        Aead.open(SymmetricKey.fromBytes(key), binding(epoch), sealed)

    /**
     * Seals the current epoch and key for a participant joining mid-call.
     *
     * The wrapping key is derived from [sessionSecret] — the secret this device shares *with the
     * joiner*, not the one the call started from — under its own label, so the call's own key and
     * the key that wraps it for the joiner are never the same material. The call id is both the
     * HKDF salt and the AEAD associated data, so the blob cannot be opened for another call. The
     * joiner reads the epoch out of the sealed body itself, which means a blob that lied about its
     * epoch does not parse.
     *
     * The caller should rotate *before* distributing: a joiner handed the key that was current
     * while they were outside the call can open the media that key sealed. Rotation on join is what
     * makes "sealed for them at join" also mean "sealed against them until join".
     */
    fun sealedJoinDistribution(sessionSecret: ByteArray): ByteArray {
        val plaintext = ByteArray(JOIN_DISTRIBUTION_LEN)
        try {
            epochToBytes(epoch).copyInto(plaintext, 0)
            key.copyInto(plaintext, 8)
            return Aead.seal(joinWrappingKey(sessionSecret, callId), idToBytes(callId), plaintext)
        } finally {
            plaintext.fill(0)
        }
    }

    /** Zeroes the key. Any later use throws. */
    fun destroy() {
        key.fill(0)
    }

    /** Call id and epoch only. The key is never rendered, not even starred. */
    override fun toString(): String = "CallKeyState(callId: $callId, epoch: $epoch, key: ***)"

    /**
     * The bytes every cryptographic operation here binds: which call, which epoch. Fixed-width so
     * the pair cannot be re-split.
     */
    private fun binding(epoch: Long): ByteArray {
        val out = ByteArray(16 + 8)
        idToBytes(callId).copyInto(out, 0)
        epochToBytes(epoch).copyInto(out, 16)
        return out
    }

    companion object {
        /** Length of a join distribution's plaintext: the epoch, then the key. */
        const val JOIN_DISTRIBUTION_LEN = 8 + CALL_KEY_LEN

        /**
         * Derives the call's first key (epoch 0) from the pairwise session secret.
         *
         * The call id is the HKDF salt, so one session cannot produce the same media key for two
         * different calls — a key that outlived its call would be a second purpose for a key that
         * already had one.
         */
        fun fromSession(sessionSecret: ByteArray, callId: Id): CallKeyState =
            CallKeyState(
                callId = callId,
                epoch = 0L,
                key = Kdf.derive(sessionSecret, idToBytes(callId), Kdf.LABEL_CALL_KEY, CALL_KEY_LEN),
            )

        /**
         * Opens a join distribution into the state it carries: the joiner's first key of a call
         * already in progress.
         *
         * This is a constructor, not an [adopt]: the joiner holds no earlier epoch to compare
         * against, so the first distribution is the baseline, exactly as the first sender-key
         * distribution is. The blob must open under the session secret this device shares with the
         * sender of the distribution and be bound to [callId].
         */
        fun fromJoinDistribution(sessionSecret: ByteArray, callId: Id, sealed: ByteArray): CallKeyState {
            val plaintext = Aead.open(joinWrappingKey(sessionSecret, callId), idToBytes(callId), sealed)
            try {
                if (plaintext.size != JOIN_DISTRIBUTION_LEN) {
                    throw CryptoError.badLength(
                        "call join distribution",
                        JOIN_DISTRIBUTION_LEN,
                        plaintext.size,
                    )
                }
                return CallKeyState(
                    callId = callId,
                    epoch = epochFromBytes(plaintext.copyOfRange(0, 8)),
                    key = plaintext.copyOfRange(8, JOIN_DISTRIBUTION_LEN),
                )
            } finally {
                plaintext.fill(0)
            }
        }

        /**
         * The key that wraps a join distribution for one joiner.
         *
         * Derived from the pairwise session secret the distributor shares with that joiner, under
         * its own label and with the call id as salt — a third purpose that secret serves, so it
         * must not share a label (or a salt pairing) with the ratchet or the call key itself.
         */
        private fun joinWrappingKey(sessionSecret: ByteArray, callId: Id): SymmetricKey {
            val derived = Kdf.derive(sessionSecret, idToBytes(callId), Kdf.LABEL_CALL_JOIN, CALL_KEY_LEN)
            // `fromBytes` copies; zeroing the derivation's buffer keeps the only live copy inside
            // the wrapper.
            val copy = derived.copyOf()
            derived.fill(0)
            return SymmetricKey.fromBytes(copy)
        }

        private fun epochToBytes(epoch: Long): ByteArray = byteArrayOf(
            (epoch ushr 56).toByte(),
            (epoch ushr 48).toByte(),
            (epoch ushr 40).toByte(),
            (epoch ushr 32).toByte(),
            (epoch ushr 24).toByte(),
            (epoch ushr 16).toByte(),
            (epoch ushr 8).toByte(),
            epoch.toByte(),
        )

        private fun epochFromBytes(bytes: ByteArray): Long {
            var value = 0L
            for (byte in bytes) value = (value shl 8) or (byte.toLong() and 0xFF)
            return value
        }
    }
}
