package com.migo.core.store

import android.content.Context
import com.migo.core.crypto.AEAD_KEY_LEN
import com.migo.core.crypto.AEAD_NONCE_LEN
import com.migo.core.crypto.AEAD_TAG_LEN
import com.migo.core.crypto.Aead
import com.migo.core.crypto.RatchetSession
import com.migo.core.crypto.ReceiverKeyState
import com.migo.core.crypto.SenderKeyState
import com.migo.core.crypto.SymmetricKey
import com.migo.core.crypto.hexOf
import com.migo.core.session.GroupPersistence
import com.migo.core.session.SessionPersistence
import com.migo.core.session.StoredSending
import com.migo.core.wire.Id
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import com.migo.core.wire.idToBytes
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.ProviderException
import java.util.concurrent.locks.ReentrantLock
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlin.concurrent.withLock

/**
 * Where the ratchets live between launches.
 *
 * [SessionCrypto][com.migo.core.session.SessionCrypto] and
 * [GroupCrypto][com.migo.core.session.GroupCrypto] hold their state in memory and hand it to a
 * [SessionPersistence] / [GroupPersistence] to survive a process death. This is that implementation
 * for Android, and it is one class for both interfaces because the two hold halves of the same thing:
 * a conversation's pairwise ratchets and its sender-key chains are created together, used together,
 * and must be forgotten together. Two objects would mean two directories, two Keystore aliases, and a
 * `deleteConversation` that could leave one half behind -- which is not a tidiness problem: a stale
 * sender-key chain whose pairwise sessions are gone is a chain the client would keep encrypting under
 * and nobody could open.
 *
 * The two interfaces declare an identical `deleteConversation`, so the single override below satisfies
 * both, and it wipes pairwise, sending and receiving state in one pass. That is the correct meaning of
 * the call, not a coincidence this class exploits.
 *
 * # One file per entry
 *
 * A conversation has one sending chain, one ratchet per peer device, and one receiving chain per peer
 * device -- so a busy account holds hundreds of small blobs, each read and written independently. One
 * file per entry means a save rewrites one entry, and a torn write damages one session rather than
 * every session. A single bundled file would turn each `save` into a read-modify-write of the whole
 * set: on the send path, once per message.
 *
 * SQLite would give indexing this has no use for. There are no queries here, only `get` and `put` by
 * a key the caller already holds, and adding a database would mean either storing sealed blobs in it
 * (the filesystem with extra steps) or storing key material in columns, where a stray `SELECT *` in a
 * debug tool prints a chain key.
 *
 * # Why the file name is authenticated
 *
 * Each entry is sealed under its own fresh key, wrapped by [KeystoreWrap], and the associated data is
 * the header **plus the entry's own name**. Without the name, an attacker with filesystem access could
 * rename device A's ratchet file over device B's: every tag still verifies, both blobs are genuine,
 * and the client would decrypt B's traffic against A's chain -- failing in a way that looks like a
 * crypto bug rather than tampering. Binding the name makes the file's location part of what the tag
 * covers, so a moved file is simply unreadable.
 *
 * # Load returns null, save throws
 *
 * Deliberately asymmetric. A session that cannot be read is recoverable: the client starts a fresh
 * one, the peer sees a new safety number, and messaging continues -- exactly what happens after a
 * reinstall. So a damaged entry is deleted and reported as absent.
 *
 * A save that cannot be written is not recoverable, and this is the sharp edge of the whole class. A
 * sending chain that silently failed to save restores one or more steps behind, re-derives a message
 * key it has already used, and seals two different messages under the same key and nonce -- the one
 * failure XChaCha20-Poly1305 offers no protection against. The caller must find out, so `save` throws.
 *
 * # What cannot be persisted through this interface
 *
 * [SessionPersistence.save] receives a [RatchetSession] and nothing else, so the pending X3DH
 * preamble that `SessionCrypto` keeps beside a freshly-initiated session is not saved. A client that
 * restarts after initiating a session the peer never answered will send its next message as an
 * established ratchet message with no preamble, and the peer -- which never derived the session --
 * cannot open it. Recovery is the ordinary one: the peer's decrypt fails, the session is re-initiated.
 * Fixing it properly means widening the interface, which is a change to the shared session layer and
 * not something this store can do on its own.
 *
 * # Threading
 *
 * Every file operation is under one lock. The interfaces are non-suspending and are called from the
 * send and receive paths, which run on different coroutines against the same conversation, and two
 * concurrent saves of one entry could otherwise interleave their temp-file rename. The work under the
 * lock is a few kilobytes of AES and one `rename`; a finer-grained scheme would be per-entry locks
 * for no measurable gain.
 */
