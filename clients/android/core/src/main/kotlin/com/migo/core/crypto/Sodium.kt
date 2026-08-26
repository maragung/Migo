package com.migo.core.crypto

import com.goterl.lazysodium.LazySodium
import com.goterl.lazysodium.LazySodiumAndroid
import com.goterl.lazysodium.SodiumAndroid
import com.goterl.lazysodium.interfaces.AEAD
import com.goterl.lazysodium.interfaces.Auth
import com.goterl.lazysodium.interfaces.DiffieHellman
import com.goterl.lazysodium.interfaces.Sign

/**
 * The single libsodium handle every primitive in this package draws on.
 *
 * ADR-0003 allows audited implementations only, and on Android that means libsodium through
 * Lazysodium: XChaCha20-Poly1305, X25519, Ed25519 and HMAC-SHA256 all come from the same C
 * library the server links, so a byte produced here matches a byte produced there. Nothing in
 * this package implements a cryptographic transform of its own; it assembles RFC constructions
 * (HKDF over HMAC, the Double Ratchet) on top of these primitives, the same way `@migo/crypto`
 * assembles them on top of `@noble/*`.
 *
 * The instance is resolved lazily so that a process that never touches crypto never loads the
 * native library. Production always uses the bundled Android build. Tests inject a desktop
 * `LazySodiumJava` through [overrideForTesting] so the conformance vectors can run on the host
 * JVM, where the Android `.so` will not load — the reason every consumer here depends on the
 * `*.Native` interfaces rather than on a concrete class.
 */
internal object Sodium {
    @Volatile
    private var override: LazySodium? = null

    private val default: LazySodium by lazy { LazySodiumAndroid(SodiumAndroid()) }

    private val impl: LazySodium
        get() = override ?: default

    /** Ed25519 signing and verification. */
    val sign: Sign.Native get() = impl

    /** XChaCha20-Poly1305 authenticated encryption. */
    val aead: AEAD.Native get() = impl

    /** HMAC-SHA256, used for HKDF and for the MACs the server signs for itself. */
    val auth: Auth.Native get() = impl

    /** X25519 Diffie-Hellman. libsodium exposes `crypto_scalarmult` under DiffieHellman, not a ScalarMult interface. */
    val dh: DiffieHellman.Native get() = impl

    /**
     * Injects a libsodium handle for host-JVM tests.
     *
     * The Android artifact bundles an `.so` that only loads on a device or emulator, so unit
     * tests pass a `LazySodiumJava(SodiumJava())` here, which bundles desktop libsodium and loads
     * on the CI runner. Passing `null` restores the default.
     */
    internal fun overrideForTesting(lazySodium: LazySodium?) {
        override = lazySodium
    }
}
