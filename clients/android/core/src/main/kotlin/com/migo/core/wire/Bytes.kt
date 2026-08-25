package com.migo.core.wire

/** Somewhere to put bytes. Kept minimal so a [Writer] and a plain accumulator both qualify. */
interface ByteSink {
    fun push(byte: Int)
}

/**
 * A growable byte buffer that satisfies [ByteSink].
 *
 * Used for the pieces of the codec that assemble a small, bounded run of bytes — a frame
 * header, a batch envelope — where the full [Writer] with its optional-field scopes would be
 * more machinery than the job needs.
 */
class ByteAccumulator(initialCapacity: Int = 32) : ByteSink {
    private var buf = ByteArray(initialCapacity.coerceAtLeast(16))
    private var len = 0

    val size: Int get() = len

    override fun push(byte: Int) {
        ensure(1)
        buf[len] = byte.toByte()
        len += 1
    }

    fun append(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        ensure(bytes.size)
        bytes.copyInto(buf, len)
        len += bytes.size
    }

    fun toByteArray(): ByteArray = buf.copyOf(len)

    private fun ensure(additional: Int) {
        val needed = len + additional
        if (needed <= buf.size) return
        var capacity = maxOf(buf.size, 16) * 2
        while (capacity < needed) capacity *= 2
        buf = buf.copyOf(capacity)
    }
}
