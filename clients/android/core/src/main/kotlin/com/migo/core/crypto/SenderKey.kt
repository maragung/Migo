package com.migo.core.crypto

import com.migo.core.wire.ByteAccumulator
import com.migo.core.wire.Varint

/**
 * Sender keys — group messaging without quadratic cost.
 *
 * A pairwise Double Ratchet in a 200-member group means encrypting every message 199 times. At
 * community-messenger-era group sizes that is the difference between a message that sends and a message that times
 * out on a 2G connection.
 *
 * Instead, each sender keeps one symmetric chain per group. The chain key is distributed once to each
 * member *over the pairwise E2E channels*, so the server never sees it, and after that a message is
 * encrypted once and fanned out to everyone. Cost per message becomes O(1) in the sender's work and
 * O(1) in bandwidth, with the O(n) cost paid only when the key distribution changes.
 *
 * # What this gives up, and what replaces it
 *
 * A sender key has forward secrecy — chain keys advance and old ones are deleted — but no
 * post-compromise security. Stealing a member's current chain key lets the thief read that sender's
 * future messages until the key is replaced. The ratchet cannot heal on its own here, because there
 * is no pairwise DH exchange to mix in.
 *
 * The replacement is rotation, and it is not optional:
 *
 * * When a member **leaves or is removed**, every remaining sender distributes a fresh chain.
 *   Otherwise the departed member keeps reading the group.
 * * After [SenderKeyState.MAX_MESSAGES_PER_CHAIN] messages, so a compromise has a bounded window
 *   even in a group where nobody ever leaves.
 *
 * Every rotation is labeled by a `groupKeyEpoch` that rises monotonically with it, and the number
 * travels in the distribution. The receiver's half of "the epoch never regresses" is
 * [ReceiverKeyState.adopt]: a distribution that does not advance the generation the receiver
 * already holds is refused rather than installed, so a member who re-sends an old distribution
 * cannot strand a peer on a dead chain.
 *
 * Rotation on removal is a correctness requirement, not a policy knob. A group implementation that
 * skips it has a member who left in March still reading messages in August.
 *
 * # Signing
 *
 * Symmetric keys prove only that *somebody in the group* wrote the message — every member holds the
 * chain key, so any member could forge another's message. Each message therefore carries an Ed25519
 * signature from the sender's identity key. Without it, group authorship is unverifiable, which in a
 * moderation context means a member can fabricate a message attributed to someone else.
 *
 * This mirrors `server/crates/migo-crypto/src/sender_key.rs`, down to verifying the signature before
 * deriving any key.
 */

/** Length of a group chain key. */
internal const val CHAIN_KEY_LEN = 32

/** Domain separator for a group message signature. */
private val GROUP_DOMAIN = "migo-sender-key-v1".toByteArray(Charsets.UTF_8)

/**
 * The distribution message a sender hands to each group member.
 *
 * Travels inside the pairwise E2E channel, never in the clear, and never through a code path that
 * could log it. The chain key is secret material held in a private field, exposed only through
 * [exposeChainKey]; everything else in this type is not secret.
 */
class SenderKeyDistribution(
    /**
     * The membership generation this chain belongs to. Rises on every rotation, never repeats,
     * never regresses — and travels *in* the distribution, because the receiver's whole defence
     * against a stale one is comparing this number against the generation it already holds.
     */
    val groupKeyEpoch: Long,
    /** Which chain this is, so a rotation can be distinguished from a resend. */
    val chainId: Long,
    /**
     * The message number the chain key corresponds to.
     *
     * A member who joins mid-conversation receives the chain key as of *now*, not from the
     * beginning. That is deliberate: a new member must not be able to decrypt history they were not
     * present for.
     */
    val messageNumber: Long,
    private val chainKey: ByteArray,
    /** The sender's identity, for verifying its signatures. */
    val identity: IdentityPublic,
) {
    init {
        requireU32(groupKeyEpoch, "group key epoch")
        requireU32(chainId, "chain id")
        requireU32(messageNumber, "message number")
        if (chainKey.size != CHAIN_KEY_LEN) {
            throw CryptoError.badLength("group chain key", CHAIN_KEY_LEN, chainKey.size)
        }
    }

    /**
     * Borrows the chain key. The greppable audit point for this secret leaving the type.
     *
     * Returns the live buffer, as [SymmetricKey.expose] does; the only callers,
     * [ReceiverKeyState.accept] and [ReceiverKeyState.adopt], copy it into their own state
     * immediately.
     */
    fun exposeChainKey(): ByteArray = chainKey

    /** Zeroes the chain key once the distribution has been handed to every member. */
    fun destroy() {
        chainKey.fill(0)
    }

    /** Never the key. */
    override fun toString(): String =
        "SenderKeyDistribution(group_key_epoch: $groupKeyEpoch, chain_id: $chainId, " +
            "message_number: $messageNumber, chain_key: ***)"
}