class SessionStore private constructor(
    private val directory: File,
    private val wrappingKey: SecretKey,
) : SessionPersistence, GroupPersistence {
    private val lock = ReentrantLock()

    // -- SessionPersistence: the pairwise Double Ratchet ------------------------------------------

    override fun load(conversationId: Id, deviceId: Id): RatchetSession? {
        val name = nameFor(PREFIX_PAIRWISE, conversationId, deviceId)
        return lock.withLock { rebuild(name) { RatchetSession.restore(it) } }
    }

    override fun save(conversationId: Id, deviceId: Id, session: RatchetSession) {
        val name = nameFor(PREFIX_PAIRWISE, conversationId, deviceId)
        lock.withLock { writeEntry(name, session.snapshot()) }
    }

    override fun delete(conversationId: Id, deviceId: Id) {
        val name = nameFor(PREFIX_PAIRWISE, conversationId, deviceId)
        lock.withLock { deleteEntry(name) }
    }

    /**
     * Declared by both [SessionPersistence] and [GroupPersistence]; one override serves both.
     *
     * Deletes by name prefix rather than by enumerating peers, because the store does not know which
     * devices a conversation had -- and after a corrupt entry was dropped, neither does anything else.
     * The prefix sweep also removes any `.new` temp file a killed process left behind.
     */
    override fun deleteConversation(conversationId: Id) {
        val conversation = hexOf(bytesOf(conversationId))
        lock.withLock {
            deletePrefix(PREFIX_PAIRWISE + conversation)
            deletePrefix(PREFIX_SENDING + conversation)
            deletePrefix(PREFIX_RECEIVER + conversation)
        }
    }

    // -- GroupPersistence: sender-key chains ------------------------------------------------------

    override fun loadSending(conversationId: Id): StoredSending? {
        val name = nameFor(PREFIX_SENDING, conversationId)
        return lock.withLock { rebuild(name) { decodeSending(it) } }
    }

    override fun saveSending(
        conversationId: Id,
        state: SenderKeyState,
        epoch: Long,
        distributed: Set<Id>,
    ) {
        val name = nameFor(PREFIX_SENDING, conversationId)
        lock.withLock { writeEntry(name, encodeSending(state, epoch, distributed)) }
    }

    override fun loadReceiver(conversationId: Id, senderDeviceId: Id): ReceiverKeyState? {
        val name = nameFor(PREFIX_RECEIVER, conversationId, senderDeviceId)
        return lock.withLock { rebuild(name) { ReceiverKeyState.restore(it) } }
    }

    override fun saveReceiver(conversationId: Id, senderDeviceId: Id, state: ReceiverKeyState) {
        val name = nameFor(PREFIX_RECEIVER, conversationId, senderDeviceId)
        lock.withLock { writeEntry(name, state.snapshot()) }
    }

    override fun deleteReceiver(conversationId: Id, senderDeviceId: Id) {
        val name = nameFor(PREFIX_RECEIVER, conversationId, senderDeviceId)
        lock.withLock { deleteEntry(name) }
    }

    // -- Lifecycle --------------------------------------------------------------------------------

    /**
     * Forgets every stored session.
     *
     * The wrapping key goes with them, for the reason given in [KeystoreWrap.forget]: deleting files
     * on flash does not reliably destroy the blocks, and a key the secure element has forgotten makes
     * every remaining byte meaningless. The next [open] generates a fresh key, which is why this is
     * safe to call at sign-out -- the entries it could no longer read are gone in the same breath.
     *
     * This does not touch [Vault]. Signing out destroys both, but clearing local message state should
     * not cost the device its identity, which is why the two hold separate aliases.
     *
     * The instance is spent once this returns: its wrapping key no longer exists, so every later save
     * would throw. Discard it and [open] a fresh one if the process carries on.
     */
    fun destroy() {
        lock.withLock { wipe(directory) }
    }

    // -- Entry names ------------------------------------------------------------------------------

    /**
     * The file name for an entry: a prefix and one 32-character hex block per id.
     *
     * Re-encoded rather than filtered. [Id] has a public constructor, so an id can hold arbitrary
     * text -- including a `/` or a `..`, which as a file name would be a path traversal. Going through
     * [idToBytes] both validates the id and produces 16 bytes, and [hexOf] maps those to a fixed
     * 32 characters that cannot be anything but `0-9a-f`. There is no filter to get wrong, because
     * the untrusted string never reaches the filesystem.
     *
     * Fixed-width blocks are also what makes the prefix sweep in [deleteConversation] exact: a
     * conversation's hex block can never be a prefix of a different conversation's.
     *
     * Two overloads rather than one `vararg`, because [Id] is `@JvmInline` and Kotlin has no
     * representation for an array of an inline class, so `vararg ids: Id` does not compile. Only
     * the two arities below are ever needed: a sending entry is keyed by the conversation alone,
     * and the other two kinds by the conversation and one device.
     */
    private fun nameFor(prefix: String, id: Id): String = prefix + hexOf(bytesOf(id))

    private fun nameFor(prefix: String, first: Id, second: Id): String =
        prefix + hexOf(bytesOf(first)) + hexOf(bytesOf(second))

    /** [idToBytes], with its `IllegalArgumentException` translated into this store's vocabulary. */
    private fun bytesOf(id: Id): ByteArray = try {
        idToBytes(id)
    } catch (_: IllegalArgumentException) {
        throw SessionStoreError.BadName
    }

    // -- The sending entry's body ------------------------------------------------------------------

    /**
     * Encodes a sending chain with the MSE writer.
     *
     * The other two entry kinds store a snapshot and nothing else, so their body *is* the snapshot.
     * A sending chain also carries the epoch and the set of devices that already have the current
     * distribution, so it needs a container, and this uses the same codec as the wire for the reason
     * [Vault] gives: it is the one encoder here that is length-checked, depth-bounded, and covered by
     * conformance vectors. A second hand-rolled format would be a second parser to keep correct.
     *
     * The trailing `u32(0)` is the optional-field count. Nothing is optional yet; the field exists so
     * a later build can add one without a format break.
     */
    private fun encodeSending(state: SenderKeyState, epoch: Long, distributed: Set<Id>): ByteArray {
        val snapshot = state.snapshot()
        val w = Writer()
        w.enter()
        w.u64(epoch)
        w.listLen(distributed.size)
        for (deviceId in distributed) {
            w.id(deviceId)
        }
        w.bytes(snapshot)
        w.u32(0)
        w.leave()
        // Zeroes this copy. The writer's own buffer still holds one until it is collected, exactly as
        // in Vault.encodeBody; the array handed to writeEntry is zeroed there.
        snapshot.fill(0)
        return w.finish()
    }

    /** Parses what [encodeSending] wrote. Any inconsistency throws, and the caller drops the file. */
    private fun decodeSending(bytes: ByteArray): StoredSending {
        val r = Reader(bytes)
        r.enter()
        val epoch = r.u64()
        // No hand-rolled cap: Reader.listLen already bounds a claimed count by MAX_LIST_ITEMS and by
        // the bytes actually remaining, so a corrupt length cannot ask for a set of two billion ids.
        val count = r.listLen()
        val distributed = LinkedHashSet<Id>(count)
        repeat(count) { distributed.add(r.id()) }
        val snapshot = r.bytes()
        val state = try {
            SenderKeyState.restore(snapshot)
        } finally {
            snapshot.fill(0)
        }
        val optionalCount = r.u32()
        for (i in 0L until optionalCount) {
            // Written by a newer build; skipped by its own length prefix.
            r.optional()
        }
        r.leave()
        return StoredSending(state, epoch, distributed)
    }

    // -- Files ------------------------------------------------------------------------------------

    /**
     * Reads an entry and rebuilds it, dropping the file if it will not rebuild.
     *
     * Every failure is one answer: absent. A truncated file, a tag that does not verify, a snapshot
     * from a version this build refuses, a wrapping key the secure element has invalidated -- the
     * caller's move is identical in all of them, which is to treat the session as gone and make a new
     * one. Keeping a file that has already failed to load would mean failing the same way at every
     * launch.
     */
    private fun <T> rebuild(name: String, from: (ByteArray) -> T): T? {
        val plaintext = readEntry(name) ?: return null
        return try {
            from(plaintext)
        } catch (_: Exception) {
            deleteEntry(name)
            null
        } finally {
            plaintext.fill(0)
        }
    }

    /** The sealed entry's plaintext, or null when it is absent or unreadable. */
    private fun readEntry(name: String): ByteArray? {
        val file = File(directory, name)
        val raw = try {
            if (!file.isFile) return null
            file.readBytes()
        } catch (_: IOException) {
            return null
        }
        return try {
            unseal(name, raw)
        } catch (_: Exception) {
            file.delete()
            null
        }
    }

    /**
     * Unwraps the entry key with the Keystore, then opens the body.
     *
     * The same two-layer shape as [Vault]: a fresh AEAD key per write, wrapped under the hardware key,
     * with the header and the entry name as the AEAD associated data. The Keystore key encrypts 32
     * bytes rather than the whole body, which keeps the secure element off the hot path while still
     * making the body unreadable without it.
     */
    private fun unseal(name: String, raw: ByteArray): ByteArray {
        if (raw.size < MIN_FILE_LEN) throw SessionStoreError.Unreadable
        if (!raw.copyOfRange(0, MAGIC.size).contentEquals(MAGIC)) throw SessionStoreError.Unreadable
        if ((raw[MAGIC.size].toInt() and 0xff) != FORMAT_VERSION) throw SessionStoreError.Unreadable

        val wrappedLen = raw[MAGIC.size + 1].toInt() and 0xff
        val nonceLen = raw[MAGIC.size + 2].toInt() and 0xff
        if (nonceLen != GCM_NONCE_LEN || wrappedLen < AEAD_KEY_LEN + GCM_TAG_LEN) {
            throw SessionStoreError.Unreadable
        }
        val headerLen = MAGIC.size + 3 + nonceLen + wrappedLen
        if (raw.size <= headerLen) throw SessionStoreError.Unreadable

        val nonce = raw.copyOfRange(MAGIC.size + 3, MAGIC.size + 3 + nonceLen)
        val wrapped = raw.copyOfRange(MAGIC.size + 3 + nonceLen, headerLen)
        val aad = associatedData(raw.copyOfRange(0, headerLen), name)

        val keyBytes = try {
            Cipher.getInstance(WRAP_TRANSFORMATION).run {
                init(Cipher.DECRYPT_MODE, wrappingKey, GCMParameterSpec(GCM_TAG_BITS, nonce))
                doFinal(wrapped)
            }
        } catch (_: GeneralSecurityException) {
            throw SessionStoreError.Unreadable
        } catch (_: ProviderException) {
            // The provider reports a key the secure element has invalidated or locked out as an
            // unchecked ProviderException, not a GeneralSecurityException.
            throw SessionStoreError.Unreadable
        }
        if (keyBytes.size != AEAD_KEY_LEN) {
            keyBytes.fill(0)
            throw SessionStoreError.Unreadable
        }
        val entryKey = SymmetricKey.fromBytes(keyBytes)
        keyBytes.fill(0)
        return try {
            Aead.open(entryKey, aad, raw.copyOfRange(headerLen, raw.size))
        } catch (_: Exception) {
            throw SessionStoreError.Unreadable
        } finally {
            entryKey.destroy()
        }
    }

    /**
     * Seals [plaintext] into the entry, atomically.
     *
     * Takes ownership of [plaintext] and zeroes it before returning, on success or failure. Every
     * caller passes a snapshot it has no further use for, and a helper that reliably wipes is better
     * than a rule each caller has to remember.
     *
     * Written to `<name>.new`, fsynced, then renamed. The fsync is kept even though it costs a
     * millisecond on the send path, because the failure it prevents is the catastrophic one: without
     * it, a power loss can leave the rename durable and the contents not, and a sending chain that
     * restores from a partially-written file is a chain that reuses a message key.
     */
    private fun writeEntry(name: String, plaintext: ByteArray) {
        val entryKey = SymmetricKey.generate()
        try {
            val cipher = try {
                Cipher.getInstance(WRAP_TRANSFORMATION).apply {
                    init(Cipher.ENCRYPT_MODE, wrappingKey)
                }
            } catch (_: GeneralSecurityException) {
                throw SessionStoreError.NotWritten
            } catch (_: ProviderException) {
                throw SessionStoreError.NotWritten
            }
            // The Keystore chooses the GCM nonce and will not accept one from us, which is the
            // behaviour to want: a repeated nonce under a long-lived key is exactly the mistake a
            // caller-supplied one invites.
            val nonce = cipher.iv
            val wrapped = try {
                cipher.doFinal(entryKey.expose())
            } catch (_: GeneralSecurityException) {
                throw SessionStoreError.NotWritten
            } catch (_: ProviderException) {
                throw SessionStoreError.NotWritten
            }

            val header = ByteArray(MAGIC.size + 3 + nonce.size + wrapped.size)
            MAGIC.copyInto(header)
            header[MAGIC.size] = FORMAT_VERSION.toByte()
            header[MAGIC.size + 1] = wrapped.size.toByte()
            header[MAGIC.size + 2] = nonce.size.toByte()
            nonce.copyInto(header, MAGIC.size + 3)
            wrapped.copyInto(header, MAGIC.size + 3 + nonce.size)

            val body = Aead.seal(entryKey, associatedData(header, name), plaintext)
            val temporary = File(directory, "$name.new")
            try {
                temporary.outputStream().use { out ->
                    out.write(header)
                    out.write(body)
                    out.fd.sync()
                }
                if (!temporary.renameTo(File(directory, name))) throw SessionStoreError.NotWritten
            } catch (_: IOException) {
                temporary.delete()
                throw SessionStoreError.NotWritten
            }
        } finally {
            entryKey.destroy()
            plaintext.fill(0)
        }
    }

    /**
     * The AEAD associated data: the header, then a domain string, then the entry name.
     *
     * The domain string keeps the name from being confusable with anything else that might one day be
     * appended here, and the name is US-ASCII by construction -- [nameFor] emits hex and a two-letter
     * prefix, so the encoding cannot vary between builds or locales.
     */
    private fun associatedData(header: ByteArray, name: String): ByteArray =
        header + (NAME_DOMAIN + name).toByteArray(Charsets.US_ASCII)

    /** Removes an entry and any temp file beside it. Silent: an absent entry is already deleted. */
    private fun deleteEntry(name: String) {
        File(directory, name).delete()
        File(directory, "$name.new").delete()
    }

    /** Removes every entry whose name starts with [prefix], `.new` files included. */
    private fun deletePrefix(prefix: String) {
        val entries = directory.listFiles() ?: return
        for (entry in entries) {
            if (entry.name.startsWith(prefix)) entry.delete()
        }
    }

    companion object {
        /** `"MIGOSES1"`. Distinct from the vault's magic so neither file can be read as the other. */
        private val MAGIC = byteArrayOf(0x4D, 0x49, 0x47, 0x4F, 0x53, 0x45, 0x53, 0x31)

        private const val FORMAT_VERSION = 1
        private const val DIRECTORY_NAME = "sessions"

        /** Separate from the vault's alias so sessions can be wiped without losing the identity. */
        private const val KEY_ALIAS = "migo.sessions.v1"

        private const val WRAP_TRANSFORMATION = "AES/GCM/NoPadding"
        private const val GCM_NONCE_LEN = 12
        private const val GCM_TAG_LEN = 16
        private const val GCM_TAG_BITS = GCM_TAG_LEN * 8

        /** Prefixed with the AEAD header bytes so a moved file cannot decrypt in its new place. */
        private const val NAME_DOMAIN = "migo-session-v1:"

        /** One pairwise ratchet, keyed by conversation and peer device. */
        private const val PREFIX_PAIRWISE = "p_"

        /** This device's sending chain for a conversation. One per conversation. */
        private const val PREFIX_SENDING = "g_"

        /** A peer's sending chain as this device receives it, keyed by conversation and device. */
        private const val PREFIX_RECEIVER = "r_"

        /** Magic, three length bytes, the GCM nonce, the wrapped key, and a non-empty sealed body. */
        private const val MIN_FILE_LEN =
            8 + 3 + GCM_NONCE_LEN + (AEAD_KEY_LEN + GCM_TAG_LEN) + (AEAD_NONCE_LEN + AEAD_TAG_LEN)

        /**
         * Opens the store, creating its directory on first use.
         *
         * Under `noBackupFilesDir` for the same reason as the vault: these files are useless anywhere
         * but this device, since the Keystore key that opens them never leaves it, and a restored
         * backup full of undecryptable sessions is worse than an empty one -- it looks like working
         * history right up until nothing opens.
         *
         * The same instance is passed to both `MigoClientOptions.sessionPersistence` and
         * `.groupPersistence`. Two instances would work but would hold two locks over one directory,
         * which is the one arrangement that could interleave two writes to the same entry.
         *
         * @throws SessionStoreError.NotWritten when the directory cannot be created
         * @throws SessionStoreError.KeystoreUnavailable when the platform key store will not serve a
         *   key
         */
        /**
         * Forgets every stored session without opening the store.
         *
         * For the two moments a caller needs the slate clean *before* it has an instance: signing a
         * different account in on a device that still holds the previous one's ratchets, and recovering
         * from a store that would not open. Doing it through an instance would not work anyway -- the
         * wrapping key is deleted, so the instance that just wiped can no longer write, and the next
         * save would fail rather than start fresh. Wipe, then [open]: the open mints a new key.
         */
        fun wipe(context: Context) {
            wipe(File(context.noBackupFilesDir, DIRECTORY_NAME))
        }

        private fun wipe(directory: File) {
            directory.listFiles()?.forEach { it.delete() }
            KeystoreWrap.forget(KEY_ALIAS)
        }

        fun open(context: Context): SessionStore {
            val directory = File(context.noBackupFilesDir, DIRECTORY_NAME)
            if (!directory.isDirectory && !directory.mkdirs()) throw SessionStoreError.NotWritten
            val key = KeystoreWrap.secretKey(KEY_ALIAS) ?: throw SessionStoreError.KeystoreUnavailable
            return SessionStore(directory, key)
        }
    }
}

/**
 * What can go wrong with the session store.
 *
 * Four cases, and no case carries a file name, an id, or a byte of what it was reading. These reach
 * logs, and brief section 174 forbids key material and sealed envelopes from appearing there; a
 * message naming the conversation whose ratchet failed would also be metadata this client has no
 * reason to write down.
 *
 * Objects rather than classes because none of them has anything to add. The distinction that matters
 * to a caller is between the two [Unreadable] and [NotWritten] represent -- one is recoverable by
 * starting a new session, the other means the next launch may reuse a message key -- and both of those
 * are the type, not a field on it.
 */
sealed class SessionStoreError(message: String) : Exception(message) {
    /** The entry exists and will not open. Reported to callers as absent; the file is deleted. */
    object Unreadable : SessionStoreError("the stored session could not be read")

    /** The entry was not saved. The caller must treat its in-memory state as unpersisted. */
    object NotWritten : SessionStoreError("the session could not be saved")

    /** An [Id] that is not a valid id reached the store, so no file name could be formed. */
    object BadName : SessionStoreError("that identifier is not a valid id")

    /** The platform key store would not serve the wrapping key. */
    object KeystoreUnavailable : SessionStoreError("the device key store is unavailable")
}
