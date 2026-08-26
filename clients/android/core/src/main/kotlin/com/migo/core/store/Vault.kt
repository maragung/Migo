package com.migo.core.store

import android.content.Context
import com.migo.core.crypto.AEAD_KEY_LEN
import com.migo.core.crypto.AEAD_NONCE_LEN
import com.migo.core.crypto.AEAD_TAG_LEN
import com.migo.core.crypto.Aead
import com.migo.core.crypto.IdentitySecret
import com.migo.core.crypto.KeyPair
import com.migo.core.crypto.SEED_LEN
import com.migo.core.crypto.SymmetricKey
import com.migo.core.wire.Id
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.ProviderException
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * This device's private keys, sealed on disk under a key the hardware holds.
 *
 * # Why the Keystore and not a passphrase
 *
 * The desktop client derives its vault key from a passphrase because a Linux desktop has no key store
 * that is present everywhere. Android does: [KeyStore] with the `AndroidKeyStore` provider, where a
 * key can be generated inside the secure element and is never readable by this process at all -- the
 * app sends bytes to it and gets bytes back. That is strictly better than anything derived from
 * something a person types on a phone keyboard, and it is what brief section 178 means by "Keystore
 * rather than local storage".
 *
 * So the passphrase is gone and with it the Argon2 cost, the stored parameters, and the unlock screen.
 * What replaces them is one wrapping key, `migo.vault.v1`, generated once per install.
 *
 * # Two layers, and why
 *
 * ```text
 * "MIGOVLT2"  8 bytes magic
 * u8          format version
 * u8          wrapped key length
 * u8          GCM nonce length
 * 12 bytes    GCM nonce for the wrapping layer
 * n bytes     the vault key, wrapped: AES-256-GCM under the Keystore key
 * remainder   XChaCha20-Poly1305 sealed body: nonce || ciphertext || tag
 * ```
 *
 * The inner body is sealed with the same libsodium AEAD the rest of this client uses, under a random
 * 32-byte vault key; only that key is wrapped by the Keystore. The layering is deliberate rather than
 * ceremonial:
 *
 * - A Keystore operation is an IPC round trip to another process. Wrapping 32 bytes costs one; sealing
 *   a body that grows with every stored prekey would cost one per save, on the main thread's critical
 *   path at startup, for a body that can be several kilobytes.
 * - The body's crypto then stays the audited path that produced every other sealed byte in this
 *   client, which means a vault written here and a vault written by the desktop differ only in how the
 *   key was obtained.
 * - `setRandomizedEncryptionRequired` is left at its default, so the Keystore chooses the GCM nonce
 *   and this code stores what it chose. Choosing it here would be the one way to catastrophically
 *   misuse GCM, and the platform will refuse a supplied nonce anyway.
 *
 * The whole header -- magic, version, both lengths and the nonce -- is the inner AEAD's associated
 * data. An attacker who swaps the wrapped key for one they control gets a tag failure rather than a
 * body decrypted under a key of their choosing.
 *
 * # What is stored, and what is not
 *
 * The identity seeds, the signed prekey, every unused one-time prekey, and optionally the saved
 * sign-in. The access token is deliberately absent: it lives for minutes, so persisting it would buy
 * nothing and widen the window in which a stolen device image is directly usable. The refresh token is
 * exchanged for a fresh pair at startup and the server rotates it, so a replay of an old one is
 * detected there as refresh reuse.
 *
 * Nothing in here is ever logged, in any form, and there is no accessor that returns the file's bytes.
 */

/** A vault failure. */
sealed class VaultError(message: String) : Exception(message) {
    /** No vault file yet: this install has never been signed in. */
    object Absent : VaultError("no vault on this device")

    /**
     * The file exists but did not open.
     *
     * One reason for every cause -- a truncated file, a wrong magic, a failed tag, a wrapping key the
     * Keystore no longer has. They are indistinguishable to the caller on purpose: the recovery is the
     * same in every case (sign in again), and a message naming which check failed tells anyone
     * inspecting the device more than it tells the user.
     */
    object Unreadable : VaultError("the vault on this device could not be opened")

    /** The Keystore refused. Usually a device with no secure hardware, or a locked-out key. */
    object KeystoreUnavailable : VaultError("this device's key store is not available")

