package com.migo.core.crypto

import org.bouncycastle.crypto.generators.Argon2BytesGenerator
import org.bouncycastle.crypto.params.Argon2Parameters

/**
 * Argon2id (RFC 9106) with the parameters chosen by the caller.
 *
 * The sodium bundle exposes Argon2id only at one lane count, and the `.migo` container format
 * rides its parameters in the file header — a container sealed with four lanes must open with
 * four lanes, whatever this build's own default is. BouncyCastle's generator takes them all.
 * The caller validates the ranges (see `ContainerParams.validate`); this wrapper only feeds the
 * values through.
 *
 * Version 0x13 (1.3) is fixed: the format writes no version field, so every container this
 * client has ever read was sealed at 1.3, and 1.0 exists only in pre-standard implementations.
 */
object Argon2 {
    /** Derives [outputLength] bytes from [passphrase] under the given cost parameters. */
    fun derive(
        passphrase: ByteArray,
        salt: ByteArray,
        memoryKib: Int,
        passes: Int,
        lanes: Int,
        outputLength: Int,
    ): ByteArray {
        val params = Argon2Parameters.Builder(Argon2Parameters.ARGON2_id)
            .withVersion(Argon2Parameters.ARGON2_VERSION_13)
            .withIterations(passes)
            .withMemoryAsKB(memoryKib)
            .withParallelism(lanes)
            .withSalt(salt.copyOf())
            .build()
        val generator = Argon2BytesGenerator()
        generator.init(params)
        val out = ByteArray(outputLength)
        generator.generateBytes(passphrase.copyOf(), out)
        return out
    }
}
