//! MSE decoder.
//!
//! Every method here treats its input as hostile. The rule the whole module is
//! built around: **a length is validated against the bytes actually available
//! before a single byte is allocated.** A four-byte frame that claims a
//! four-gigabyte string must cost four bytes to reject, not four gigabytes to
//! discover. That one property is the difference between a codec and a remote
//! out-of-memory primitive.
//!
//! The reader holds a [`Bytes`], so sub-readers for optional fields are
//! reference-counted slices rather than copies — skipping an unknown field is
//! free, and so is handing a nested struct its own view of the payload.

use bytes::Bytes;
use migo_core::{Id, Timestamp};

use crate::error::{Result, WireError};
use crate::limits::{
    MAX_BYTES_LEN, MAX_FRAME_BYTES, MAX_LIST_ITEMS, MAX_NESTING_DEPTH, MAX_STRING_BYTES,
};
use crate::varint;

/// Decodes an MSE payload.
#[derive(Clone, Debug)]
pub struct Reader {
    input: Bytes,
    pos: usize,
    depth: usize,
}

impl Reader {
    /// Wraps an owned payload. Cheap: `Bytes` is reference-counted.
    #[must_use]
    pub fn new(input: Bytes) -> Self {
        Self {
            input,
            pos: 0,
            depth: 0,
        }
    }

    /// Wraps a borrowed payload by copying it. Convenient in tests; prefer
    /// [`Reader::new`] on the hot path, where the transport already owns a
    /// `Bytes`.
    #[must_use]
    pub fn from_slice(input: &[u8]) -> Self {
        Self::new(Bytes::copy_from_slice(input))
    }