    /** The vault could not be written. */
    object NotWritten : VaultError("the vault could not be saved")
}

/**
 * A saved sign-in, so launching the client does not mean a full login.
 *
 * Mirrors the desktop's `SavedSession` field for field, minus the passphrase concept -- the fields are
 * what a client needs to get from a cold start back to a live session without asking the user
 * anything.
 */
data class SavedSession(
    /** Base URL of the server this account belongs to. */
    val serverUrl: String,
    /** The account. */
    val accountId: Id,
    /** This device, as registered with the server. Stable across sign-ins. */
    val deviceId: Id,
    /** The username, so a launch screen can greet the right person before any network call. */
    val username: String,
    /** The refresh token, exchanged for an access token at startup. */
    val refreshToken: String,
) {
    /** The refresh token is bearer material and never appears here. */
    override fun toString(): String =
        "SavedSession(server_url: $serverUrl, account_id: $accountId, " +
            "device_id: $deviceId, username: $username, refresh_token: ***)"
}

/**
 * Everything this device holds privately.
 *
 * Not a `data class`: the derived `toString` would print seeds, and the derived `equals` over the map
 * would invite comparing two sets of private keys, which is not an operation with a use.
 */
class DeviceKeys(
    /** The long-term identity: one Ed25519 signing seed and one X25519 exchange seed. */
    val identity: IdentitySecret,
    /** Which signed prekey the published bundle names. */
    val signedPrekeyId: Long,
    /** The signed prekey's private half. */
    val signedPrekey: KeyPair,
    /** Unused one-time prekeys, by key id. An entry is removed when a peer consumes it. */
    val oneTime: MutableMap<Long, KeyPair>,
    /**
     * The id the next signed prekey rotation will use.
     *
     * Persisted rather than derived from [signedPrekeyId] because a key id must never be reused: a
     * message already in flight names the prekey it was sealed against, and if that id now resolves
     * to a different private key the X3DH derives a different secret and the message is lost with no
     * indication of why. `signedPrekeyId + 1` happens to be right today and stops being right the
     * moment a build retires more than one prekey between saves.
     */
    val nextSignedPrekeyId: Long,
    /**
     * The id the next minted one-time prekey will use.
     *
     * Same reason, and here the derivation is not merely fragile but wrong: `max(oneTime.keys) + 1`
     * reuses an id whenever the highest-numbered prekey is the one a peer consumed, which is exactly
     * the case where a first message using it may still be in flight.
     */
    val nextOneTimePrekeyId: Long,
    /** The saved sign-in, when there is one. */
    val session: SavedSession?,
) {
    /** Public shape only; no seed and no token. */
    override fun toString(): String =
        "DeviceKeys(identity: ${identity.public()}, signed_prekey_id: $signedPrekeyId, " +
            "one_time_count: ${oneTime.size}, session: ${session ?: "none"})"

    /**
     * Drops the one-time prekeys.
     *
     * Not a full wipe, and deliberately not called one. [IdentitySecret] and [KeyPair] hold their
     * seeds privately and hand out copies, so there is no in-place zeroing this class can perform on
     * them -- and adding one would be a promise the JVM cannot keep anyway: the garbage collector
     * relocates and copies objects, so a seed that has lived in a heap object has already been
     * duplicated in memory the moment it was moved. What is actually achievable is what this class
     * does elsewhere: keep the seeds out of logs, out of `toString`, and off the disk in plaintext.
     */
    fun forgetOneTimePrekeys() {
        oneTime.clear()
    }
}

/**
 * The vault, bound to one file and one Keystore alias.
 *
 * Construct it with [open]. Every method is blocking: the Keystore call and the file write both are,
 * and wrapping them in a coroutine here would only hide from the caller that this belongs off the main
 * thread. The caller dispatches.
 */
class Vault private constructor(private val file: File, private val wrappingKey: SecretKey) {

    /** True when this device has a vault to load. */
    fun exists(): Boolean = file.exists()

