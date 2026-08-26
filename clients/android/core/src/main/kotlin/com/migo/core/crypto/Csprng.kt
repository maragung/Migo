package com.migo.core.crypto

import java.security.SecureRandom

/**
 * The one place client-side key material is drawn from.
 *
 * The Rust crate threads a `&mut dyn Random` through every function that consumes entropy so a
 * deterministic simulation can inject a seeded generator and a reviewer can see, in the signature,
 * exactly which functions take randomness. Kotlin has no such handle to pass, and the equivalent
 * hazard is a test helper that quietly returns weak bytes past a reviewer who does not notice. The
 * countermeasure is the mirror image of the Rust one: there is exactly one source of random bytes,
 * it is the platform CSPRNG, and it cannot be swapped for anything weaker. Determinism, where a
 * test needs it, is routed through the `*WithNonce` and `fromSeed` entry points that take their
 * bytes as arguments instead.
 *
 * `SecureRandom` with its default construction is the platform CSPRNG on Android and is seeded
 * from the OS entropy pool; it is the same primitive `crypto.getRandomValues` resolves to on the
 * web. It is deliberately not made injectable: a fallback that could be swapped for a
 * non-cryptographic generator would produce keys that look random in every test and are
 * predictable in production, which is the single worst failure a key generator can have.
 */
internal object Csprng {
    private val secureRandom = SecureRandom()

    /** Returns [length] cryptographically secure random bytes. */
    fun bytes(length: Int): ByteArray {
        val out = ByteArray(length)
        secureRandom.nextBytes(out)
        return out
    }

    /** Fills [out] with cryptographically secure random bytes. */
    fun fill(out: ByteArray) {
        secureRandom.nextBytes(out)
    }
}
