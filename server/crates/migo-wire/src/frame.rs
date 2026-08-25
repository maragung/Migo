//! MWP/1 frame header.
//!
//! Every message on every transport is one frame:
//!
//! ```text
//! u8      version          protocol version, 1
//! u8      flags            see crate::flags
//! varint  opcode           what this frame is
//! varint  correlation      request/response pairing, 0 for unsolicited
//! [16]u8  trace_id         only when TRACED
//! [8]u8   span_id          only when TRACED
//! varint  fragment_index   only when FRAGMENT
//! varint  fragment_total   only when FRAGMENT
//! ...     payload          the remainder of the frame
//! ```
//!
//! Note what is *not* here: a length. The payload runs to the end of the frame,
//! and the transport supplies the boundary. WebSocket and QUIC datagrams already
//! frame messages, so a length field would be redundant bytes on every single
//! message. Stream transports that do not frame (raw TCP, QUIC streams) prepend
//! a `u32` big-endian length — see [`Frame::encode_length_prefixed`] — which keeps the
//! cost where it is actually needed instead of taxing the common case.
//!
//! Header fields are ordered by how often they are present: version and flags
//! always, opcode and correlation always, then the optional blocks. A parser
//! therefore reads forward once and never seeks.

use bytes::{BufMut, Bytes, BytesMut};

use crate::error::{Result, WireError};
use crate::flags;
use crate::limits::MAX_FRAME_BYTES;
use crate::reader::Reader;
use crate::varint;

/// The wire protocol version this build speaks.
///
/// Bumping this is a breaking change and is not how features are added: new
/// optional fields and new opcodes are backward compatible by construction (see
/// the crate docs on MSE). This changes only if the *framing* changes.
pub const PROTOCOL_VERSION: u8 = 1;

/// Distributed-tracing identifiers carried on a frame.
///
/// W3C Trace Context shapes: a 16-byte trace id and an 8-byte span id, sent as
/// raw bytes rather than the 55-character `traceparent` string. Sampling happens
/// at the edge, so most frames omit this entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceContext {
    /// Identifies the whole trace.
    pub trace_id: [u8; 16],
    /// Identifies the sending span within the trace.
    pub span_id: [u8; 8],
}

impl TraceContext {
    /// Encoded size of a trace context in a frame header.
    pub const ENCODED_LEN: usize = 24;

    /// Returns true when both identifiers are all zero, which W3C Trace Context
    /// defines as invalid. Such a context is dropped rather than propagated.
    #[must_use]
    pub fn is_invalid(&self) -> bool {
        self.trace_id == [0u8; 16] && self.span_id == [0u8; 8]
    }
}

/// Position of a frame within a fragmented message.
///
/// Used only for payloads that cannot fit [`MAX_FRAME_BYTES`] — in practice
/// large media metadata and history backfills. Chat messages never fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    /// Zero-based index of this fragment.
    pub index: u32,
    /// Total number of fragments in the message.
    pub total: u32,
}

impl Fragment {
    /// Returns true when this is the last fragment of its message.
    #[must_use]
    pub fn is_last(&self) -> bool {
        self.index + 1 >= self.total
    }

    /// Validates the pair. A total of zero, or an index at or past the total, is
    /// a protocol error rather than something to reassemble optimistically.
    fn validate(&self) -> Result<()> {
        if self.total == 0 || self.index >= self.total {
            return Err(WireError::InvalidFragment {
                index: self.index,
                total: self.total,
            });
        }
        Ok(())
    }
}

/// The parsed header of an MWP/1 frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Protocol version byte as it appeared on the wire.
    pub version: u8,
    /// Raw flag bits. Use the helpers rather than testing bits inline.
    pub flags: u8,
    /// The operation this frame carries.
    pub opcode: u32,
    /// Correlates a response with its request; 0 for server-initiated frames.
    pub correlation: u32,
    /// Present when the `TRACED` flag is set.
    pub trace: Option<TraceContext>,
    /// Present when the `FRAGMENT` flag is set.
    pub fragment: Option<Fragment>,
}

