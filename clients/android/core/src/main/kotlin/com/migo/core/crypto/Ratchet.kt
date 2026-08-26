package com.migo.core.crypto

import com.migo.core.wire.ByteAccumulator
import com.migo.core.wire.Varint

/**
 * The plaintext header attached to every ratchet message.
 *
 * All three fields are public and all three are authenticated as associated data, so tampering with
 * them makes decryption fail rather than succeed differently.
 */
class RatchetHeader private constructor(
    private val keyBytes: ByteArray,
    /** How many messages the sender sent in its previous chain. */
    val previousChainLength: Long,
    /** Index of this message within the sender's current chain. */
    val messageNumber: Long,
) {
    /** The sender's current ratchet public key. */
    val ratchetKey: ByteArray get() = keyBytes.copyOf()

    companion object {
        /** Encoded length: key, then two big-endian `u32`s. */
        const val ENCODED_LEN = PUBLIC_KEY_LEN + 8

        /**
         * Builds a header.
         *
         * [previousChainLength] lets the receiver derive the keys it never saw from the *old* chain
         * before moving to the new one. Without it, a DH step would silently drop any message still
         * in flight from the previous chain.
         */
        fun of(ratchetKey: ByteArray, previousChainLength: Long, messageNumber: Long): RatchetHeader {
            if (ratchetKey.size != PUBLIC_KEY_LEN) {
                throw CryptoError.badLength("ratchet key", PUBLIC_KEY_LEN, ratchetKey.size)
            }
            requireU32(previousChainLength, "previous chain length")
            requireU32(messageNumber, "message number")
            return RatchetHeader(ratchetKey.copyOf(), previousChainLength, messageNumber)
        }

        /** Parses a header, rejecting anything not exactly [ENCODED_LEN] bytes. */
        fun parse(bytes: ByteArray): RatchetHeader {
            if (bytes.size != ENCODED_LEN) throw CryptoError.malformedHeader()
            return RatchetHeader(
                bytes.copyOfRange(0, PUBLIC_KEY_LEN),
                readU32Be(bytes, PUBLIC_KEY_LEN),
                readU32Be(bytes, PUBLIC_KEY_LEN + 4),
            )
        }
    }

    /**
     * Serialises the header.
     *
     * Fixed-width big-endian rather than varints, deliberately. This byte string is authenticated, so
     * it must be canonical: a varint with two valid encodings would give one header two valid
     * authentication tags.
     */
    fun toBytes(): ByteArray {
        val out = ByteArray(ENCODED_LEN)
        System.arraycopy(keyBytes, 0, out, 0, PUBLIC_KEY_LEN)
        putU32Be(out, PUBLIC_KEY_LEN, previousChainLength)
        putU32Be(out, PUBLIC_KEY_LEN + 4, messageNumber)
        return out
    }

    override fun toString(): String =
        "RatchetHeader(ratchet_key: ${hexOf(keyBytes)}, " +
            "previous_chain_length: $previousChainLength, message_number: $messageNumber)"
}

/** A sealed ratchet message: the public header, and the AEAD output without a nonce prefix. */
class RatchetMessage(val header: RatchetHeader, val ciphertext: ByteArray)

/** A message key and the nonce derived alongside it. */
private class RatchetMessageKey(val key: SymmetricKey, val nonce: ByteArray)

/** Key material for one skipped message, awaiting late delivery. */
private class StashedKey(val key: ByteArray, val nonce: ByteArray)

/**
 * A Double Ratchet session for one device pair.
 *
 * X3DH produces one shared secret. If that secret encrypted every message, then stealing a phone
 * once would decrypt the entire conversation history, and every future message too. This turns that
 * one secret into a fresh key per message, with two properties that matter to a real user:
 *
 * * **Forward secrecy** — a key compromised today does not decrypt yesterday's messages, because
 *   yesterday's keys were deleted after use.
 * * **Post-compromise security** — once the attacker loses access, the ratchet heals. New
 *   Diffie-Hellman material from the other side is mixed in on every turn of the conversation, and
 *   the attacker cannot follow.
 *
 * Two ratchets combine. The **DH ratchet** turns when the conversation turns: each side attaches a
 * fresh public key, and both root keys advance by mixing in a new DH output. The **symmetric chain**
 * advances per message within one DH step, which is cheap and gives forward secrecy between
 * consecutive messages without a round trip.
 *
 * There is no copy of this object. Two copies would each advance independently and each believe it
 * had already used a key the other had not; in Rust the type is simply not `Clone`, and here the
 * private mutable state and the absence of a copy path serve the same purpose.
 *
 * This mirrors `server/crates/migo-crypto/src/ratchet.rs` step for step — including where the chain
 * key is advanced in place versus on a local copy, which is the difference between a forged frame
 * that is merely rejected and one that corrupts the session.
 */
