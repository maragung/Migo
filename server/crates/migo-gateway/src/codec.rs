//! Turning a protocol message into the bytes of one frame — in one place, so the compression
//! toggle and the `ERROR` flag are decided the same way on every send path.
//!
//! Both the connection driver (for handshake replies) and the [`ClientContext`](crate::dispatch::ClientContext)
//! a dispatcher is handed encode through here. The gateway's own knob is honoured: when
//! compression is off the frame is built with [`Frame::new`] and the policy is never consulted;
//! when it is on, [`Frame::compressing`] applies brief section 155's size-and-gain rule.

use bytes::Bytes;

use migo_core::Error as CoreError;
use migo_protocol::{fault, to_bytes, Encode, Error as ErrorMessage, Frame, FrameHeader};

/// Encodes a protocol message as one frame's bytes for the given opcode and correlation.
///
/// # Errors
///
/// Propagates a `DECODE_FAILED`/`FRAME_TOO_LARGE` [`CoreError`] if the value cannot be
/// serialized or the frame would exceed [`MAX_FRAME_BYTES`](migo_wire::frame). A message the
/// server itself built failing to encode is an internal fault, not the client's.
pub(crate) fn encode_message<T: Encode>(
    opcode: u32,
    correlation: u32,
    message: &T,
    compression: bool,
) -> Result<Bytes, CoreError> {
    let payload = to_bytes(message).map_err(fault::from_wire)?;
    frame_bytes(FrameHeader::new(opcode, correlation), payload, compression)
}

/// Encodes a [`CoreError`] as one frame's bytes, with the `ERROR` flag set (section 140).
///
/// `opcode` is the request's opcode for a correlated reply, or [`Opcode::Error`](migo_protocol::Opcode::Error)
/// for a standalone server error; `correlation` matches the request, or `0` when the error is
/// server-initiated (section 139). Only the *public* face of the error crosses the wire —
/// the internal message stays in the log (section 161).
///
/// # Errors
///
/// As [`encode_message`]: an internal fault if the tiny error struct cannot be encoded.
pub(crate) fn encode_error(
    opcode: u32,
    correlation: u32,
    error: &CoreError,
    compression: bool,
) -> Result<Bytes, CoreError> {
    let payload = to_bytes(&wire_error(error)).map_err(fault::from_wire)?;
    frame_bytes(
        FrameHeader::new(opcode, correlation).error(),
        payload,
        compression,
    )
}

/// Projects an internal [`CoreError`] onto the wire [`ErrorMessage`], disclosing only what a
/// client may see.
fn wire_error(error: &CoreError) -> ErrorMessage {
    let public = error.public_message();
    ErrorMessage {
        code: error.code(),
        symbol: error.symbol().to_string(),
        message: if public.is_empty() {
            None
        } else {
            Some(public.to_string())
        },
        retry_after_ms: error.retry_after(),
        field: None,
    }
}

/// Builds and encodes a frame, applying the compression policy only when compression is on.
fn frame_bytes(header: FrameHeader, payload: Bytes, compression: bool) -> Result<Bytes, CoreError> {
    let frame = if compression {
        Frame::compressing(header, payload)
    } else {
        Frame::new(header, payload)
    };
    frame.encode().map_err(fault::from_wire)
}
