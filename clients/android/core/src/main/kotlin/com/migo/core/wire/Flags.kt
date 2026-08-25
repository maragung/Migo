package com.migo.core.wire

/**
 * Frame flag bits (MWP/1 byte 1).
 *
 * Reserved bits are rejected rather than ignored. A receiver that silently drops unknown
 * flags can never be given new ones: old peers would already be accepting frames whose
 * meaning they do not understand, and there would be no version at which the bit could
 * safely start to mean something.
 */
object Flags {
    /** Payload is raw-deflate compressed. */
    const val COMPRESSED = 0x01

    /** A 16-byte trace id and an 8-byte span id precede the payload. */
    const val TRACED = 0x02

    /** Payload is a varint count followed by that many length-prefixed sub-frames. */
    const val BATCH = 0x04

    /** Payload is an `Error` struct instead of the opcode's response type. */
    const val ERROR = 0x08

    /** The sender expects an ACK for this frame. */
    const val ACK_REQUIRED = 0x10

    /** A fragment index and total precede the payload. */
    const val FRAGMENT = 0x20

    /** Reserved for a future version. Setting it is a decode error today. */
    const val RESERVED_6 = 0x40

    /** Reserved as a flags-extension escape. Setting it is a decode error today. */
    const val FLAGS_EXT = 0x80

    /** Bits that must be zero in MWP/1. */
    const val RESERVED_MASK = RESERVED_6 or FLAGS_EXT

    /** Bits this version defines. */
    const val KNOWN_MASK = COMPRESSED or TRACED or BATCH or ERROR or ACK_REQUIRED or FRAGMENT
}