impl FrameHeader {
    /// A minimal header: current version, no flags, no trace, no fragment.
    #[must_use]
    pub fn new(opcode: u32, correlation: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            flags: 0,
            opcode,
            correlation,
            trace: None,
            fragment: None,
        }
    }

    /// Marks the payload as raw-DEFLATE compressed.
    #[must_use]
    pub fn compressed(mut self) -> Self {
        self.flags |= flags::COMPRESSED;
        self
    }

    /// Marks the frame as carrying a protocol error payload.
    #[must_use]
    pub fn error(mut self) -> Self {
        self.flags |= flags::ERROR;
        self
    }

    /// Requests an application-level acknowledgement.
    #[must_use]
    pub fn ack_required(mut self) -> Self {
        self.flags |= flags::ACK_REQUIRED;
        self
    }

    /// Attaches a trace context, ignoring the all-zero (invalid) one.
    #[must_use]
    pub fn with_trace(mut self, trace: TraceContext) -> Self {
        if trace.is_invalid() {
            return self;
        }
        self.flags |= flags::TRACED;
        self.trace = Some(trace);
        self
    }

    /// Attaches fragment coordinates.
    #[must_use]
    pub fn with_fragment(mut self, fragment: Fragment) -> Self {
        self.flags |= flags::FRAGMENT;
        self.fragment = Some(fragment);
        self
    }

    /// True when the payload needs inflating before decoding.
    #[must_use]
    pub fn is_compressed(&self) -> bool {
        self.flags & flags::COMPRESSED != 0
    }

    /// True when the payload is a batch of sub-frames.
    #[must_use]
    pub fn is_batch(&self) -> bool {
        self.flags & flags::BATCH != 0
    }

    /// True when the payload is an error rather than the opcode's normal type.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.flags & flags::ERROR != 0
    }

    /// True when the sender wants an acknowledgement.
    #[must_use]
    pub fn is_ack_required(&self) -> bool {
        self.flags & flags::ACK_REQUIRED != 0
    }

    /// Encoded size of this header in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        let mut len = 2
            + varint::encoded_len(u64::from(self.opcode))
            + varint::encoded_len(u64::from(self.correlation));
        if self.trace.is_some() {
            len += TraceContext::ENCODED_LEN;
        }
        if let Some(fragment) = self.fragment {
            len += varint::encoded_len(u64::from(fragment.index))
                + varint::encoded_len(u64::from(fragment.total));
        }
        len
    }

    /// Appends this header to `out`.
    ///
    /// The flag bits and the optional blocks are written from the same source of
    /// truth — the `Option` fields — so a header can never claim `TRACED` without
    /// carrying a trace.
    pub fn encode(&self, out: &mut BytesMut) -> Result<()> {
        let mut flag_bits = self.flags;
        flag_bits &= !(flags::TRACED | flags::FRAGMENT);
        if self.trace.is_some() {
            flag_bits |= flags::TRACED;
        }
        if self.fragment.is_some() {
            flag_bits |= flags::FRAGMENT;
        }
        if flag_bits & flags::RESERVED_MASK != 0 {
            return Err(WireError::ReservedFlags {
                bits: flag_bits & flags::RESERVED_MASK,
            });
        }

        out.put_u8(self.version);
        out.put_u8(flag_bits);
        varint::encode_u64(u64::from(self.opcode), out);
        varint::encode_u64(u64::from(self.correlation), out);
        if let Some(trace) = &self.trace {
            out.put_slice(&trace.trace_id);
            out.put_slice(&trace.span_id);
        }
        if let Some(fragment) = self.fragment {
            fragment.validate()?;
            varint::encode_u64(u64::from(fragment.index), out);
            varint::encode_u64(u64::from(fragment.total), out);
        }
        Ok(())
    }

    /// Parses a header from the front of `input`, returning it and the offset at
    /// which the payload begins.
    ///
    /// Rejects, in this order: a short frame, an unsupported version, reserved
    /// flag bits, a truncated trace block, and an impossible fragment pair.
    /// Reserved bits are an error and not something to ignore — a peer that sets
    /// them is speaking a dialect we do not know, and silently discarding the
    /// bits would let a future extension be stripped by an old node.
    pub fn decode(input: &[u8]) -> Result<(Self, usize)> {
        if input.len() < 2 {
            return Err(WireError::UnexpectedEnd {
                offset: input.len(),
                needed: 2,
            });
        }
        let version = input[0];
        if version != PROTOCOL_VERSION {
            return Err(WireError::UnsupportedVersion {
                found: version,
                supported: PROTOCOL_VERSION,
            });
        }
        let flag_bits = input[1];
        if flag_bits & flags::RESERVED_MASK != 0 {
            return Err(WireError::ReservedFlags {
                bits: flag_bits & flags::RESERVED_MASK,
            });
        }

        let mut offset = 2;
        let (opcode, used) = varint::decode_u64(input, offset)?;
        offset += used;
        let (correlation, used) = varint::decode_u64(input, offset)?;
        offset += used;

        let opcode =
            u32::try_from(opcode).map_err(|_| WireError::FieldOverflow { field: "opcode" })?;
        let correlation = u32::try_from(correlation).map_err(|_| WireError::FieldOverflow {
            field: "correlation",
        })?;

        let trace = if flag_bits & flags::TRACED != 0 {
            let end = offset + TraceContext::ENCODED_LEN;
            if input.len() < end {
                return Err(WireError::UnexpectedEnd {
                    offset,
                    needed: TraceContext::ENCODED_LEN,
                });
            }
            let mut trace_id = [0u8; 16];
            trace_id.copy_from_slice(&input[offset..offset + 16]);
            let mut span_id = [0u8; 8];
            span_id.copy_from_slice(&input[offset + 16..end]);
            offset = end;
            Some(TraceContext { trace_id, span_id })
        } else {
            None
        };

        let fragment = if flag_bits & flags::FRAGMENT != 0 {
            let (index, used) = varint::decode_u64(input, offset)?;
            offset += used;
            let (total, used) = varint::decode_u64(input, offset)?;
            offset += used;
            let fragment = Fragment {
                index: u32::try_from(index).map_err(|_| WireError::FieldOverflow {
                    field: "fragment_index",
                })?,
                total: u32::try_from(total).map_err(|_| WireError::FieldOverflow {
                    field: "fragment_total",
                })?,
            };
            fragment.validate()?;
            Some(fragment)
        } else {
            None
        };

        let header = Self {
            version,
            flags: flag_bits,
            opcode,
            correlation,
            trace,
            fragment,
        };
        Ok((header, offset))
    }
}

