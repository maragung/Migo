//! The sealed frame and the routing metadata that travels beside it.
//!
//! # Why the payload is a newtype and not a byte slice
//!
//! The one promise an SFU makes (section 165) is that it forwards encrypted
//! packets without access to plaintext media. A `Vec<u8>` parameter makes
//! that a convention every forwarding path has to keep; [`SealedFrame`] makes
//! it a property of the type: the bytes go in, the bytes come out, and there
//! is no method in between that hands them out by reference. Combined with
//! the crate linking no cryptography at all, there is nothing here that could
//! open a frame even by accident.
//!
//! # What the forwarder legitimately reads
//!
//! Identifiers and counters. Which call, which publisher, which stream, which
//! simulcast layer, which sequence number — the minimum a router needs to
//! decide where a frame goes. That is [`InboundFrame`], and every field of it
//! is a header in the honest sense of the word.

use migo_core::Id;

use crate::model::Member;

/// One simulcast layer of a published video stream.
///
/// Three, because that is what a simulcasting publisher encodes: the same
/// picture two or three times at different sizes, so the SFU's whole
/// stream-selection job is picking one of them per subscriber. Mapped onto
/// the brief's quality ladder (section 165), `High` is the 720p-and-above
/// rungs, `Medium` is 480p, `Low` is 360p. The ladder's fifth rung — audio
/// only — is not a layer but the absence of one, which is why there is no
/// `Audio` variant and a degraded subscription forwards no video at all
/// rather than an "audio layer".
///
/// The derived order is the quality order, `Low < Medium < High`, so the
/// minimum of a requested layer, an adaptive rung's layer, and a bandwidth
/// mode's ceiling is the layer that actually gets forwarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// The 360p rung.
    Low,
    /// The 480p rung.
    Medium,
    /// The 720p-and-above rungs.
    High,
}

impl Layer {
    /// How many layers exist: the simulcast offer ceiling a video publish
    /// may declare.
    pub const VARIANTS: usize = 3;
}

/// A sealed media frame, opaque by construction.
///
/// The bytes enter with [`SealedFrame::from_bytes`] or
/// [`SealedFrame::from_slice`] and leave only with
/// [`SealedFrame::into_bytes`], which consumes the frame because a transport
/// somewhere has to write the bytes and that place is the transport, not the
/// forwarder. There is deliberately no `as_bytes`, no `Deref`, and no
/// iteration: a method that lends the bytes out is a method a future
/// "helpful" change could call, and the type exists so that change does not
/// compile. Length is exposed because a transport pacing its socket needs a
/// size, and a size says nothing about content.
///
/// The crate links no cryptography, so nothing in this crate could open the
/// frame even if it wanted to. A call key that reaches this type is a key the
/// caller kept: the SFU holds routing, participants hold keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedFrame(Box<[u8]>);

impl SealedFrame {
    /// Wraps bytes this crate will never interpret.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes.into_boxed_slice())
    }

    /// Copies a slice into a frame, for callers whose transport hands them
    /// borrowed buffers. Same opacity as [`SealedFrame::from_bytes`].
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self(bytes.to_vec().into_boxed_slice())
    }

    /// How many bytes are being carried.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when the frame carries no bytes at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Hands the bytes to the writer that will put them on a socket,
    /// consuming the frame. The only way out.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0.into_vec()
    }
}

/// One frame arriving from a publisher, with the routing header it needs.
///
/// Every field except the payload is something the forwarder must read to
/// route; the payload is the one thing it must not, and — being a
/// [`SealedFrame`] — cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundFrame {
    /// Which of the publisher's streams this frame belongs to.
    pub stream_id: Id,
    /// The publisher's own numbering, per stream, passed through untouched.
    /// A subscriber who sees a gap knows a frame was dropped — by the
    /// publisher, or by the frame-rate stride this plane applies, which drops
    /// by this number's arithmetic and tells the subscriber which parity to
    /// expect.
    pub sequence: u64,
    /// Which simulcast layer of the stream this frame carries. Audio streams
    /// carry [`Layer::Low`]: voice is not simulcast, and the single value
    /// keeps one rule for every stream.
    pub layer: Layer,
    /// The sealed bytes, carried unopened.
    pub payload: SealedFrame,
}

/// One frame bound for one subscriber.
///
/// The transport writes `frame` to `to`; the identities ride along so the
/// receiving client can route the frame into the right decryptor without
/// trusting the sealed bytes to name their own sender.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// The seated participant the frame is bound for.
    pub to: Member,
    /// The seated participant whose stream the frame came from.
    pub publisher: Member,
    /// Which of the publisher's streams.
    pub stream_id: Id,
    /// The publisher's own sequence number, passed through.
    pub sequence: u64,
    /// Which simulcast layer was forwarded.
    pub layer: Layer,
    /// The sealed bytes, still unopened.
    pub frame: SealedFrame,
}
