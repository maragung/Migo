package com.migo.core.wire

/**
 * Timestamps.
 *
 * On the wire a timestamp is milliseconds since the Migo epoch, 2024-01-01T00:00:00Z.
 * Counting from 1970 would spend a varint byte on 54 years no Migo message can fall in;
 * counting from 2024 fits the next 68 years in five bytes instead of six.
 *
 * In memory a timestamp is Unix milliseconds (a `Long`), so it goes straight into
 * `java.time.Instant.ofEpochMilli(...)` and every date API with no wrapper type. The
 * conversion lives here and nowhere else.
 *
 * Getting this wrong is a 54-year offset, so the conformance vectors pin it: a build that
 * forgets the epoch fails crypto-free, in mse.json, on the first timestamp case.
 */
object WireTime {
    /** Milliseconds between the Unix epoch and the Migo epoch. */
    const val MIGO_EPOCH_MS = 1704067200000L

    /**
     * Converts Unix milliseconds to the wire representation.
     *
     * A timestamp before 2024 cannot be represented and is almost always a default-
     * constructed zero rather than a real date. Clamped rather than rejected, matching
     * `Timestamp::to_wire`, so one bad clock cannot make a whole frame unsendable.
     */
    fun toWire(unixMs: Long): Long {
        val wire = unixMs - MIGO_EPOCH_MS
        return if (wire < 0L) 0L else wire
    }

    /** Converts the wire representation to Unix milliseconds. */
    fun fromWire(wireMs: Long): Long = wireMs + MIGO_EPOCH_MS
}