/// A complete frame: header plus payload bytes.
///
/// The payload is [`Bytes`], so slicing it out of a received buffer and handing
/// the same payload to a thousand room subscribers are both refcount bumps. That
/// property is the reason fanout is cheap, and it is why nothing in this crate
/// takes `Vec<u8>` on the read path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Parsed header.
    pub header: FrameHeader,
    /// Payload bytes exactly as they appeared, still compressed if the
    /// `COMPRESSED` flag is set.
    pub payload: Bytes,
}

impl Frame {
    /// Builds a frame from a header and payload.
    #[must_use]
    pub fn new(header: FrameHeader, payload: Bytes) -> Self {
        Self { header, payload }
    }

    /// Builds an uncompressed frame with no trace and no fragmentation.
    #[must_use]
    pub fn simple(opcode: u32, correlation: u32, payload: Bytes) -> Self {
        Self::new(FrameHeader::new(opcode, correlation), payload)
    }

    /// Builds a frame, compressing the payload when the policy says it pays.
    ///
    /// This is the constructor the gateway uses on the send path: the decision is
    /// made once, here, rather than being re-litigated at each call site.
    #[must_use]
    pub fn compressing(mut header: FrameHeader, payload: Bytes) -> Self {
        match crate::compress::maybe_deflate(&payload) {
            Some(compressed) => {
                header.flags |= flags::COMPRESSED;
                Self::new(header, Bytes::from(compressed))
            }
            None => {
                header.flags &= !flags::COMPRESSED;
                Self::new(header, payload)
            }
        }
    }