    /**
     * Loads and decrypts the vault.
     *
     * @throws VaultError.Absent when there is no vault
     * @throws VaultError.Unreadable for every other failure
     */
    fun load(): DeviceKeys {
        if (!file.exists()) throw VaultError.Absent
        val raw = try {
            file.readBytes()
        } catch (_: IOException) {
            throw VaultError.Unreadable
        }
        if (raw.size < MIN_FILE_LEN) throw VaultError.Unreadable
        if (!raw.copyOfRange(0, MAGIC.size).contentEquals(MAGIC)) throw VaultError.Unreadable
        if ((raw[MAGIC.size].toInt() and 0xFF) != FORMAT_VERSION) throw VaultError.Unreadable

        val wrappedLen = raw[MAGIC.size + 1].toInt() and 0xFF
        val nonceLen = raw[MAGIC.size + 2].toInt() and 0xFF
        if (nonceLen != GCM_NONCE_LEN || wrappedLen == 0) throw VaultError.Unreadable
        val headerLen = MAGIC.size + 3 + nonceLen + wrappedLen
        if (raw.size <= headerLen) throw VaultError.Unreadable

        val header = raw.copyOfRange(0, headerLen)
        val nonce = raw.copyOfRange(MAGIC.size + 3, MAGIC.size + 3 + nonceLen)
        val wrapped = raw.copyOfRange(MAGIC.size + 3 + nonceLen, headerLen)
        val body = raw.copyOfRange(headerLen, raw.size)

        val keyBytes = try {
            val cipher = Cipher.getInstance(WRAP_TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, wrappingKey, GCMParameterSpec(GCM_TAG_BITS, nonce))
            cipher.doFinal(wrapped)
        } catch (_: GeneralSecurityException) {
            throw VaultError.Unreadable
        } catch (_: ProviderException) {
            throw VaultError.Unreadable
        }
        if (keyBytes.size != AEAD_KEY_LEN) {
            keyBytes.fill(0)
            throw VaultError.Unreadable
        }

        val vaultKey = SymmetricKey.fromBytes(keyBytes)
        keyBytes.fill(0)
        return try {
            decodeBody(Aead.open(vaultKey, header, body))
        } catch (_: Exception) {
            throw VaultError.Unreadable
        } finally {
            vaultKey.destroy()
        }
    }

    /**
     * Encrypts and writes the vault.
     *
     * A fresh vault key and a fresh Keystore nonce on every save. Reusing either would mean two
     * versions of the file encrypted under the same key and nonce, which for GCM is the failure that
     * leaks the key stream -- and there is no reason to reuse them when generating both costs
     * microseconds.
     *
     * Written to a temporary file and renamed. A vault half-overwritten by a process killed mid-write
     * is a device that has lost its identity key, and the messages sealed to it with it.
     */
    fun save(keys: DeviceKeys) {
        val vaultKey = SymmetricKey.generate()
        try {
            val cipher = try {
                Cipher.getInstance(WRAP_TRANSFORMATION).apply {
                    init(Cipher.ENCRYPT_MODE, wrappingKey)
                }
            } catch (_: GeneralSecurityException) {
                throw VaultError.KeystoreUnavailable
            } catch (_: ProviderException) {
                throw VaultError.KeystoreUnavailable
            }
            // The Keystore picked this nonce; storing it is the whole reason the header has room for
            // one. See the class note on why it is not chosen here.
            val nonce = cipher.iv
            // `expose()` returns the key's live array rather than a copy, so this must not be
            // zeroed here: the seal below still needs it, and `destroy()` in the `finally` at the
            // bottom is what wipes it -- once, after the last use.
            val wrapped = try {
                cipher.doFinal(vaultKey.expose())
            } catch (_: GeneralSecurityException) {
                throw VaultError.KeystoreUnavailable
            } catch (_: ProviderException) {
                // The AndroidKeyStore provider reports a key the secure element has invalidated or
                // locked out as an unchecked ProviderException, not a GeneralSecurityException.
                throw VaultError.KeystoreUnavailable
            }

            val header = ByteArray(MAGIC.size + 3 + nonce.size + wrapped.size)
            MAGIC.copyInto(header)
            header[MAGIC.size] = FORMAT_VERSION.toByte()
            header[MAGIC.size + 1] = wrapped.size.toByte()
            header[MAGIC.size + 2] = nonce.size.toByte()
            nonce.copyInto(header, MAGIC.size + 3)
            wrapped.copyInto(header, MAGIC.size + 3 + nonce.size)

            val body = Aead.seal(vaultKey, header, encodeBody(keys))
            val temporary = File(file.parentFile, file.name + ".new")
            try {
                temporary.outputStream().use { out ->
                    out.write(header)
                    out.write(body)
                    out.fd.sync()
                }
                if (!temporary.renameTo(file)) throw VaultError.NotWritten
            } catch (_: IOException) {
                temporary.delete()
                throw VaultError.NotWritten
            }
        } finally {
            vaultKey.destroy()
        }
    }

