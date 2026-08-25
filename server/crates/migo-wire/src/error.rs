//! Codec failures.
//!
//! Every variant names the limit or invariant that was violated and, where
//! useful, the offending value. Two reasons: a decode failure is usually either
//! a protocol version mismatch or an attack, and both are much easier to tell
//! apart from `StringTooLong { len: 4294967295, max: 65536 }` than from
//! `"decode error"`.
//!
//! These errors never carry payload bytes. A hex dump of attacker-controlled
//! data in a log line is a log-injection vector and, if the frame was a private
//! message, a privacy incident.

/// What went wrong while encoding or decoding.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The buffer ended in the middle of a value.
    #[error("unexpected end of input: needed {needed} more bytes at offset {offset}")]
    UnexpectedEnd {
        /// Read position when the buffer ran out.
        offset: usize,
        /// How many more bytes the value required.
        needed: usize,
    },

    /// A varint kept setting the continuation bit past the legal length.
    #[error("varint longer than {max} bytes at offset {offset}")]
    VarintTooLong {
        /// Where the varint started.
        offset: usize,
        /// Longest accepted encoding.
        max: usize,
    },

    /// A varint was padded with redundant zero groups.
    ///
    /// Rejected because the codec must be canonical: mesh handshakes sign frame
    /// bytes, and two encodings of the same value would mean two valid
    /// signatures for one logical message.
    #[error("non-minimal varint encoding at offset {offset}")]
    NonMinimalVarint {
        /// Where the varint started.
        offset: usize,
    },

    /// A string field exceeded its limit.
    #[error("string field is {len} bytes, limit is {max}")]
    StringTooLong {
        /// Claimed length.
        len: usize,
        /// Configured limit.
        max: usize,
    },

    /// A byte field exceeded its limit.
    #[error("bytes field is {len} bytes, limit is {max}")]
    BytesTooLong {
        /// Claimed length.
        len: usize,
        /// Configured limit.
        max: usize,
    },

    /// A list field exceeded its limit.
    #[error("list has {len} items, limit is {max}")]
    ListTooLong {
        /// Claimed item count.
        len: usize,
        /// Configured limit.
        max: usize,
    },

    /// Struct nesting exceeded the limit. Bounds recursion during decode.
    #[error("struct nesting deeper than {max}")]
    DepthExceeded {
        /// Configured limit.
        max: usize,
    },

    /// A frame exceeded the size limit.
    #[error("frame is {len} bytes, limit is {max}")]
    FrameTooLarge {
        /// Actual or claimed length.
        len: usize,
        /// Configured limit.
        max: usize,
    },

    /// A string field was not valid UTF-8.
    #[error("string field is not valid UTF-8")]
    InvalidUtf8,

    /// A boolean byte was neither `0` nor `1`.
    ///
    /// Rejected for the same reason a non-minimal varint is: the codec has to be
    /// canonical. If `0x02` also meant `true`, one logical message would have
    /// many valid byte encodings, and mesh frames are signed and deduplicated by
    /// hash. "Spare bits for a future version" is not a use for this byte — MSE
    /// extends by adding optional field ids, not by widening a required bool.
    #[error("boolean byte is {found}, expected 0 or 1")]
    InvalidBool {
        /// The offending byte.
        found: u8,
    },

    /// The frame declared a protocol version this build does not speak.
    #[error("unsupported protocol version {found}, this build speaks {supported}")]
    UnsupportedVersion {
        /// Version byte from the frame.
        found: u8,
        /// Version this build implements.
        supported: u8,
    },

    /// The frame set flag bits that are reserved in this protocol version.
    ///
    /// Rejected rather than ignored: a receiver that silently drops unknown
    /// flags cannot later assign meaning to them, because old peers would
    /// already be accepting frames they do not understand.
    #[error("reserved flag bits set: 0x{bits:02x}")]
    ReservedFlags {
        /// The offending bits, masked.
        bits: u8,
    },

    /// A frame or field had bytes left over after decoding finished.
    #[error("{count} trailing bytes after decoding")]
    TrailingBytes {
        /// How many bytes were left.
        count: usize,
    },

    /// A batch declared more items than the limit allows.
    #[error("batch has {len} items, limit is {max}")]
    BatchTooLarge {
        /// Claimed item count.
        len: usize,
        /// Configured limit.
        max: usize,
    },

    /// A batch item was itself a batch.
    #[error("nested batch frames are not allowed")]
    NestedBatch,

    /// Compressed payload could not be inflated.
    #[error("cannot decompress payload")]
    DecompressFailed,

    /// The payload inflated past the frame limit — a decompression bomb.
    #[error("payload expands past the {max} byte limit when decompressed")]
    DecompressedTooLarge {
        /// Configured limit.
        max: usize,
    },

    /// A length prefix did not fit in `usize` on this platform.
    #[error("length prefix {len} does not fit in a usize")]
    LengthOverflow {
        /// Claimed length.
        len: u64,
    },

    /// A fragmented frame carried an impossible index/total pair.
    ///
    /// A total of zero, or an index at or past the total, cannot be reassembled.
    /// Accepting it would let a peer keep a reassembly buffer alive forever.
    #[error("invalid fragment {index} of {total}")]
    InvalidFragment {
        /// Zero-based index from the frame.
        index: u32,
        /// Declared total.
        total: u32,
    },

    /// A wire value did not fit the field's declared width.
    ///
    /// Varints are decoded as `u64` and then narrowed. The field name is a
    /// static string chosen by this crate, never peer-supplied text.
    #[error("value does not fit field `{field}`")]
    FieldOverflow {
        /// Name of the field that overflowed.
        field: &'static str,
    },
}

/// Codec result alias.
pub type Result<T, E = WireError> = core::result::Result<T, E>;