    /// Bytes not yet consumed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.pos)
    }

    /// True when the payload is fully consumed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Current read offset, for error reporting.
    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Asserts the payload was fully consumed.
    ///
    /// Called at the top level only. Trailing bytes there mean the sender and
    /// receiver disagree about the schema, which is worth failing on: silently
    /// ignoring them turns a version mismatch into a mystery.
    pub fn finish(&self) -> Result<()> {
        if self.remaining() > 0 {
            return Err(WireError::TrailingBytes {
                count: self.remaining(),
            });
        }
        Ok(())
    }

    /// Opens a struct. Bounds recursion.
    pub fn enter(&mut self) -> Result<()> {
        if self.depth >= MAX_NESTING_DEPTH {
            return Err(WireError::DepthExceeded {
                max: MAX_NESTING_DEPTH,
            });
        }
        self.depth += 1;
        Ok(())
    }

    /// Closes a struct.
    pub fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Reads one byte as a boolean, accepting only `0` and `1`.
    ///
    /// Anything else is an error rather than "truthy". The codec is canonical by
    /// rule, and a byte with eight legal encodings of `true` breaks that rule the
    /// same way a padded varint would. Forward compatibility in MSE comes from
    /// optional field ids, not from spare bits inside a required field.
    pub fn read_bool(&mut self) -> Result<bool> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            found => Err(WireError::InvalidBool { found }),
        }
    }

    /// Reads a varint that must fit in 32 bits.
    pub fn read_u32(&mut self) -> Result<u32> {
        let value = self.read_varint()?;
        u32::try_from(value).map_err(|_| WireError::LengthOverflow { len: value })
    }

    /// Reads a varint.
    pub fn read_u64(&mut self) -> Result<u64> {
        self.read_varint()
    }

    /// Reads a timestamp from Migo-epoch milliseconds.
    pub fn read_timestamp(&mut self) -> Result<Timestamp> {
        Ok(Timestamp::from_wire(self.read_varint()?))
    }

    /// Reads a 16-byte identifier.
    pub fn read_id(&mut self) -> Result<Id> {
        let bytes = self.take(16)?;
        let mut raw = [0u8; 16];
        raw.copy_from_slice(&bytes);
        Ok(Id::from_bytes(raw))
    }

    /// Reads a length-prefixed UTF-8 string.
    pub fn read_string(&mut self) -> Result<String> {
        let len = self.read_length(MAX_STRING_BYTES, |len, max| WireError::StringTooLong {
            len,
            max,
        })?;
        let bytes = self.take(len)?;
        // Validate before allocating a String; from_utf8 on Bytes would copy first.
        match std::str::from_utf8(&bytes) {
            Ok(text) => Ok(text.to_owned()),
            Err(_) => Err(WireError::InvalidUtf8),
        }
    }

    /// Reads a length-prefixed opaque byte field.
    pub fn read_bytes(&mut self) -> Result<Vec<u8>> {
        let len = self.read_length(MAX_BYTES_LEN, |len, max| WireError::BytesTooLong {
            len,
            max,
        })?;
        Ok(self.take(len)?.to_vec())
    }

    /// Reads a length-prefixed opaque byte field without copying.
    pub fn read_bytes_shared(&mut self) -> Result<Bytes> {
        let len = self.read_length(MAX_BYTES_LEN, |len, max| WireError::BytesTooLong {
            len,
            max,
        })?;
        self.take(len)
    }

    /// Reads a list header and returns the item count.
    ///
    /// Checked against both [`MAX_LIST_ITEMS`] and the remaining bytes: every
    /// item costs at least one byte, so a count larger than what is left is a
    /// lie, and callers size a `Vec` from this number.
    pub fn read_list_len(&mut self) -> Result<usize> {
        let raw = self.read_varint()?;
        let len = usize::try_from(raw).map_err(|_| WireError::LengthOverflow { len: raw })?;
        if len > MAX_LIST_ITEMS {
            return Err(WireError::ListTooLong {
                len,
                max: MAX_LIST_ITEMS,
            });
        }
        if len > self.remaining() {
            return Err(WireError::ListTooLong {
                len,
                max: self.remaining(),
            });
        }
        Ok(len)
    }

    /// Reads one optional field header and returns its id together with a reader
    /// scoped to exactly that field's bytes.
    ///
    /// Scoping is what makes an unknown field safe to ignore: the caller drops
    /// the sub-reader and the outer position has already advanced past the whole
    /// field. A malformed unknown field therefore cannot desynchronise the
    /// stream.
    pub fn read_optional(&mut self) -> Result<(u32, Reader)> {
        let field_id = self.read_u32()?;
        let len = self.read_length(MAX_FRAME_BYTES, |len, max| WireError::FrameTooLarge {
            len,
            max,
        })?;
        let bytes = self.take(len)?;
        Ok((
            field_id,
            Reader {
                input: bytes,
                pos: 0,
                depth: self.depth,
            },
        ))
    }

    fn read_u8(&mut self) -> Result<u8> {
        let byte = *self.input.get(self.pos).ok_or(WireError::UnexpectedEnd {
            offset: self.pos,
            needed: 1,
        })?;
        self.pos += 1;
        Ok(byte)
    }

    fn read_varint(&mut self) -> Result<u64> {
        let (value, used) = varint::decode_u64(&self.input, self.pos)?;
        self.pos += used;
        Ok(value)
    }

    /// Reads a length prefix and rejects it against both the configured limit and
    /// the bytes actually present, before the caller allocates anything.
    fn read_length(
        &mut self,
        max: usize,
        too_long: impl Fn(usize, usize) -> WireError,
    ) -> Result<usize> {
        let raw = self.read_varint()?;
        let len = usize::try_from(raw).map_err(|_| WireError::LengthOverflow { len: raw })?;
        if len > max {
            return Err(too_long(len, max));
        }
        if len > self.remaining() {
            return Err(WireError::UnexpectedEnd {
                offset: self.pos,
                needed: len - self.remaining(),
            });
        }
        Ok(len)
    }

    /// Consumes `len` bytes as a shared slice.
    fn take(&mut self, len: usize) -> Result<Bytes> {
        if len > self.remaining() {
            return Err(WireError::UnexpectedEnd {
                offset: self.pos,
                needed: len - self.remaining(),
            });
        }
        let slice = self.input.slice(self.pos..self.pos + len);
        self.pos += len;
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::Writer;

    #[test]
    fn reads_back_what_the_writer_wrote() {
        let mut w = Writer::new();
        w.write_u32(300);
        w.write_u64(u64::MAX);
        w.write_bool(true);
        w.write_str("halo").expect("writes");
        w.write_bytes(&[1, 2, 3]).expect("writes");
        w.write_id(&Id::from_bytes([4u8; 16]));
        w.write_timestamp(Timestamp::from_millis(9_999));
        let payload = w.finish().expect("finishes");

        let mut r = Reader::new(payload);
        assert_eq!(r.read_u32().expect("u32"), 300);
        assert_eq!(r.read_u64().expect("u64"), u64::MAX);
        assert!(r.read_bool().expect("bool"));
        assert_eq!(r.read_string().expect("string"), "halo");
        assert_eq!(r.read_bytes().expect("bytes"), vec![1, 2, 3]);
        assert_eq!(r.read_id().expect("id"), Id::from_bytes([4u8; 16]));
        assert_eq!(
            r.read_timestamp().expect("timestamp"),
            Timestamp::from_millis(9_999)
        );
        r.finish().expect("fully consumed");
    }

    #[test]
    fn a_string_length_beyond_the_buffer_is_rejected_without_allocating() {
        // Claims 4 GiB of string in five bytes.
        let hostile = [0xFF, 0xFF, 0xFF, 0xFF, 0x0F];
        let mut r = Reader::from_slice(&hostile);
        let error = r.read_string().expect_err("must be rejected");
        assert!(
            matches!(error, WireError::StringTooLong { .. }),
            "expected a limit rejection, got {error:?}"
        );
    }

    #[test]
    fn a_plausible_but_absent_length_is_rejected() {
        // Length 100, but only three bytes follow.
        let truncated = [100u8, b'a', b'b', b'c'];
        let mut r = Reader::from_slice(&truncated);
        assert_eq!(
            r.read_string(),
            Err(WireError::UnexpectedEnd {
                offset: 1,
                needed: 97
            })
        );
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let mut r = Reader::from_slice(&[2, 0xFF, 0xFE]);
        assert_eq!(r.read_string(), Err(WireError::InvalidUtf8));
    }

    #[test]
    fn list_counts_are_bounded_by_the_remaining_bytes() {
        // Claims 4000 items with nothing following.
        let mut w = Writer::new();
        w.list_len(4000).expect("writes");
        let mut r = Reader::new(w.finish().expect("finishes"));
        assert!(matches!(
            r.read_list_len(),
            Err(WireError::ListTooLong { .. })
        ));
    }

    #[test]
    fn list_counts_within_the_payload_are_accepted() {
        let mut w = Writer::new();
        w.list_len(3).expect("writes");
        for value in 1..=3u32 {
            w.write_u32(value);
        }
        let mut r = Reader::new(w.finish().expect("finishes"));
        assert_eq!(r.read_list_len().expect("len"), 3);
        for value in 1..=3u32 {
            assert_eq!(r.read_u32().expect("item"), value);
        }
        r.finish().expect("consumed");
    }

    #[test]
    fn unknown_optional_fields_are_skipped_by_length() {
        let mut w = Writer::new();
        w.write_u32(2); // optional_count
        w.optional(1, |w| {
            w.write_str("known")?;
            Ok(())
        })
        .expect("writes");
        w.optional(999, |w| {
            // A field from a future protocol version.
            w.write_bytes(&[0xDE, 0xAD, 0xBE, 0xEF])?;
            w.write_str("extra")?;
            Ok(())
        })
        .expect("writes");
        let payload = w.finish().expect("finishes");

        let mut r = Reader::new(payload);
        let count = r.read_u32().expect("count");
        assert_eq!(count, 2);
        let mut seen = Vec::new();
        for _ in 0..count {
            let (field_id, mut sub) = r.read_optional().expect("optional header");
            // An unknown id is dropped without being read: the outer reader has
            // already skipped past the field's bytes.
            if field_id == 1 {
                seen.push(sub.read_string().expect("string"));
            }
        }
        assert_eq!(seen, vec!["known".to_string()]);
        r.finish().expect("outer stream stayed in sync");
    }

    #[test]
    fn a_corrupt_unknown_field_cannot_desynchronise_the_stream() {
        let mut w = Writer::new();
        w.optional(500, |w| {
            // Claims a 200-byte string inside a field that is only a few bytes long.
            w.write_u32(200);
            Ok(())
        })
        .expect("writes");
        w.write_u32(0xABC);
        let payload = w.finish().expect("finishes");

        let mut r = Reader::new(payload);
        let (field_id, mut sub) = r.read_optional().expect("header");
        assert_eq!(field_id, 500);
        // Reading inside the corrupt field fails, but only inside it.
        assert!(sub.read_string().is_err());
        assert_eq!(r.read_u32().expect("next field still readable"), 0xABC);
    }

    #[test]
    fn trailing_bytes_are_an_error_at_the_top_level() {
        let mut r = Reader::from_slice(&[0x01, 0x02]);
        assert_eq!(r.read_u32().expect("u32"), 1);
        assert_eq!(r.finish(), Err(WireError::TrailingBytes { count: 1 }));
    }

    #[test]
    fn nesting_depth_is_bounded() {
        let mut r = Reader::from_slice(&[]);
        for _ in 0..MAX_NESTING_DEPTH {
            r.enter().expect("within limit");
        }
        assert_eq!(
            r.enter(),
            Err(WireError::DepthExceeded {
                max: MAX_NESTING_DEPTH
            })
        );
    }

    #[test]
    fn sub_readers_inherit_depth_so_nesting_stays_bounded() {
        let mut w = Writer::new();
        w.optional(1, |w| {
            w.write_u32(1);
            Ok(())
        })
        .expect("writes");
        let mut r = Reader::new(w.finish().expect("finishes"));
        for _ in 0..MAX_NESTING_DEPTH {
            r.enter().expect("within limit");
        }
        r.leave();
        let (_, sub) = r.read_optional().expect("header");
        assert_eq!(sub.depth, MAX_NESTING_DEPTH - 1);
    }

    #[test]
    fn truncated_ids_are_rejected() {
        let mut r = Reader::from_slice(&[1u8; 15]);
        assert_eq!(
            r.read_id(),
            Err(WireError::UnexpectedEnd {
                offset: 0,
                needed: 1
            })
        );
    }

    #[test]
    fn u32_fields_reject_values_that_do_not_fit() {
        let mut w = Writer::new();
        w.write_u64(u64::from(u32::MAX) + 1);
        let mut r = Reader::new(w.finish().expect("finishes"));
        assert!(matches!(
            r.read_u32(),
            Err(WireError::LengthOverflow { .. })
        ));
    }

    #[test]
    fn a_boolean_byte_other_than_zero_or_one_is_rejected() {
        // Canonical encoding: `true` has exactly one representation. Accepting
        // 0x02 as "truthy" would give one message many valid signatures.
        assert!(Reader::from_slice(&[0x00]).read_bool() == Ok(false));
        assert!(Reader::from_slice(&[0x01]).read_bool() == Ok(true));
        for byte in [0x02u8, 0x7f, 0x80, 0xff] {
            assert_eq!(
                Reader::from_slice(&[byte]).read_bool(),
                Err(WireError::InvalidBool { found: byte }),
                "byte 0x{byte:02x} must not decode as a boolean"
            );
        }
    }
}
