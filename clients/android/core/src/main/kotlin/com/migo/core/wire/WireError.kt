package com.migo.core.wire

/**
 * The closed set of things that can go wrong in the codec.
 *
 * The names are the Rust enum's variant names, character for character, because the
 * conformance vectors in shared/protocol/vectors name the expected failure and all three
 * implementations have to read that name the same way. A vector that says `NonMinimalVarint`
 * must mean the same thing here as it does in `migo-wire`.
 */
enum class WireErrorKind {
    UnexpectedEnd,
    VarintTooLong,
    NonMinimalVarint,
    StringTooLong,
    BytesTooLong,
    ListTooLong,
    DepthExceeded,
    FrameTooLarge,
    InvalidUtf8,
    InvalidBool,
    UnsupportedVersion,
    ReservedFlags,
    TrailingBytes,
    BatchTooLarge,
    NestedBatch,
    DecompressFailed,
    DecompressedTooLarge,
    LengthOverflow,
    InvalidFragment,
    FieldOverflow,
}

/**
 * A codec failure.
 *
 * These errors never carry payload bytes. A hex dump of attacker-controlled data in a log
 * line is a log-injection vector and, if the frame was a private message, a privacy incident.
 * The message strings hold only numbers, offsets, and static field names chosen by this
 * package — never peer-supplied text.
 */
class WireError(val kind: WireErrorKind, message: String) : Exception(message) {
    companion object {
        fun unexpectedEnd(offset: Int, needed: Int) =
            WireError(WireErrorKind.UnexpectedEnd, "unexpected end of input: needed $needed more bytes at offset $offset")

        fun varintTooLong(offset: Int, max: Int) =
            WireError(WireErrorKind.VarintTooLong, "varint longer than $max bytes at offset $offset")

        fun nonMinimalVarint(offset: Int) =
            WireError(WireErrorKind.NonMinimalVarint, "non-minimal varint encoding at offset $offset")

        fun stringTooLong(len: Long, max: Int) =
            WireError(WireErrorKind.StringTooLong, "string field is $len bytes, limit is $max")

        fun bytesTooLong(len: Long, max: Int) =
            WireError(WireErrorKind.BytesTooLong, "bytes field is $len bytes, limit is $max")

        fun listTooLong(len: Long, max: Int) =
            WireError(WireErrorKind.ListTooLong, "list has $len items, limit is $max")

        fun depthExceeded(max: Int) =
            WireError(WireErrorKind.DepthExceeded, "struct nesting deeper than $max")

        fun frameTooLarge(len: Long, max: Int) =
            WireError(WireErrorKind.FrameTooLarge, "frame is $len bytes, limit is $max")

        fun invalidUtf8() =
            WireError(WireErrorKind.InvalidUtf8, "string field is not valid UTF-8")

        fun invalidBool(found: Int) =
            WireError(WireErrorKind.InvalidBool, "boolean byte is $found, expected 0 or 1")

        fun unsupportedVersion(found: Int, supported: Int) =
            WireError(WireErrorKind.UnsupportedVersion, "unsupported protocol version $found, this build speaks $supported")

        fun reservedFlags(bits: Int) =
            WireError(WireErrorKind.ReservedFlags, "reserved flag bits set: 0x%02x".format(bits))

        fun trailingBytes(count: Int) =
            WireError(WireErrorKind.TrailingBytes, "$count trailing bytes after decoding")

        fun batchTooLarge(len: Long, max: Int) =
            WireError(WireErrorKind.BatchTooLarge, "batch has $len items, limit is $max")

        fun nestedBatch() =
            WireError(WireErrorKind.NestedBatch, "nested batch frames are not allowed")

        fun decompressFailed() =
            WireError(WireErrorKind.DecompressFailed, "cannot decompress payload")

        fun decompressedTooLarge(max: Int) =
            WireError(WireErrorKind.DecompressedTooLarge, "payload expands past the $max byte limit when decompressed")

        fun lengthOverflow(len: ULong) =
            WireError(WireErrorKind.LengthOverflow, "length prefix $len does not fit an array index")

        fun invalidFragment(index: Long, total: Long) =
            WireError(WireErrorKind.InvalidFragment, "invalid fragment $index of $total")

        /** `field` is a static string chosen by this package, never peer-supplied text. */
        fun fieldOverflow(field: String) =
            WireError(WireErrorKind.FieldOverflow, "value does not fit field `$field`")
    }
}
