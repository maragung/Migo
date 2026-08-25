//! MSE encoder.
//!
//! Migo Struct Encoding has two halves. **Required** fields are written
//! positionally — no tags, no lengths, nothing but the values in schema order —
//! which is where the byte savings over protobuf come from. **Optional** fields
//! follow a count and each carries `(field_id, byte_len, bytes)`, so a receiver
//! can skip a field it has never heard of. That is what makes the protocol
//! forward compatible without a negotiation step: a v1 client can read a v2
//! server's frames as long as the required prefix has not changed.
//!
//! The consequence, stated plainly because it constrains every future schema
//! change: **the required prefix of a struct is frozen for the life of the
//! protocol version.** New fields are optional, or they are a new protocol
//! version. `tools/protocol-codegen` enforces the ordering rule; this module
//! implements the encoding.

use bytes::Bytes;
use migo_core::{Id, Timestamp};

use crate::error::{Result, WireError};
use crate::limits::{
    MAX_BYTES_LEN, MAX_FRAME_BYTES, MAX_LIST_ITEMS, MAX_NESTING_DEPTH, MAX_STRING_BYTES,
};
use crate::varint;

/// Builds an MSE payload.
///
/// Scalar writes are infallible: they cannot exceed a limit by themselves. Writes
/// that carry a length — strings, byte fields, lists, optional fields — are
/// fallible, and their limits are checked *before* the bytes are appended.
#[derive(Debug)]
pub struct Writer {
    buf: Vec<u8>,
    /// Buffers of enclosing `optional()` calls, innermost last.
    saved: Vec<Vec<u8>>,
    /// Spare buffers available for reuse by the next `optional()` call.
    ///
    /// Kept separate from `saved` on purpose. An earlier version used one stack
    /// for both, and a nested `optional()` then recycled its own parent's
    /// buffer — silently discarding the enclosing struct's bytes.
    pool: Vec<Vec<u8>>,
    /// Combined length of everything in `saved`, so the size guard is O(1).
    outer_len: usize,
    depth: usize,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    /// A writer with a small default capacity, sized for a typical text message.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    /// A writer with a preallocated buffer.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity.min(MAX_FRAME_BYTES)),
            saved: Vec::new(),
            pool: Vec::new(),
            outer_len: 0,
            depth: 0,
        }
    }

    /// Total bytes written so far, including enclosing optional buffers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.outer_len + self.buf.len()
    }

    /// True when nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrows the encoded bytes. Only meaningful at depth zero.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Finishes encoding and returns the payload.
    ///
    /// Fails if the payload exceeds [`MAX_FRAME_BYTES`]: better to refuse to
    /// build a frame than to hand the peer something it is required to reject.
    pub fn finish(self) -> Result<Bytes> {
        if self.buf.len() > MAX_FRAME_BYTES {
            return Err(WireError::FrameTooLarge {
                len: self.buf.len(),
                max: MAX_FRAME_BYTES,
            });
        }
        Ok(Bytes::from(self.buf))
    }

    /// Finishes encoding into a `Vec`.
    pub fn finish_vec(self) -> Result<Vec<u8>> {
        if self.buf.len() > MAX_FRAME_BYTES {
            return Err(WireError::FrameTooLarge {
                len: self.buf.len(),
                max: MAX_FRAME_BYTES,
            });
        }
        Ok(self.buf)
    }

    /// Opens a struct. Bounds nesting so a schema cycle cannot blow the stack.
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

    /// Writes a boolean as one byte.
    pub fn write_bool(&mut self, value: bool) {
        self.buf.push(u8::from(value));
    }

    /// Writes an unsigned 32-bit value as a varint.
    pub fn write_u32(&mut self, value: u32) {
        varint::encode_u64(u64::from(value), &mut self.buf);
    }

    /// Writes an unsigned 64-bit value as a varint.
    pub fn write_u64(&mut self, value: u64) {
        varint::encode_u64(value, &mut self.buf);
    }

    /// Writes a timestamp as a varint of Migo-epoch milliseconds.
    pub fn write_timestamp(&mut self, value: Timestamp) {
        varint::encode_u64(value.to_wire(), &mut self.buf);
    }

    /// Writes an identifier as 16 raw bytes.
    ///
    /// Not length-prefixed and not text: an id is a fixed-width value, so a
    /// prefix would be a wasted byte on every single one.
    pub fn write_id(&mut self, value: &Id) {
        self.buf.extend_from_slice(value.as_bytes());
    }

    /// Writes a UTF-8 string as varint length then bytes.
    pub fn write_str(&mut self, value: &str) -> Result<()> {
        let bytes = value.as_bytes();
        if bytes.len() > MAX_STRING_BYTES {
            return Err(WireError::StringTooLong {
                len: bytes.len(),
                max: MAX_STRING_BYTES,
            });
        }
        self.guard_growth(bytes.len())?;
        varint::encode_u64(bytes.len() as u64, &mut self.buf);
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// Writes an opaque byte field as varint length then bytes.
    pub fn write_bytes(&mut self, value: &[u8]) -> Result<()> {
        if value.len() > MAX_BYTES_LEN {
            return Err(WireError::BytesTooLong {
                len: value.len(),
                max: MAX_BYTES_LEN,
            });
        }
        self.guard_growth(value.len())?;
        varint::encode_u64(value.len() as u64, &mut self.buf);
        self.buf.extend_from_slice(value);
        Ok(())
    }

    /// Writes a list header. The caller then writes exactly `len` items.
    pub fn list_len(&mut self, len: usize) -> Result<()> {
        if len > MAX_LIST_ITEMS {
            return Err(WireError::ListTooLong {
                len,
                max: MAX_LIST_ITEMS,
            });
        }
        varint::encode_u64(len as u64, &mut self.buf);
        Ok(())
    }

    /// Writes one optional field: `field_id`, byte length, then the bytes
    /// produced by `write`.
    ///
    /// The length prefix is what makes an unknown field skippable, and it cannot
    /// be known before the field is encoded. Rather than allocate a temporary
    /// buffer per field, the writer swaps its own buffer for a recycled one, runs
    /// the closure, and swaps back — so a message with ten optional fields does
    /// no more allocation than one with none.
    pub fn optional<F>(&mut self, field_id: u32, write: F) -> Result<()>
    where
        F: FnOnce(&mut Writer) -> Result<()>,
    {
        if self.saved.len() >= MAX_NESTING_DEPTH {
            return Err(WireError::DepthExceeded {
                max: MAX_NESTING_DEPTH,
            });
        }

        // Take a spare buffer if a completed sibling left one behind.
        let mut inner = self.pool.pop().unwrap_or_default();
        inner.clear();
        let outer = std::mem::replace(&mut self.buf, inner);
        self.outer_len += outer.len();
        self.saved.push(outer);

        let result = write(self);

        // Restore the outer buffer whether or not the closure succeeded, so the
        // writer is never left with a field's buffer as its own.
        let inner = match self.saved.pop() {
            Some(outer) => {
                self.outer_len -= outer.len();
                std::mem::replace(&mut self.buf, outer)
            }
            // Unreachable: the push above is unconditional.
            None => Vec::new(),
        };
        result?;

        self.guard_growth(inner.len())?;
        varint::encode_u64(u64::from(field_id), &mut self.buf);
        varint::encode_u64(inner.len() as u64, &mut self.buf);
        self.buf.extend_from_slice(&inner);
        // Hand the buffer back for the next sibling field to reuse.
        self.pool.push(inner);
        Ok(())
    }

    /// Refuses a write that would push the payload past the frame limit.
    fn guard_growth(&self, additional: usize) -> Result<()> {
        let projected = self.len().saturating_add(additional);
        if projected > MAX_FRAME_BYTES {
            return Err(WireError::FrameTooLarge {
                len: projected,
                max: MAX_FRAME_BYTES,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_are_varint_encoded() {
        let mut w = Writer::new();
        w.write_u32(300);
        w.write_u64(1);
        w.write_bool(true);
        w.write_bool(false);
        assert_eq!(w.as_slice(), &[0xAC, 0x02, 0x01, 0x01, 0x00]);
    }

    #[test]
    fn ids_are_sixteen_raw_bytes() {
        let mut w = Writer::new();
        w.write_id(&Id::from_bytes([9u8; 16]));
        assert_eq!(w.as_slice(), &[9u8; 16]);
    }

    #[test]
    fn strings_carry_a_length_prefix() {
        let mut w = Writer::new();
        w.write_str("hi").expect("writes");
        assert_eq!(w.as_slice(), &[0x02, b'h', b'i']);
    }

    #[test]
    fn optional_fields_carry_id_and_length() {
        let mut w = Writer::new();
        w.optional(7, |w| {
            w.write_str("ab")?;
            Ok(())
        })
        .expect("writes");
        // field_id 7, byte_len 3, then the string's own prefix and bytes.
        assert_eq!(w.as_slice(), &[0x07, 0x03, 0x02, b'a', b'b']);
    }

    #[test]
    fn nested_optionals_nest_their_lengths() {
        let mut w = Writer::new();
        w.optional(1, |w| {
            w.write_u32(1);
            w.optional(2, |w| {
                w.write_u32(2);
                Ok(())
            })?;
            Ok(())
        })
        .expect("writes");
        // outer: id=1 len=4 [ 01, id=2, len=1, 02 ]
        assert_eq!(w.as_slice(), &[0x01, 0x04, 0x01, 0x02, 0x01, 0x02]);
    }

    #[test]
    fn optional_buffers_are_recycled_across_siblings() {
        let mut w = Writer::new();
        for id in 1..=8u32 {
            w.optional(id, |w| {
                w.write_u32(id);
                Ok(())
            })
            .expect("writes");
        }
        // Two buffers ever exist: the writer's own and one recycled scratch.
        assert_eq!(w.pool.len(), 1);
        assert_eq!(w.as_slice().len(), 8 * 3);
    }

    #[test]
    fn a_failing_optional_restores_the_outer_buffer() {
        let mut w = Writer::new();
        w.write_u32(1);
        let error = w
            .optional(1, |w| {
                w.write_u32(2);
                Err(WireError::InvalidUtf8)
            })
            .expect_err("propagates");
        assert_eq!(error, WireError::InvalidUtf8);
        // The partial field is discarded and the earlier byte survives.
        assert_eq!(w.as_slice(), &[0x01]);
        w.write_u32(3);
        assert_eq!(w.as_slice(), &[0x01, 0x03]);
    }

    #[test]
    fn oversized_strings_are_refused_before_allocation() {
        let mut w = Writer::new();
        let huge = "x".repeat(MAX_STRING_BYTES + 1);
        assert_eq!(
            w.write_str(&huge),
            Err(WireError::StringTooLong {
                len: MAX_STRING_BYTES + 1,
                max: MAX_STRING_BYTES
            })
        );
        assert!(w.is_empty(), "nothing must be written on refusal");
    }

    #[test]
    fn oversized_lists_are_refused() {
        let mut w = Writer::new();
        assert!(w.list_len(MAX_LIST_ITEMS + 1).is_err());
        assert!(w.list_len(MAX_LIST_ITEMS).is_ok());
    }

    #[test]
    fn nesting_depth_is_bounded() {
        let mut w = Writer::new();
        for _ in 0..MAX_NESTING_DEPTH {
            w.enter().expect("within limit");
        }
        assert_eq!(
            w.enter(),
            Err(WireError::DepthExceeded {
                max: MAX_NESTING_DEPTH
            })
        );
        w.leave();
        assert!(w.enter().is_ok());
    }

    #[test]
    fn frames_cannot_grow_past_the_limit() {
        let mut w = Writer::new();
        let chunk = vec![0u8; MAX_STRING_BYTES];
        let mut wrote = 0;
        loop {
            match w.write_bytes(&chunk) {
                Ok(()) => wrote += 1,
                Err(error) => {
                    assert!(
                        matches!(error, WireError::FrameTooLarge { .. }),
                        "{error:?}"
                    );
                    break;
                }
            }
            assert!(wrote < 100, "the guard never fired");
        }
        assert!(w.len() <= MAX_FRAME_BYTES);
    }

    #[test]
    fn timestamps_encode_as_epoch_millis() {
        let mut w = Writer::new();
        w.write_timestamp(Timestamp::from_millis(300));
        assert_eq!(w.as_slice(), &[0xAC, 0x02]);
    }
}