/** The header on a group message. */
class SenderKeyHeader(
    /** Which chain of the sender's this message belongs to. */
    val chainId: Long,
    /** Index within that chain. */
    val messageNumber: Long,
) {
    init {
        requireU32(chainId, "chain id")
        requireU32(messageNumber, "message number")
    }

    companion object {
        /** Encoded length: two big-endian `u32`s. */
        const val ENCODED_LEN = 8

        /** Parses a header, rejecting anything not exactly [ENCODED_LEN] bytes. */
        fun parse(bytes: ByteArray): SenderKeyHeader {
            if (bytes.size != ENCODED_LEN) throw CryptoError.malformedHeader()
            return SenderKeyHeader(readU32Be(bytes, 0), readU32Be(bytes, 4))
        }
    }

    /** Serialises the header. Fixed-width, because it is authenticated. */
    fun toBytes(): ByteArray {
        val out = ByteArray(ENCODED_LEN)
        putU32Be(out, 0, chainId)
        putU32Be(out, 4, messageNumber)
        return out
    }

    override fun toString(): String =
        "SenderKeyHeader(chain_id: $chainId, message_number: $messageNumber)"
}

/** A sealed group message. */
class SenderKeyMessage(
    /** Chain and position. */
    val header: SenderKeyHeader,
    /** AEAD output, without a nonce prefix — the nonce is derived. */
    val ciphertext: ByteArray,
    /** The sender's signature over the header and the ciphertext. */
    val signature: ByteArray,
)

/** A group message key and the nonce derived alongside it. */
private class GroupMessageKey(val key: SymmetricKey, val nonce: ByteArray)

/** Key material for one skipped group message, awaiting late delivery. */
private class GroupStashedKey(val key: ByteArray, val nonce: ByteArray)

