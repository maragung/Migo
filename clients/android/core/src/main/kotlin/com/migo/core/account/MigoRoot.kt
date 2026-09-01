package com.migo.core.account

import com.migo.core.crypto.Csprng
import com.migo.core.crypto.Kdf

/**
 * The domain labels of the account root, duplicated verbatim from the Rust crate's `root.rs`.
 *
 * Each label is one HKDF-SHA256 expansion of the root, and the labels are versioned constants
 * rather than strings at call sites so the full set lives in exactly one greppable place: a
 * derivation that ever needs to change becomes `/V2` beside the old one, never a silent change
 * under the same name — the day a label changes meaning is the day every existing account's
 * derived keys change too.
 */
object AccountDomains {
    /** The identity domain: login and account authentication (ML-DSA-65). */
    const val IDENTITY = "MIGO/IDENTITY/V1"

    /** The EVM wallet domain: BIP-32 master seed, BIP-44 coin type 60. */
    const val EVM = "MIGO/EVM/V1"

    /** The E2EE domain: the founding device's X3DH identity seeds. */
    const val E2EE = "MIGO/E2EE/V1"

    /** The backup domain: the .migo container's key schedule. */
    const val BACKUP = "MIGO/BACKUP/V1"

    /**
     * The device domain label, documented for completeness only: device credentials are NOT
     * derived from the root (a leaked root alone must not impersonate a registered device), and
     * this label is reserved so a future per-device derivation, if one is ever justified, has a
     * name that cannot collide with the four live domains.
     */
    const val DEVICE = "MIGO/DEVICE/V1"

    /** Sub-label for the founding device's Ed25519 signing seed, under the E2EE domain. */
    const val E2EE_SIGNING = "migo-e2ee-signing-v1"

    /** Sub-label for the founding device's X25519 exchange seed, under the E2EE domain. */
    const val E2EE_EXCHANGE = "migo-e2ee-exchange-v1"
}

/**
 * The account root secret: 32 bytes from which the whole account is derived.
 *
 * It is the only secret a user who loses every device actually needs backed up — everything else
 * is a function of it, except per-device credentials, which are deliberately random so that a
 * leaked root alone cannot log in as a device that is still registered.
 *
 * Kotlin has no drop hook, so the zeroize-on-drop the Rust type enjoys is a [destroy] the owner
 * of the root must call when it goes away; callers that hold a root for the app's lifetime may
 * reasonably never call it, exactly as they hold the session keys for the app's lifetime. The
 * class never renders its bytes: [toString] names the type and nothing else.
 */
class MigoRoot private constructor(private val bytes: ByteArray) {
    private var destroyed = false

    companion object {
        /** Root secret length in bytes. */
        const val LEN = 32

        /** Draws a fresh root from the platform CSPRNG. */
        fun generate(): MigoRoot = MigoRoot(Csprng.bytes(LEN))

        /** Wraps existing root bytes, e.g. after opening a container. Copies its input. */
        fun fromBytes(bytes: ByteArray): MigoRoot {
            if (bytes.size != LEN) {
                throw AccountError.badLength("root secret", LEN, bytes.size)
            }
            return MigoRoot(bytes.copyOf())
        }
    }

    /** The root bytes, for sealing into a container. The copy is the caller's to zeroize. */
    fun asBytes(): ByteArray {
        check(!destroyed) { "MigoRoot has been destroyed" }
        return bytes.copyOf()
    }

    /** Derives the 32-byte seed of one domain. */
    fun domainSeed(label: String): ByteArray {
        check(!destroyed) { "MigoRoot has been destroyed" }
        return Kdf.derive(bytes, null, label, 32)
    }

    /** Zeroes the root. Any later [asBytes] or [domainSeed] throws. */
    fun destroy() {
        bytes.fill(0)
        destroyed = true
    }

    override fun toString(): String = "MigoRoot(<32 bytes>)"
}

/**
 * The founding device's E2EE identity seeds, derived from the E2EE domain.
 *
 * Returns the two 32-byte seeds the existing X3DH identity format is built from (Ed25519
 * signing, X25519 exchange). The E2EE protocol above them is unchanged by the account root;
 * only the *origin* of the founding device's seeds is, which is what makes the account's E2EE
 * history recoverable from a container while additional devices keep generating fresh keys and
 * never inherit historical plaintext.
 */
fun foundingDeviceE2eeSeeds(root: MigoRoot): Pair<ByteArray, ByteArray> {
    val domain = root.domainSeed(AccountDomains.E2EE)
    val signing = Kdf.derive(domain, null, AccountDomains.E2EE_SIGNING, 32)
    val exchange = Kdf.derive(domain, null, AccountDomains.E2EE_EXCHANGE, 32)
    domain.fill(0)
    return Pair(signing, exchange)
}