    /**
     * Deletes the vault and the wrapping key.
     *
     * Both, and the key especially: a file overwrite on flash storage does not reliably destroy the old
     * blocks, so the honest way to make a vault unrecoverable is to destroy the key that opens it, which
     * for a Keystore key means the secure element forgets it. What is left on the flash is then
     * ciphertext under a key that no longer exists anywhere.
     */
    fun destroy() {
        file.delete()
        File(file.parentFile, file.name + ".new").delete()
        KeystoreWrap.forget(KEY_ALIAS)
    }

    /**
     * Serialises the vault body with the MSE writer.
     *
     * The same codec as the wire, not because the body is ever sent -- it is not -- but because it is
     * the one encoder in this client that is length-checked, depth-bounded and covered by conformance
     * vectors. A second hand-rolled format for local storage would be a second parser to get right.
     *
     * The saved session is an optional field, so a vault written by a build that stores more is still
     * readable by one that stores less.
     */
    private fun encodeBody(keys: DeviceKeys): ByteArray {
        val w = Writer()
        w.enter()
        w.bytes(keys.identity.exposeSigningSeed())
        w.bytes(keys.identity.exposeExchangeSeed())
        w.u32(keys.signedPrekeyId)
        w.bytes(keys.signedPrekey.exposeSeed())
        w.u32(keys.oneTime.size)
        for ((keyId, pair) in keys.oneTime) {
            w.u32(keyId)
            w.bytes(pair.exposeSeed())
        }
        val session = keys.session
        w.u32(if (session == null) 1 else 2)
        w.optional(FIELD_KEY_COUNTERS) { sub ->
            sub.u32(keys.nextSignedPrekeyId)
            sub.u32(keys.nextOneTimePrekeyId)
        }
        if (session != null) {
            w.optional(FIELD_SESSION) { sub ->
                sub.str(session.serverUrl)
                sub.id(session.accountId)
                sub.id(session.deviceId)
                sub.str(session.username)
                sub.str(session.refreshToken)
            }
        }
        w.leave()
        return w.finish()
    }

    /** Parses what [encodeBody] wrote. Any inconsistency is a failure, never a partial result. */
    private fun decodeBody(bytes: ByteArray): DeviceKeys {
        val r = Reader(bytes)
        r.enter()
        val signingSeed = r.bytes()
        val exchangeSeed = r.bytes()
        if (signingSeed.size != SEED_LEN || exchangeSeed.size != SEED_LEN) {
            throw VaultError.Unreadable
        }
        val identity = IdentitySecret.fromSeeds(signingSeed, exchangeSeed)
        signingSeed.fill(0)
        exchangeSeed.fill(0)

        val signedPrekeyId = r.u32()
        val signedSeed = r.bytes()
        if (signedSeed.size != SEED_LEN) throw VaultError.Unreadable
        val signedPrekey = KeyPair.fromSeed(signedSeed)
        signedSeed.fill(0)

        val count = r.u32()
        // A count is a length the file claims, and this file may have been tampered with even though
        // the tag says otherwise for the bytes that follow. Bounding it here means a corrupt count
        // cannot ask for a map of two billion entries before the read that would have failed.
        if (count < 0 || count > MAX_ONE_TIME_PREKEYS) throw VaultError.Unreadable
        val oneTime = HashMap<Long, KeyPair>(count.toInt())
        for (i in 0L until count) {
            val keyId = r.u32()
            val seed = r.bytes()
            if (seed.size != SEED_LEN) throw VaultError.Unreadable
            oneTime[keyId] = KeyPair.fromSeed(seed)
            seed.fill(0)
        }

        var session: SavedSession? = null
        var nextSignedPrekeyId: Long? = null
        var nextOneTimePrekeyId: Long? = null
        val optionalCount = r.u32()
        for (i in 0L until optionalCount) {
            val (fieldId, sub) = r.optional()
            when (fieldId) {
                FIELD_SESSION.toLong() ->
                    session = SavedSession(sub.str(), sub.id(), sub.id(), sub.str(), sub.str())
                FIELD_KEY_COUNTERS.toLong() -> {
                    nextSignedPrekeyId = sub.u32()
                    nextOneTimePrekeyId = sub.u32()
                }
                // Any other field was written by a newer build and is skipped by its length.
                else -> {}
            }
        }
        r.leave()
        // A vault written before the counters existed gets the only defaults available, and they are
        // the unsafe derivations the fields exist to replace. That is the honest trade: a vault from
        // an older build has no better answer, and refusing to open it would strand a device whose
        // identity is still perfectly good. Every save from here on writes the real values.
        return DeviceKeys(
            identity,
            signedPrekeyId,
            signedPrekey,
            oneTime,
            nextSignedPrekeyId ?: (signedPrekeyId + 1L),
            nextOneTimePrekeyId ?: ((oneTime.keys.maxOrNull() ?: 0L) + 1L),
            session,
        )
    }