class RatchetSession private constructor(
    private var rootKey: ByteArray,
    private val associatedData: ByteArray,
) {
    /** Our current ratchet pair. Null for a responder that has not yet sent. */
    private var sendingPair: KeyPair? = null

    /** The peer's latest ratchet key, once seen. */
    private var receivingKey: ByteArray? = null
    private var sendingChain: ByteArray? = null
    private var receivingChain: ByteArray? = null
    private var sentCount = 0L
    private var receivedCount = 0L
    private var previousSendingCount = 0L

    /**
     * Keys for skipped messages, keyed by `hex(ratchetKey):messageNumber`.
     *
     * A [LinkedHashMap] rather than a map plus a separate order list: it iterates in insertion order,
     * so the first entry is always the oldest, and re-inserting an existing key leaves its position
     * alone. That is exactly the eviction order the Rust and TypeScript versions maintain by hand.
     */
    private val skipped = LinkedHashMap<String, StashedKey>()

    companion object {
        /** Maximum number of messages a single header may claim to have skipped. */
        const val MAX_CHAIN_GAP = 2_000L

        /** Maximum number of skipped message keys retained across a session. */
        const val MAX_SKIPPED_KEYS = 2_000

        /**
         * Starts a session as the initiator, who already knows the peer's prekey.
         *
         * The initiator can send immediately: it performs the first DH step against the peer's signed
         * prekey, which is exactly the key the peer published for this purpose. [pair] defaults to a
         * fresh one and is a parameter only so a test vector can pin it.
         */
        fun initiator(
            seed: SessionSeed,
            peerSignedPrekey: ByteArray,
            pair: KeyPair = KeyPair.generate(),
        ): RatchetSession {
            val session = RatchetSession(seed.exposeSharedSecret(), seed.associatedData)
            val dh = pair.diffieHellman(peerSignedPrekey)
            val (rootKey, chain) =
                Kdf.derivePair(dh, session.rootKey, Kdf.LABEL_RATCHET_ROOT, 32, 32)
            dh.fill(0)
            session.rootKey.fill(0)
            session.rootKey = rootKey
            session.sendingChain = chain
            session.sendingPair = pair
            session.receivingKey = peerSignedPrekey.copyOf()
            return session
        }

        /**
         * Starts a session as the responder, whose signed prekey pair is the first ratchet key.
         *
         * The responder cannot send until it has received, because until then it has no peer ratchet
         * key to step against. That is not a limitation in practice: the responder is by definition
         * the side that received the first message.
         */
        fun responder(seed: SessionSeed, signedPrekeyPair: KeyPair): RatchetSession {
            val session = RatchetSession(seed.exposeSharedSecret(), seed.associatedData)
            session.sendingPair = signedPrekeyPair
            return session
        }

        /**
         * Restores a session from [snapshot].
         *
         * Every failure is [CryptoError.malformedHeader], which is deliberate rather than lazy. The
         * ten [CryptoErrorKind] variants are a contract shared with `migo-crypto` and `@migo/crypto`
         * through `shared/protocol/vectors/crypto`, so a `BadSnapshot` variant added here would put
         * this client one enum member ahead of the two implementations it has to agree with. And a
         * snapshot *is* a ratchet header, in the only sense the caller cares about: a byte layout
         * describing chain position and key material that did not parse. There is exactly one useful
         * response either way -- treat the stored session as gone and start a fresh one.
         *
         * Refuses trailing bytes. Forward compatibility is [STATE_SNAPSHOT_VERSION]'s job; a
         * snapshot with something after the last field is a partial write or a codec bug, and
         * accepting it would let a torn file restore as a valid-looking session that is quietly a
         * few messages behind.
         *
         * A restored session is the same session, not a copy of one: the counters, the chain keys and
         * the retained out-of-order keys all come back. That is what makes it safe to restore -- two
         * live sessions from one snapshot would each advance independently, reuse a message key and
         * destroy the guarantee the ratchet exists for, so a store must hand out one and forget it.
         */
        fun restore(bytes: ByteArray): RatchetSession {
            val cursor = Cursor(bytes)
            if (cursor.u8() != STATE_SNAPSHOT_VERSION) throw CryptoError.malformedHeader()

            val associatedData = cursor.take(cursor.varintU32().toInt())
            val session = RatchetSession(cursor.take(SHARED_SECRET_LEN), associatedData)

            val flags = cursor.u8()
            // An unknown bit means a writer this build does not understand, and the fields it
            // implies are not in the stream. Guessing would read the next field from the wrong
            // offset, which is how a chain key becomes half a counter.
            if (flags and FLAG_ALL.inv() != 0) throw CryptoError.malformedHeader()

            if (flags and FLAG_SENDING_PAIR != 0) {
                val seed = cursor.take(SEED_LEN)
                session.sendingPair = KeyPair.fromSeed(seed)
                seed.fill(0)
            }
            if (flags and FLAG_RECEIVING_KEY != 0) {
                session.receivingKey = cursor.take(PUBLIC_KEY_LEN)
            }
            if (flags and FLAG_SENDING_CHAIN != 0) {
                session.sendingChain = cursor.take(CHAIN_KEY_LEN)
            }
            if (flags and FLAG_RECEIVING_CHAIN != 0) {
                session.receivingChain = cursor.take(CHAIN_KEY_LEN)
            }

            session.sentCount = cursor.varintU32()
            session.receivedCount = cursor.varintU32()
            session.previousSendingCount = cursor.varintU32()

            val stashedCount = cursor.varintU32()
            // Restores the class invariant rather than trusting the file: the live session caps the
            // map at MAX_SKIPPED_KEYS, so a snapshot claiming more was not written by this code.
            if (stashedCount > MAX_SKIPPED_KEYS.toLong()) throw CryptoError.malformedHeader()
            repeat(stashedCount.toInt()) {
                val mapKey = String(cursor.take(cursor.varintU32().toInt()), Charsets.US_ASCII)
                session.skipped[mapKey] = StashedKey(
                    cursor.take(AEAD_KEY_LEN),
                    cursor.take(AEAD_NONCE_LEN),
                )
            }

            if (cursor.rest().isNotEmpty()) throw CryptoError.malformedHeader()
            return session
        }

        // Which of the four optional fields follow the flag byte. A bitmask rather than four
        // presence bytes because the four are correlated: a responder that has not sent has none of
        // them, an established session has all four, and one byte says which case this is.
        private const val FLAG_SENDING_PAIR = 0x01
        private const val FLAG_RECEIVING_KEY = 0x02
        private const val FLAG_SENDING_CHAIN = 0x04
        private const val FLAG_RECEIVING_CHAIN = 0x08
        private const val FLAG_ALL = 0x0f
    }

    /** Number of messages sent in the current chain. */
    fun sentCount(): Long = sentCount

    /** Number of messages received in the current chain. */
    fun receivedCount(): Long = receivedCount

    /** How many skipped keys are currently retained. */
    fun skippedCount(): Int = skipped.size

    /**
     * Encrypts [plaintext].
     *
     * The ciphertext has no nonce prefix: the nonce is derived from the message key, which the
     * receiver reconstructs from the header, so it is never transmitted and cannot be tampered with.
     */
    fun encrypt(plaintext: ByteArray): RatchetMessage {
        val pair = sendingPair ?: throw CryptoError.noSession()
        val chain = sendingChain ?: throw CryptoError.noSession()

        val messageKey = advanceRatchetChain(chain)
        val header = RatchetHeader.of(pair.public(), previousSendingCount, sentCount)
        sentCount += 1

        val aad = concatBytes(associatedData, header.toBytes())
        val sealed = Aead.sealWithNonce(messageKey.key, messageKey.nonce, aad, plaintext)
        messageKey.key.destroy()
        messageKey.nonce.fill(0)
        // sealWithNonce prefixes the nonce; the receiver derives it, so drop it.
        return RatchetMessage(header, sealed.copyOfRange(AEAD_NONCE_LEN, sealed.size))
    }

    /**
     * Decrypts a message.
     *
     * Advances the ratchet only when decryption succeeds. A forged message that claimed a new ratchet
     * key would otherwise destroy the session's ability to decrypt genuine ones — a denial of service
     * from anyone who can inject a frame.
     */
    fun decrypt(header: RatchetHeader, ciphertext: ByteArray): ByteArray {
        val aad = concatBytes(associatedData, header.toBytes())
        val headerKey = header.ratchetKey

        // A late message whose key was already derived and set aside. Removed before the open, not
        // after: a stored key is spent the moment it is used, which is what makes a replayed frame
        // fail rather than deliver the same message twice.
        val stashed = skipped.remove(skippedMapKey(headerKey, header.messageNumber))
        if (stashed != null) {
            val key = SymmetricKey.fromBytes(stashed.key)
            stashed.key.fill(0)
            try {
                return Aead.openWithNonce(key, stashed.nonce, aad, ciphertext)
            } finally {
                key.destroy()
                stashed.nonce.fill(0)
            }
        }

        val current = receivingKey
        val isNewChain = current == null || !current.contentEquals(headerKey)
        return if (isNewChain) {
            stepReceivingChain(header, headerKey, aad, ciphertext)
        } else {
            decryptInCurrentChain(header, headerKey, aad, ciphertext)
        }
    }

    /** Handles a message that belongs to the chain we are already tracking. */
    private fun decryptInCurrentChain(
        header: RatchetHeader,
        headerKey: ByteArray,
        aad: ByteArray,
        ciphertext: ByteArray,
    ): ByteArray {
        if (header.messageNumber < receivedCount) {
            // Already consumed. The key was deleted on use, so this is either a replay or a
            // duplicate delivery; either way there is nothing to do.
            throw CryptoError.keyAlreadyUsed()
        }
        val gap = header.messageNumber - receivedCount
        if (gap > MAX_CHAIN_GAP) throw CryptoError.chainGapTooLarge()
        val chain = receivingChain ?: throw CryptoError.noSession()

        // Derive and stash the keys for anything skipped, then the key we want. This advances the
        // tracked receiving chain in place, exactly as the Rust version's `as_mut()` does.
        val pending = ArrayList<Pair<Long, RatchetMessageKey>>()
        var offset = 0L
        while (offset < gap) {
            pending.add(Pair(receivedCount + offset, advanceRatchetChain(chain)))
            offset += 1
        }
        val target = advanceRatchetChain(chain)
        val plaintext = try {
            Aead.openWithNonce(target.key, target.nonce, aad, ciphertext)
        } finally {
            target.key.destroy()
            target.nonce.fill(0)
        }

        // Only now, once the message is proven genuine, mutate session state.
        for ((number, key) in pending) stashSkipped(headerKey, number, key)
        receivedCount = header.messageNumber + 1
        return plaintext
    }

    /** Handles the first message of a new chain: turn the DH ratchet. */
    private fun stepReceivingChain(
        header: RatchetHeader,
        headerKey: ByteArray,
        aad: ByteArray,
        ciphertext: ByteArray,
    ): ByteArray {
        if (header.messageNumber > MAX_CHAIN_GAP ||
            header.previousChainLength > saturatingAddU32(MAX_CHAIN_GAP, receivedCount)
        ) {
            throw CryptoError.chainGapTooLarge()
        }
        val pair = sendingPair ?: throw CryptoError.noSession()

        // Finish the previous chain, so messages still in flight from it can be decrypted when they
        // arrive. This advances the *old* receiving chain in place; the new chain below is local
        // until it is proven, so a forged frame cannot corrupt what we already track.
        val leftovers = ArrayList<Triple<ByteArray, Long, RatchetMessageKey>>()
        val oldChain = receivingChain
        val previousKey = receivingKey
        if (oldChain != null && previousKey != null) {
            val remaining = saturatingSubU32(header.previousChainLength, receivedCount)
            if (remaining > MAX_CHAIN_GAP) throw CryptoError.chainGapTooLarge()
            var offset = 0L
            while (offset < remaining) {
                leftovers.add(
                    Triple(previousKey, receivedCount + offset, advanceRatchetChain(oldChain)),
                )
                offset += 1
            }
        }

        // Turn the DH ratchet: mix the peer's new key into the root key. `newChain` is a local copy
        // — nothing on this object is touched until the target message opens.
        val dh = pair.diffieHellman(headerKey)
        val (rootAfterReceive, newChain) =
            Kdf.derivePair(dh, rootKey, Kdf.LABEL_RATCHET_ROOT, 32, 32)
        dh.fill(0)

        // Derive the keys this new chain skipped, then the one we want.
        val pending = ArrayList<Pair<Long, RatchetMessageKey>>()
        var number = 0L
        while (number < header.messageNumber) {
            pending.add(Pair(number, advanceRatchetChain(newChain)))
            number += 1
        }
        val target = advanceRatchetChain(newChain)
        val plaintext = try {
            Aead.openWithNonce(target.key, target.nonce, aad, ciphertext)
        } finally {
            target.key.destroy()
            target.nonce.fill(0)
        }

        // Proven genuine: commit. Our own next chain steps too, with a fresh pair, which is what
        // makes the ratchet heal after a compromise.
        for ((ratchetKey, leftoverNumber, key) in leftovers) {
            stashSkipped(ratchetKey, leftoverNumber, key)
        }
        for ((pendingNumber, key) in pending) stashSkipped(headerKey, pendingNumber, key)
        rootKey.fill(0)
        rootKey = rootAfterReceive
        receivingChain = newChain
        receivingKey = headerKey.copyOf()
        receivedCount = header.messageNumber + 1
        previousSendingCount = sentCount
        sentCount = 0
        // The sending chain is left unset: it is derived lazily on the next send, against a pair
        // generated then, so a session that only receives never generates keys it does not use.
        sendingChain = null
        return plaintext
    }

    /**
     * Prepares the sending chain if a receive has invalidated it.
     *
     * Separated from [encrypt] so that the send path is not forced to touch the random source on
     * every message: the pair is only generated when the ratchet actually needs to turn.
     */
    fun prepareSend() {
        if (sendingChain != null) return
        val peerKey = receivingKey ?: throw CryptoError.noSession()
        val pair = KeyPair.generate()
        val dh = pair.diffieHellman(peerKey)
        val (newRootKey, chain) = Kdf.derivePair(dh, rootKey, Kdf.LABEL_RATCHET_ROOT, 32, 32)
        dh.fill(0)
        rootKey.fill(0)
        rootKey = newRootKey
        sendingChain = chain
        sendingPair = pair
    }

    /** Encrypts, turning the DH ratchet first if the last operation was a receive. */
    fun encryptNext(plaintext: ByteArray): RatchetMessage {
        prepareSend()
        return encrypt(plaintext)
    }

    /** Stores a skipped key, evicting the oldest once the bound is reached. */
    private fun stashSkipped(ratchetKey: ByteArray, number: Long, key: RatchetMessageKey) {
        while (skipped.size >= MAX_SKIPPED_KEYS) {
            // Oldest first: a message that has been missing longest is the least likely to still
            // arrive.
            val iterator = skipped.entries.iterator()
            if (!iterator.hasNext()) break
            val evicted = iterator.next()
            evicted.value.key.fill(0)
            evicted.value.nonce.fill(0)
            iterator.remove()
        }
        val exposed = key.key.expose()
        skipped[skippedMapKey(ratchetKey, number)] = StashedKey(exposed.copyOf(), key.nonce.copyOf())
        key.key.destroy()
        key.nonce.fill(0)
    }

    /**
     * Serialises the entire session, key material included, for a store that will seal it.
     *
     * ```text
     * u8      version                STATE_SNAPSHOT_VERSION
     * varint  associated_data_len
     * bytes   associated_data        both device identities, bound by X3DH
     * 32      root_key
     * u8      flags                  which of the next four fields are present
     * [32]    sending_pair_seed      KeyPair.exposeSeed(), if FLAG_SENDING_PAIR
     * [32]    receiving_key          the peer's latest ratchet key, if FLAG_RECEIVING_KEY
     * [32]    sending_chain          if FLAG_SENDING_CHAIN
     * [32]    receiving_chain        if FLAG_RECEIVING_CHAIN
     * varint  sent_count
     * varint  received_count
     * varint  previous_sending_count
     * varint  skipped_count
     * repeat skipped_count times:
     *   varint  map_key_len
     *   bytes   map_key              "hex(ratchet_key):number", ASCII
     *   32      message_key
     *   24      nonce
     * ```
     *
     * # This returns secrets, and the caller owns that
     *
     * Root key, chain keys, the ratchet seed and every retained message key are all in here in the
     * clear. That is unavoidable -- persisting a ratchet means persisting exactly the material that
     * makes it work -- so the contract is placed on the caller instead: seal these bytes under a key
     * the Android Keystore holds, write them somewhere the OS backup does not reach, and zero the
     * array once it is sealed (brief sections 3002 and 4779). Nothing here may be logged, and
     * [toString] deliberately gives a caller nothing it could log by accident.
     *
     * # Why the skipped keys are kept
     *
     * Dropping them would shrink the file and would also make a message still in flight permanently
     * undecryptable, which a person experiences as a gap in their history with no explanation. They
     * are already held in memory for exactly as long, the file is sealed, and the store rewrites it
     * on every save -- so a key that gets used disappears from the file at the next save, which is
     * what "used keys are deleted" means for a ratchet that must also tolerate reordering.
     *
     * # The map key travels verbatim
     *
     * Not the ratchet key and number re-encoded, but the string [skippedMapKey] produced. Rebuilding
     * it on restore would mean a second implementation of that format, and a lookup only ever finds a
     * stashed key by exact string match -- so the two must agree, and the way to guarantee that is to
     * not have two.
     */
    fun snapshot(): ByteArray {
        val perStashed = 5 + AEAD_KEY_LEN + AEAD_NONCE_LEN + 2 * PUBLIC_KEY_LEN + 11
        val out = ByteAccumulator(
            1 + 5 + associatedData.size + SHARED_SECRET_LEN + 1 + 4 * SEED_LEN + 20 +
                skipped.size * perStashed,
        )
        out.push(STATE_SNAPSHOT_VERSION)
        Varint.encodeU64(associatedData.size.toLong(), out)
        out.append(associatedData)
        out.append(rootKey)

        var flags = 0
        if (sendingPair != null) flags = flags or FLAG_SENDING_PAIR
        if (receivingKey != null) flags = flags or FLAG_RECEIVING_KEY
        if (sendingChain != null) flags = flags or FLAG_SENDING_CHAIN
        if (receivingChain != null) flags = flags or FLAG_RECEIVING_CHAIN
        out.push(flags)

        sendingPair?.let { pair ->
            val seed = pair.exposeSeed()
            out.append(seed)
            // exposeSeed hands back a copy, so this zeroes the copy and not the live pair.
            seed.fill(0)
        }
        receivingKey?.let { out.append(it) }
        sendingChain?.let { out.append(it) }
        receivingChain?.let { out.append(it) }

        Varint.encodeU64(sentCount, out)
        Varint.encodeU64(receivedCount, out)
        Varint.encodeU64(previousSendingCount, out)

        Varint.encodeU64(skipped.size.toLong(), out)
        for ((mapKey, stashed) in skipped) {
            val keyBytes = mapKey.toByteArray(Charsets.US_ASCII)
            Varint.encodeU64(keyBytes.size.toLong(), out)
            out.append(keyBytes)
            out.append(stashed.key)
            out.append(stashed.nonce)
        }
        return out.toByteArray()
    }

    /** Never a key. */
    override fun toString(): String =
        "RatchetSession(sent: $sentCount, received: $receivedCount, skipped: ${skipped.size})"
}

/**
 * Advances a chain key one step and returns the message key it yields.
 *
 * Two keys from one derivation: the next chain key, and the message key plus its nonce. The chain key
 * is overwritten in place, so the previous value is gone — that is the mechanism of forward secrecy,
 * and it is why this takes the buffer and mutates it rather than returning a new one. Callers rely on
 * the mutation: the session holds the same buffer.
 */
private fun advanceRatchetChain(chain: ByteArray): RatchetMessageKey {
    val (nextChain, material) = Kdf.derivePair(chain, null, Kdf.LABEL_RATCHET_CHAIN, 32, 56)
    System.arraycopy(nextChain, 0, chain, 0, nextChain.size)
    nextChain.fill(0)

    val keyBytes = material.copyOfRange(0, AEAD_KEY_LEN)
    val nonce = material.copyOfRange(AEAD_KEY_LEN, material.size)
    material.fill(0)
    val key = SymmetricKey.fromBytes(keyBytes)
    keyBytes.fill(0)
    return RatchetMessageKey(key, nonce)
}

/** The map key for a skipped message: the ratchet key in hex, then its number. */
private fun skippedMapKey(ratchetKey: ByteArray, number: Long): String =
    "${hexOf(ratchetKey)}:$number"