    /// Total encoded size in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.header.encoded_len() + self.payload.len()
    }

    /// Encodes the frame for a transport that supplies its own message
    /// boundaries (WebSocket, QUIC datagram).
    pub fn encode(&self) -> Result<Bytes> {
        let len = self.encoded_len();
        if len > MAX_FRAME_BYTES {
            return Err(WireError::FrameTooLarge {
                len,
                max: MAX_FRAME_BYTES,
            });
        }
        let mut out = BytesMut::with_capacity(len);
        self.header.encode(&mut out)?;
        out.put_slice(&self.payload);
        Ok(out.freeze())
    }

    /// Encodes the frame with a `u32` big-endian length prefix, for stream
    /// transports that do not frame messages themselves.
    pub fn encode_length_prefixed(&self) -> Result<Bytes> {
        let body = self.encode()?;
        let mut out = BytesMut::with_capacity(4 + body.len());
        out.put_u32(
            u32::try_from(body.len()).map_err(|_| WireError::FrameTooLarge {
                len: body.len(),
                max: MAX_FRAME_BYTES,
            })?,
        );
        out.put_slice(&body);
        Ok(out.freeze())
    }

    /// Parses one frame from a complete transport message.
    ///
    /// The size limit is checked before anything is parsed, because the cheapest
    /// place to reject an oversized frame is before touching it.
    pub fn decode(input: Bytes) -> Result<Self> {
        if input.len() > MAX_FRAME_BYTES {
            return Err(WireError::FrameTooLarge {
                len: input.len(),
                max: MAX_FRAME_BYTES,
            });
        }
        let (header, offset) = FrameHeader::decode(&input)?;
        let payload = input.slice(offset..);
        Ok(Self { header, payload })
    }

    /// Parses one length-prefixed frame, returning it and the number of bytes
    /// consumed so a stream reader can advance.
    ///
    /// Returns `Ok(None)` when the buffer does not yet hold a whole frame, which
    /// is the normal state of a stream transport rather than an error.
    pub fn decode_length_prefixed(input: &Bytes) -> Result<Option<(Self, usize)>> {
        if input.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([input[0], input[1], input[2], input[3]]) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(WireError::FrameTooLarge {
                len,
                max: MAX_FRAME_BYTES,
            });
        }
        if input.len() < 4 + len {
            return Ok(None);
        }
        let frame = Self::decode(input.slice(4..4 + len))?;
        Ok(Some((frame, 4 + len)))
    }

    /// Returns the payload, inflating it when the `COMPRESSED` flag is set.
    pub fn payload_inflated(&self) -> Result<Bytes> {
        if self.header.is_compressed() {
            crate::compress::inflate_raw(&self.payload, MAX_FRAME_BYTES)
        } else {
            Ok(self.payload.clone())
        }
    }

    /// Returns a reader over the inflated payload, ready for [`crate::Decode`].
    pub fn payload_reader(&self) -> Result<Reader> {
        Ok(Reader::new(self.payload_inflated()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace() -> TraceContext {
        TraceContext {
            trace_id: [0xAB; 16],
            span_id: [0xCD; 8],
        }
    }

    #[test]
    fn a_minimal_header_is_four_bytes() {
        let header = FrameHeader::new(1, 0);
        let mut out = BytesMut::new();
        header.encode(&mut out).expect("encodes");
        assert_eq!(&out[..], &[PROTOCOL_VERSION, 0, 1, 0]);
        assert_eq!(header.encoded_len(), 4);
    }

    #[test]
    fn round_trips_without_options() {
        let frame = Frame::simple(0x21, 7, Bytes::from_static(b"payload"));
        let encoded = frame.encode().expect("encodes");
        let decoded = Frame::decode(encoded).expect("decodes");
        assert_eq!(decoded.header.opcode, 0x21);
        assert_eq!(decoded.header.correlation, 7);
        assert_eq!(&decoded.payload[..], b"payload");
        assert!(decoded.header.trace.is_none());
        assert!(decoded.header.fragment.is_none());
    }

    #[test]
    fn round_trips_with_every_option() {
        let header = FrameHeader::new(300, 65_536)
            .with_trace(trace())
            .with_fragment(Fragment { index: 2, total: 5 })
            .ack_required()
            .error();
        let frame = Frame::new(header, Bytes::from_static(b"x"));
        let encoded = frame.encode().expect("encodes");
        assert_eq!(encoded.len(), frame.encoded_len());
        let decoded = Frame::decode(encoded).expect("decodes");
        assert_eq!(decoded.header.opcode, 300);
        assert_eq!(decoded.header.correlation, 65_536);
        assert_eq!(decoded.header.trace, Some(trace()));
        assert_eq!(
            decoded.header.fragment,
            Some(Fragment { index: 2, total: 5 })
        );
        assert!(decoded.header.is_ack_required());
        assert!(decoded.header.is_error());
    }

    #[test]
    fn an_invalid_trace_context_is_dropped_rather_than_sent() {
        let header = FrameHeader::new(1, 0).with_trace(TraceContext {
            trace_id: [0; 16],
            span_id: [0; 8],
        });
        assert!(header.trace.is_none());
        assert_eq!(header.flags & flags::TRACED, 0);
    }

    #[test]
    fn reserved_flag_bits_are_rejected_not_ignored() {
        let mut encoded = Frame::simple(1, 0, Bytes::new())
            .encode()
            .expect("encodes")
            .to_vec();
        encoded[1] |= flags::FLAGS_EXT;
        assert_eq!(
            Frame::decode(Bytes::from(encoded)),
            Err(WireError::ReservedFlags {
                bits: flags::FLAGS_EXT
            })
        );
    }

    #[test]
    fn an_unknown_version_is_rejected() {
        let mut encoded = Frame::simple(1, 0, Bytes::new())
            .encode()
            .expect("encodes")
            .to_vec();
        encoded[0] = 99;
        assert_eq!(
            Frame::decode(Bytes::from(encoded)),
            Err(WireError::UnsupportedVersion {
                found: 99,
                supported: PROTOCOL_VERSION
            })
        );
    }

    #[test]
    fn a_truncated_trace_block_is_rejected() {
        let header = FrameHeader::new(1, 0).with_trace(trace());
        let frame = Frame::new(header, Bytes::new());
        let encoded = frame.encode().expect("encodes");
        // Cut the trace block in half.
        let truncated = encoded.slice(..encoded.len() - 12);
        assert!(matches!(
            Frame::decode(truncated),
            Err(WireError::UnexpectedEnd { .. })
        ));
    }

    #[test]
    fn an_impossible_fragment_is_rejected() {
        let mut out = BytesMut::new();
        out.put_u8(PROTOCOL_VERSION);
        out.put_u8(flags::FRAGMENT);
        varint::encode_u64(1, &mut out);
        varint::encode_u64(0, &mut out);
        varint::encode_u64(5, &mut out); // index
        varint::encode_u64(5, &mut out); // total: index == total is invalid
        assert_eq!(
            Frame::decode(out.freeze()),
            Err(WireError::InvalidFragment { index: 5, total: 5 })
        );
    }

    #[test]
    fn fragment_ordering_is_reported() {
        assert!(!Fragment { index: 0, total: 3 }.is_last());
        assert!(Fragment { index: 2, total: 3 }.is_last());
    }

    #[test]
    fn an_oversized_frame_is_rejected_before_parsing() {
        let huge = Bytes::from(vec![0u8; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            Frame::decode(huge),
            Err(WireError::FrameTooLarge {
                len: MAX_FRAME_BYTES + 1,
                max: MAX_FRAME_BYTES
            })
        );
    }

    #[test]
    fn an_oversized_payload_cannot_be_encoded() {
        let frame = Frame::simple(1, 0, Bytes::from(vec![0u8; MAX_FRAME_BYTES]));
        assert!(matches!(
            frame.encode(),
            Err(WireError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn the_compression_policy_sets_the_flag_only_when_it_compressed() {
        let redundant = Bytes::from(vec![b'a'; 4096]);
        let frame = Frame::compressing(FrameHeader::new(1, 0), redundant.clone());
        assert!(frame.header.is_compressed());
        assert!(frame.payload.len() < redundant.len());
        assert_eq!(frame.payload_inflated().expect("inflates"), redundant);

        let tiny = Bytes::from_static(b"typing");
        let frame = Frame::compressing(FrameHeader::new(1, 0), tiny.clone());
        assert!(!frame.header.is_compressed());
        assert_eq!(frame.payload_inflated().expect("passthrough"), tiny);
    }

    #[test]
    fn compression_survives_a_round_trip_through_the_wire() {
        let payload = Bytes::from("selamat pagi ".repeat(400).into_bytes());
        let frame = Frame::compressing(FrameHeader::new(0x21, 3), payload.clone());
        let decoded = Frame::decode(frame.encode().expect("encodes")).expect("decodes");
        assert!(decoded.header.is_compressed());
        assert_eq!(decoded.payload_inflated().expect("inflates"), payload);
    }

    #[test]
    fn length_prefixed_framing_round_trips() {
        let frame = Frame::simple(9, 1, Bytes::from_static(b"stream"));
        let encoded = frame.encode_length_prefixed().expect("encodes");
        let (decoded, used) = Frame::decode_length_prefixed(&encoded)
            .expect("decodes")
            .expect("complete");
        assert_eq!(used, encoded.len());
        assert_eq!(decoded, frame);
    }

    #[test]
    fn a_partial_length_prefixed_frame_is_not_an_error() {
        let frame = Frame::simple(9, 1, Bytes::from_static(b"stream"));
        let encoded = frame.encode_length_prefixed().expect("encodes");
        for cut in 0..encoded.len() {
            assert_eq!(
                Frame::decode_length_prefixed(&encoded.slice(..cut)),
                Ok(None),
                "a {cut}-byte prefix must be treated as incomplete"
            );
        }
    }

    #[test]
    fn a_hostile_length_prefix_is_rejected_without_buffering() {
        // Claims 4 GiB. Must fail immediately rather than wait for the bytes.
        let hostile = Bytes::from_static(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(matches!(
            Frame::decode_length_prefixed(&hostile),
            Err(WireError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn two_frames_in_one_buffer_are_read_in_sequence() {
        let first = Frame::simple(1, 1, Bytes::from_static(b"one"));
        let second = Frame::simple(2, 2, Bytes::from_static(b"two"));
        let mut buf = BytesMut::new();
        buf.put_slice(&first.encode_length_prefixed().expect("encodes"));
        buf.put_slice(&second.encode_length_prefixed().expect("encodes"));
        let mut buf = buf.freeze();

        let (a, used) = Frame::decode_length_prefixed(&buf)
            .expect("decodes")
            .expect("complete");
        buf = buf.slice(used..);
        let (b, used) = Frame::decode_length_prefixed(&buf)
            .expect("decodes")
            .expect("complete");
        buf = buf.slice(used..);
        assert_eq!(a, first);
        assert_eq!(b, second);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_payload_is_a_refcount_slice_of_the_received_buffer() {
        let encoded = Frame::simple(1, 0, Bytes::from_static(b"shared"))
            .encode()
            .expect("encodes");
        let decoded = Frame::decode(encoded.clone()).expect("decodes");
        // Same allocation, different window: this is what makes fanout cheap.
        assert_eq!(
            decoded.payload.as_ptr(),
            encoded[encoded.len() - 6..].as_ptr()
        );
    }
}
