/**
 * Frame flag bits (MWP/1 byte 1).
 *
 * Reserved bits are rejected rather than ignored. A receiver that silently drops
 * unknown flags can never be given new ones: old peers would already be accepting
 * frames whose meaning they do not understand, and there would be no version at which
 * the bit could safely start to mean something.
 */

/** Payload is raw-deflate compressed. */
export const COMPRESSED = 0x01;
/** A 16-byte trace id and an 8-byte span id precede the payload. */
export const TRACED = 0x02;
/** Payload is a varint count followed by that many length-prefixed sub-frames. */
export const BATCH = 0x04;
/** Payload is an `Error` struct instead of the opcode's response type. */
export const ERROR = 0x08;
/** The sender expects an ACK for this frame. */
export const ACK_REQUIRED = 0x10;
/** A fragment index and total precede the payload. */
export const FRAGMENT = 0x20;
/** Reserved for a future version. Setting it is a decode error today. */
export const RESERVED_6 = 0x40;
/** Reserved as a flags-extension escape. Setting it is a decode error today. */
export const FLAGS_EXT = 0x80;

/** Bits that must be zero in MWP/1. */
export const RESERVED_MASK = RESERVED_6 | FLAGS_EXT;
/** Bits this version defines. */
export const KNOWN_MASK = COMPRESSED | TRACED | BATCH | ERROR | ACK_REQUIRED | FRAGMENT;
