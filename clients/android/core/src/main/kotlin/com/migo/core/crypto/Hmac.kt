package com.migo.core.crypto

import com.goterl.lazysodium.interfaces.Auth

/**
 * HMAC-SHA256 built on libsodium's streaming `crypto_auth_hmacsha256` API.
 *
 * Two things force the streaming form over the one-shot `cryptoAuthHMACSha256`. First, HKDF-Extract
 * keys the HMAC with a salt that is not always 32 bytes — the fingerprint salt is 16, an absent
 * salt is empty — and the one-shot demands exactly `HMACSHA256_KEYBYTES` (32); the streaming init
 * takes an arbitrary key length and pads or hashes it per the HMAC spec, which is what RFC 5869
 * requires. Second, [tagParts] authenticates a length-prefixed sequence of parts, and streaming
 * `update` is how you feed a MAC in pieces without allocating and copying the concatenation.
 *
 * An empty key (`keyLen == 0`) is HMAC with a zero-padded key, which is byte-identical to HMAC with
 * a 32-zero key because both pad to the 64-byte block — the reason RFC 5869's "salt not provided"
 * (HashLen zeros) and an empty salt produce the same PRK, as the kdf vectors assert.
 */
internal object Hmac {
    const val OUTPUT_LEN = 32

    /** HMAC-SHA256 of a single message. */
    fun sha256(key: ByteArray, message: ByteArray): ByteArray =
        sha256Parts(key, if (message.isEmpty()) emptyList() else listOf(message))

    /**
     * HMAC-SHA256 over a sequence of parts, fed to the MAC in order.
     *
     * Empty parts are skipped — updating a MAC with zero bytes is a no-op, and a zero-length array
     * is the one argument shape JNA is least happy mapping. The caller is responsible for any
     * length framing between parts (see [MacKey.tagParts]); this only concatenates.
     */
    fun sha256Parts(key: ByteArray, parts: List<ByteArray>): ByteArray {
        val state = Auth.StateHMAC256()
        check(Sodium.auth.cryptoAuthHMACSha256Init(state, key, key.size)) { "hmac init failed" }
        for (part in parts) {
            if (part.isEmpty()) continue
            check(Sodium.auth.cryptoAuthHMACSha256Update(state, part, part.size.toLong())) {
                "hmac update failed"
            }
        }
        val out = ByteArray(OUTPUT_LEN)
        check(Sodium.auth.cryptoAuthHMACSha256Final(state, out)) { "hmac final failed" }
        return out
    }
}