/** The sending half: one per group, held by the sender. */
class SenderKeyState private constructor(
    private val chainId: Long,
    private val chainKey: ByteArray,
    private var messageNumber: Long,
) {
    companion object {
        /** Messages a single chain may produce before it must be rotated. */
        const val MAX_MESSAGES_PER_CHAIN = 2_000L

        /** Starts a fresh chain with a random chain key from the platform CSPRNG. */
        fun create(chainId: Long): SenderKeyState {
            requireU32(chainId, "chain id")
            return SenderKeyState(chainId, Csprng.bytes(CHAIN_KEY_LEN), 0)
        }

        /**
         * Restores a sending chain from [snapshot].
         *
         * Every failure is [CryptoError.malformedHeader], for the reason given on
         * [RatchetSession.restore]: the [CryptoErrorKind] variants are a contract shared with
         * `migo-crypto` and `@migo/crypto` through `shared/protocol/vectors/crypto`, and a new
         * variant here would put this client one member ahead of both.
         *
         * Restoring a *sending* chain is the operation with teeth. The message number is what makes
         * each message key unique, so a chain restored one step behind where it actually got to will
         * re-derive a key it has already used, and every recipient will see two different messages
         * sealed under the same key and nonce -- which is the one failure XChaCha20-Poly1305 gives no
         * protection against. A store must therefore save after every send, and must never hand the
         * same snapshot to two live senders.
         */
        fun restore(bytes: ByteArray): SenderKeyState {
            val cursor = Cursor(bytes)
            if (cursor.u8() != STATE_SNAPSHOT_VERSION) throw CryptoError.malformedHeader()
            val chainId = cursor.varintU32()
            val chainKey = cursor.take(CHAIN_KEY_LEN)
            val messageNumber = cursor.varintU32()
            if (cursor.rest().isNotEmpty()) throw CryptoError.malformedHeader()
            return SenderKeyState(chainId, chainKey, messageNumber)
        }
    }

    /** Which chain this state represents. */
    fun chainId(): Long = chainId

    /** How many messages this chain has produced. */
    fun messageNumber(): Long = messageNumber

    /**
     * True once the chain has reached its rotation bound.
     *
     * Callers check this and rotate. It is a bound on the blast radius of a compromise, so ignoring
     * it means the window is the lifetime of the group.
     */
    fun needsRotation(): Boolean = messageNumber >= MAX_MESSAGES_PER_CHAIN

    /**
     * Builds the distribution message for the chain's current position.
     *
     * The chain key is copied into the distribution, not shared: this state's key advances with every
     * message, and a distribution that aliased it would silently change under the recipient.
     *
     * The epoch is passed in rather than held: this class tracks one chain, while the membership
     * generation belongs to the conversation around it (GroupCrypto owns the numbering, as the
     * Rust sender state owns its own). The distribution names the generation so the receiver can
     * refuse a stale one before any key material moves — see [ReceiverKeyState.adopt].
     */
    fun distribution(groupKeyEpoch: Long, identity: IdentitySecret): SenderKeyDistribution =
        SenderKeyDistribution(groupKeyEpoch, chainId, messageNumber, chainKey.copyOf(), identity.public())

    /** Encrypts and signs a group message. */
    fun encrypt(
        identity: IdentitySecret,
        groupContext: ByteArray,
        plaintext: ByteArray,
    ): SenderKeyMessage {
        if (needsRotation()) {
            // Refusing rather than silently continuing: the caller has a rotation path, and a chain
            // that runs past its bound is exactly the state the bound exists to prevent.
            throw CryptoError.keyAlreadyUsed()
        }
        val header = SenderKeyHeader(chainId, messageNumber)
        val messageKey = advanceSenderChain(chainKey)
        messageNumber += 1

        val aad = groupAssociatedData(groupContext, header)
        val sealed = Aead.sealWithNonce(messageKey.key, messageKey.nonce, aad, plaintext)
        messageKey.key.destroy()
        messageKey.nonce.fill(0)
        val ciphertext = sealed.copyOfRange(AEAD_NONCE_LEN, sealed.size)

        // Sign header and ciphertext together, so neither can be moved onto the other. Group
        // authorship depends on this signature and nothing else.
        val signed = concatBytes(aad, ciphertext)
        return SenderKeyMessage(header, ciphertext, identity.sign(GROUP_DOMAIN, signed))
    }

    /** Zeroes the chain key. Called when the chain is rotated out. */
    fun destroy() {
        chainKey.fill(0)
    }

    /**
     * Serialises the sending chain for a store that will seal it.
     *
     * ```text
     * u8      version         STATE_SNAPSHOT_VERSION
     * varint  chain_id
     * 32      chain_key
     * varint  message_number
     * ```
     *
     * The chain key is the group secret in the clear, so the same contract as
     * [RatchetSession.snapshot] applies: seal it under a Keystore-held key, keep it out of OS backup,
     * zero the array once sealed, never log it.
     *
     * The identity is not in here. A sending chain signs with whatever identity the caller passes to
     * [encrypt], which is the device's own long-term key held in the vault -- storing a second copy
     * beside every chain would be one more place for it to leak and one more thing to keep in step.
     */
    fun snapshot(): ByteArray {
        val out = ByteAccumulator(1 + 5 + CHAIN_KEY_LEN + 5)
        out.push(STATE_SNAPSHOT_VERSION)
        Varint.encodeU64(chainId, out)
        out.append(chainKey)
        Varint.encodeU64(messageNumber, out)
        return out.toByteArray()
    }

    /** Never the key. */
    override fun toString(): String =
        "SenderKeyState(chain_id: $chainId, message_number: $messageNumber)"
}