    companion object {
        /** `"MIGOVLT2"`: version 2 of the vault format, the Keystore-wrapped one. */
        private val MAGIC = byteArrayOf(0x4D, 0x49, 0x47, 0x4F, 0x56, 0x4C, 0x54, 0x32)
        private const val FORMAT_VERSION = 2
        private const val FILE_NAME = "device.vault"

        /** The Keystore alias. Versioned, so a future format can migrate rather than overwrite. */
        private const val KEY_ALIAS = "migo.vault.v1"
        private const val WRAP_TRANSFORMATION = "AES/GCM/NoPadding"
        private const val GCM_NONCE_LEN = 12
        private const val GCM_TAG_LEN = 16
        private const val GCM_TAG_BITS = GCM_TAG_LEN * 8

        /** The optional-field id under which the saved sign-in lives. */
        private const val FIELD_SESSION = 1
        private const val FIELD_KEY_COUNTERS = 2

        /**
         * A ceiling on the one-time prekeys a vault may claim to hold.
         *
         * Generously above the hundred a client publishes, because a device that topped up before its
         * old batch was consumed legitimately holds more than one batch for a while.
         */
        private const val MAX_ONE_TIME_PREKEYS = 4096L

        /**
         * The shortest byte count a well-formed vault can have.
         *
         * Magic, version, the two length bytes, the GCM nonce, the wrapped key with its GCM tag,
         * and a body consisting of nothing but an XChaCha20-Poly1305 nonce and tag. Checked before
         * any field is read so a truncated file fails on its length rather than on a slice index.
         */
        private const val MIN_FILE_LEN =
            8 + 3 + GCM_NONCE_LEN + (AEAD_KEY_LEN + GCM_TAG_LEN) + (AEAD_NONCE_LEN + AEAD_TAG_LEN)

        /**
         * Opens the vault for this app, creating the wrapping key on first use.
         *
         * The file lives in [Context.getNoBackupFilesDir]. Not `filesDir`: that one is included in
         * Android's automatic backup, and a vault in a cloud backup is a device's private keys in a
         * cloud backup -- which would also be useless there, since the Keystore key that opens it
         * never leaves the device it was generated on. A restored file that cannot be decrypted is
         * worse than an absent one, because it looks like a working sign-in until it does not.
         *
         * @throws VaultError.KeystoreUnavailable when the platform key store will not serve a key
         */
        fun open(context: Context): Vault {
            val file = File(context.noBackupFilesDir, FILE_NAME)
            return Vault(file, wrappingKey())
        }

        /**
         * Fetches the wrapping key, generating it once per install.
         *
         * The parameters, the StrongBox retry and the never-regenerate-over-an-existing-alias rule all
         * live in [KeystoreWrap], because [SessionStore] wants a key on exactly the same terms and the
         * pieces that are easy to get subtly wrong are exactly the pieces that would be copied. What
         * stays here is the one thing that differs: the alias, and what it means when there is no key.
         *
         * @throws VaultError.KeystoreUnavailable when the platform key store will not serve a key
         */
        private fun wrappingKey(): SecretKey =
            KeystoreWrap.secretKey(KEY_ALIAS) ?: throw VaultError.KeystoreUnavailable
    }
}
