package com.migo.core.store

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.KeyStore
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

/**
 * The one place this client asks the Android Keystore for a key.
 *
 * Two stores need the same thing: [Vault] wraps the device's private keys, [SessionStore] wraps the
 * ratchet and sender-key state. Both want an AES-256-GCM key that the secure element holds and never
 * hands over, under their own alias, generated once per install. That is the whole of what is shared,
 * and it is shared here rather than written twice because the parts that are easy to get subtly wrong
 * are exactly the parts that would be copied: the StrongBox retry, the two unrelated exception types
 * a missing provider throws, and the fact that a key must never be regenerated under an alias that
 * already has one.
 *
 * What is deliberately *not* shared is the alias. A single alias for both stores would mean signing
 * out (which destroys the vault's key, because overwriting a file on flash does not reliably destroy
 * the old blocks) also made every stored session unreadable, and clearing local message state would
 * take the device's identity with it. Two aliases make those two operations independent, which is
 * what they are.
 *
 * # No `setUserAuthenticationRequired`
 *
 * It would tie decryption to a recent screen unlock, which sounds strictly better and is not: the
 * client has to open inbound messages while the screen is off to show a notification, so a key that
 * will not work until the user looks at the phone is a client that cannot tell them a message
 * arrived. The screen lock still protects this data at rest through file-based encryption; this key
 * protects it against an attacker holding the flash contents without the secure element.
 */
internal object KeystoreWrap {
    private const val PROVIDER = "AndroidKeyStore"
    private const val KEY_BITS = 256

    /**
     * The wrapping key for [alias], generating it on first use.
     *
     * Null rather than an exception when the platform will not serve one, because each caller has its
     * own error type and its own idea of how bad that is: a vault that cannot be opened is a device
     * that cannot sign in, while sessions that cannot be opened are a conversation that re-keys.
     * Returning null lets each of them say so in its own words.
     */
    fun secretKey(alias: String): SecretKey? {
        val store = try {
            KeyStore.getInstance(PROVIDER).apply { load(null) }
        } catch (_: GeneralSecurityException) {
            return null
        } catch (_: IOException) {
            return null
        }

        // Fetched before generated, always. Generating under an alias that already holds a key
        // replaces it, and the replaced key is the only thing that could have opened what is already
        // on disk -- so the bug would present as every stored file turning unreadable at once.
        (store.getKey(alias, null) as? SecretKey)?.let { return it }

        // StrongBox is requested when the device claims to have it and the request is retried without
        // it otherwise, because `setIsStrongBoxBacked` throws at generation time on hardware that
        // cannot honour it rather than quietly falling back.
        val strongBoxPossible = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P
        return generate(alias, strongBox = strongBoxPossible) ?: generate(alias, strongBox = false)
    }

    /**
     * Makes the key under [alias] unrecoverable.
     *
     * The honest way to destroy data on flash storage is to destroy the key that opens it: a file
     * overwrite leaves the old blocks readable to anyone who can address them directly, while a key
     * the secure element has forgotten is gone everywhere. What is left on the flash is then
     * ciphertext under a key that no longer exists.
     *
     * Silent on failure. An alias that is already absent, or a store that will not talk to us, leaves
     * nothing further for a caller to do -- and every caller deletes the file regardless.
     */
    fun forget(alias: String) {
        try {
            KeyStore.getInstance(PROVIDER).apply { load(null) }.deleteEntry(alias)
        } catch (_: GeneralSecurityException) {
            // Nothing to do, and nothing to report.
        } catch (_: IOException) {
            // Same.
        }
    }

    /** One generation attempt. Null means this device would not accept these parameters. */
    private fun generate(alias: String, strongBox: Boolean): SecretKey? = try {
        val spec = KeyGenParameterSpec.Builder(
            alias,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(KEY_BITS)
            .apply {
                // Guarded twice over: the caller only asks for StrongBox on API 28 or later, and the
                // builder method itself does not exist before then.
                if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                    setIsStrongBoxBacked(true)
                }
            }
            .build()
        KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
            .apply { init(spec) }
            .generateKey()
    } catch (_: GeneralSecurityException) {
        null
    } catch (_: IllegalStateException) {
        null
    }
}