/** The receiving half: one per (group, sender) pair. */
class ReceiverKeyState private constructor(
    /**
     * The membership generation this state's chain belongs to. The baseline every later
     * distribution from this sender is measured against; only [adopt] moves it, and only forward.
     */
    private var groupKeyEpoch: Long,
    private var chainId: Long,
    private var chainKey: ByteArray,
    private var nextMessageNumber: Long,
    private var identity: IdentityPublic,
) {
    /**
     * Keys derived for messages that have not arrived yet, oldest first.
     *
     * A [LinkedHashMap] keyed by message number: insertion-ordered, so the first entry is the oldest,
     * which is the eviction order the reference maintains with an array and `shift()`.
     */
    private val skipped = LinkedHashMap<Long, GroupStashedKey>()

    companion object {
        /** How far ahead of the receiver a message may claim to be. */
        const val MAX_CHAIN_GAP = 1_000L

        /**
         * Accepts the *first* distribution a receiver sees from this sender.
         *
         * The first distribution is the baseline: there is no earlier generation to compare
         * against, so any epoch is accepted here and every later one is measured from it.
         * Freshness of the baseline is vouched for by the pairwise channel it arrived through.
         * Once state exists, use [adopt] — this constructor never refuses anything and must not
         * be used to replace held state.
         */
        fun accept(distribution: SenderKeyDistribution): ReceiverKeyState =
            ReceiverKeyState(
                distribution.groupKeyEpoch,
                distribution.chainId,
                distribution.exposeChainKey().copyOf(),
                distribution.messageNumber,
                distribution.identity,
            )

        /**
         * Restores a receiving chain from [snapshot].
         *
         * Every failure is [CryptoError.malformedHeader], as on [RatchetSession.restore].
         *
         * The sender's identity comes back with the chain, and that matters more than it looks: the
         * signature check in [decrypt] is what establishes group authorship, and a receiver that
         * restored without it would have to re-accept a distribution message to learn who it is
         * listening to -- which is exactly the moment a hostile server would offer a different
         * identity for the same chain.
         */
        fun restore(bytes: ByteArray): ReceiverKeyState {
            val cursor = Cursor(bytes)
            if (cursor.u8() != STATE_SNAPSHOT_VERSION) throw CryptoError.malformedHeader()
            val chainId = cursor.varintU32()
            val chainKey = cursor.take(CHAIN_KEY_LEN)
            val nextMessageNumber = cursor.varintU32()
            val identity = try {
                IdentityPublic.parse(cursor.take(IDENTITY_PUBLIC_LEN))
            } catch (_: CryptoError) {
                // Folded into the one reason: a stored identity that is not a valid pair of points
                // is a damaged file, and the caller's move is the same either way.
                throw CryptoError.malformedHeader()
            }

            val stashedCount = cursor.varintU32()
            // Restores the class invariant rather than trusting the file: decrypt trims the map to
            // MAX_CHAIN_GAP, so a snapshot claiming more was not written by this code.
            if (stashedCount > MAX_CHAIN_GAP) throw CryptoError.malformedHeader()
            val stashed = LinkedHashMap<Long, GroupStashedKey>()
            repeat(stashedCount.toInt()) {
                val number = cursor.varintU32()
                stashed[number] = GroupStashedKey(
                    cursor.take(AEAD_KEY_LEN),
                    cursor.take(AEAD_NONCE_LEN),
                )
            }

            // The trailing epoch is optional: snapshots written before the field existed end at
            // the skipped keys. Such a receiver restores with a baseline of 0 and re-baselines
            // from the next distribution it adopts, which never names an epoch lower than the
            // chain it belongs to. Anything further after the varint is refused, as before.
            val trailing = cursor.rest()
            val groupKeyEpoch = if (trailing.isEmpty()) {
                0L
            } else {
                val epochCursor = Cursor(trailing)
                val epoch = epochCursor.varintU32()
                if (epochCursor.rest().isNotEmpty()) throw CryptoError.malformedHeader()
                epoch
            }

            val state = ReceiverKeyState(groupKeyEpoch, chainId, chainKey, nextMessageNumber, identity)
            state.skipped.putAll(stashed)
            return state
        }
    }

    /**
     * Adopts a distribution into state that already tracks this sender.
     *
     * This is the receiver's half of "the epoch never regresses". Four cases, in the order they
     * are decided:
     *
     * * **Older epoch** — refused with [CryptoError.keyAlreadyUsed]. This is the
     *   stale-distribution attack: a member re-sends a distribution from before the last
     *   membership change, and if it were installed the receiver would sit on a chain nothing
     *   further is sealed under. The held state is untouched, so the receiver keeps reading on
     *   the current chain and can still re-sync forward.
     * * **Same epoch, same chain** — a re-send of the distribution the receiver already holds.
     *   Not a regression, and not a reason to re-install either: re-installing would rewind the
     *   receiver's position inside the live chain. Succeeds as a no-op.
     * * **Same epoch, different chain** — refused. A rotation always raises the epoch, so a
     *   second chain at one generation is a swap, not a rotation.
     * * **Newer epoch** — installed. The old chain key and every skipped key are zeroed before
     *   the new state lands, and the skipped window is emptied because message numbers restart
     *   with the chain.
     *
     * The boundary of this guarantee: refusing a stale distribution protects liveness and
     * correctness of the receiver's chain, not confidentiality — the harm refused is a stranded
     * receiver, which is a denial of message delivery until re-sync, not a disclosure.
     */
    fun adopt(distribution: SenderKeyDistribution) {
        if (distribution.groupKeyEpoch < groupKeyEpoch) {
            throw CryptoError.keyAlreadyUsed()
        }
        if (distribution.groupKeyEpoch == groupKeyEpoch) {
            if (distribution.chainId == chainId) {
                return
            }
            throw CryptoError.keyAlreadyUsed()
        }
        destroy()
        groupKeyEpoch = distribution.groupKeyEpoch
        chainId = distribution.chainId
        chainKey = distribution.exposeChainKey().copyOf()
        nextMessageNumber = distribution.messageNumber
        identity = distribution.identity
    }

    /** The membership generation this state holds. */
    fun groupKeyEpoch(): Long = groupKeyEpoch

    /** Which chain this state tracks. */
    fun chainId(): Long = chainId

    /** How many out-of-order keys are retained. */
    fun skippedCount(): Int = skipped.size

    /**
     * Verifies and decrypts a group message.
     *
     * The signature is checked *before* any key derivation. A forged message should cost the receiver
     * one signature verification, not a thousand KDF steps, and checking the cheap authentication
     * first is what makes that true.
     */
    fun decrypt(groupContext: ByteArray, message: SenderKeyMessage): ByteArray {
        if (message.header.chainId != chainId) {
            // A different chain means a rotation this receiver has not been told about. The caller
            // fetches the new distribution message and retries.
            throw CryptoError.noSession()
        }
        val aad = groupAssociatedData(groupContext, message.header)
        val signed = concatBytes(aad, message.ciphertext)
        identity.verify(GROUP_DOMAIN, signed, message.signature)

        val number = message.header.messageNumber
        val stashed = skipped.remove(number)
        if (stashed != null) {
            val key = SymmetricKey.fromBytes(stashed.key)
            stashed.key.fill(0)
            try {
                return Aead.openWithNonce(key, stashed.nonce, aad, message.ciphertext)
            } finally {
                key.destroy()
                stashed.nonce.fill(0)
            }
        }
        if (number < nextMessageNumber) throw CryptoError.keyAlreadyUsed()
        val gap = number - nextMessageNumber
        if (gap > MAX_CHAIN_GAP) throw CryptoError.chainGapTooLarge()

        val pending = ArrayList<Pair<Long, GroupMessageKey>>()
        var offset = 0L
        while (offset < gap) {
            pending.add(Pair(nextMessageNumber + offset, advanceSenderChain(chainKey)))
            offset += 1
        }
        val target = advanceSenderChain(chainKey)
        val plaintext = try {
            Aead.openWithNonce(target.key, target.nonce, aad, message.ciphertext)
        } finally {
            target.key.destroy()
            target.nonce.fill(0)
        }

        // Only now, once the message is proven genuine, mutate the tracked position.
        for ((pendingNumber, key) in pending) {
            val exposed = key.key.expose()
            skipped[pendingNumber] = GroupStashedKey(exposed.copyOf(), key.nonce.copyOf())
            key.key.destroy()
            key.nonce.fill(0)
        }
        // Bounded, oldest evicted first, for the same reason as the pairwise ratchet: a sender who
        // never fills the gaps must not grow this forever.
        while (skipped.size > MAX_CHAIN_GAP) {
            val iterator = skipped.entries.iterator()
            if (!iterator.hasNext()) break
            val evicted = iterator.next()
            evicted.value.key.fill(0)
            evicted.value.nonce.fill(0)
            iterator.remove()
        }
        nextMessageNumber = number + 1
        return plaintext
    }

    /** Zeroes the chain key and every retained skipped key. */
    fun destroy() {
        chainKey.fill(0)
        for (entry in skipped.values) {
            entry.key.fill(0)
            entry.nonce.fill(0)
        }
        skipped.clear()
    }

    /**
     * Serialises the receiving chain for a store that will seal it.
     *
     * ```text
     * u8      version              STATE_SNAPSHOT_VERSION
     * varint  chain_id
     * 32      chain_key
     * varint  next_message_number
     * 64      sender_identity      IdentityPublic.toBytes()
     * varint  skipped_count
     * repeat skipped_count times:
     *   varint  message_number
     *   32      message_key
     *   24      nonce
     * varint  group_key_epoch      appended last; absent in files written before the field existed
     * ```
     *
     * Chain key and retained message keys are secrets in the clear, so the same contract as
     * [RatchetSession.snapshot] applies. The identity is public material and is the one field here
     * that would be harmless in a log; it is still not logged, because a per-sender group identity is
     * metadata about who talks in which group (brief section 174).
     *
     * The skipped keys are kept for the reason given on [RatchetSession.snapshot]: dropping them
     * would make a group message still in flight permanently undecryptable, and the file is sealed
     * and rewritten on every save, so a key that gets used leaves the file at the next save.
     *
     * The epoch is appended rather than the version bumped because STATE_SNAPSHOT_VERSION is shared
     * with the pairwise ratchet: bumping it would invalidate every ratchet snapshot on every device,
     * while an optional trailing field leaves old receiver snapshots readable at a baseline of 0.
     */
    fun snapshot(): ByteArray {
        val out = ByteAccumulator(
            1 + 5 + CHAIN_KEY_LEN + 5 + IDENTITY_PUBLIC_LEN + 5 +
                skipped.size * (5 + AEAD_KEY_LEN + AEAD_NONCE_LEN) + 5,
        )
        out.push(STATE_SNAPSHOT_VERSION)
        Varint.encodeU64(chainId, out)
        out.append(chainKey)
        Varint.encodeU64(nextMessageNumber, out)
        out.append(identity.toBytes())

        Varint.encodeU64(skipped.size.toLong(), out)
        for ((number, stashed) in skipped) {
            Varint.encodeU64(number, out)
            out.append(stashed.key)
            out.append(stashed.nonce)
        }
        Varint.encodeU64(groupKeyEpoch, out)
        return out.toByteArray()
    }

    /** Never a key. */
    override fun toString(): String =
        "ReceiverKeyState(chain_id: $chainId, next_message_number: $nextMessageNumber, " +
            "skipped: ${skipped.size})"
}

/**
 * Group context and header, authenticated on every message.
 *
 * The group id is in here so a ciphertext cannot be lifted from one group and replayed into another
 * where the same sender is also a member.
 */
private fun groupAssociatedData(groupContext: ByteArray, header: SenderKeyHeader): ByteArray =
    concatBytes(groupContext, header.toBytes())

/**
 * Advances the chain and yields the message key and nonce.
 *
 * Mutates [chain] in place, as the pairwise ratchet's equivalent does: the previous chain key is
 * overwritten, which is the mechanism of forward secrecy.
 */
private fun advanceSenderChain(chain: ByteArray): GroupMessageKey {
    val (next, material) = Kdf.derivePair(chain, null, Kdf.LABEL_SENDER_CHAIN, 32, 56)
    System.arraycopy(next, 0, chain, 0, next.size)
    next.fill(0)

    val keyBytes = material.copyOfRange(0, AEAD_KEY_LEN)
    val nonce = material.copyOfRange(AEAD_KEY_LEN, material.size)
    material.fill(0)
    val key = SymmetricKey.fromBytes(keyBytes)
    keyBytes.fill(0)
    return GroupMessageKey(key, nonce)
}
