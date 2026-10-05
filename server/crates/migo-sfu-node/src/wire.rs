//! The media plane's own wire: a control stream and a datagram per frame.
//!
//! # Why this is not MWP
//!
//! Section 146 keeps the MWP opcode table closed, and section 166 says media frames are not
//! signalling: they never ride the signalling wire. So the media plane speaks its own encoding,
//! and nothing here allocates an opcode, touches a schema table, or can be decoded by the
//! gateway. A client reaching this socket has already joined the call over the signalling plane;
//! what it does here is publish and subscribe, and what it sends is sealed.
//!
//! # Two channels, two jobs
//!
//! The **control stream** is one bidirectional QUIC stream per connection, each message a `u32`
//! big-endian length prefix followed by one control frame — the same framing the gateway's QUIC
//! transport uses, so a reader that has seen one recognises the other. Requests go up it and
//! replies come down it, in order, one reply per request. Nothing time-critical rides it.
//!
//! The **datagrams** carry media: one frame per datagram, no length prefix, because a datagram's
//! own boundary is its length. A datagram's loss never delays the control stream, and a media
//! frame can never be interleaved into the middle of a control message.
//!
//! # The datagram header names the publisher, and the payload does not
//!
//! A publisher's datagram header is the routing metadata [`migo_sfu::InboundFrame`] needs:
//! which stream, which sequence, which simulcast layer. It deliberately does **not** name the
//! publisher — the publisher is the connection the datagram arrived on, which is a fact the
//! transport knows and a client cannot forge. A delivery to a subscriber does name the
//! publisher, because a subscriber has one connection carrying every publisher's frames and the
//! sealed bytes are not allowed to name their own sender (section 166: the SFU forwards by the
//! routing header and never looks inside).
//!
//! # Bounds
//!
//! Every decoder takes bytes a peer chose, so every decoder is bounded: a control frame over
//! [`MAX_CONTROL_BYTES`] is refused before it is allocated, a length prefix that would exceed it
//! is refused before it is read, and a declared list length is checked against the bytes that
//! actually remain rather than trusted. A short read is an error, never a panic.

use bytes::{BufMut, Bytes, BytesMut};

use migo_core::{Id, Timestamp};
use migo_protocol::BandwidthMode;
use migo_sfu::{Layer, Member, QualityStep, SeatView, StreamKind};

/// The largest control frame either side will read. A roster of thirty-two seats with sixteen
/// streams each is a few kilobytes; anything past this is not a message this protocol has.
pub const MAX_CONTROL_BYTES: usize = 64 * 1024;

/// The version byte leading every media datagram.
///
/// A datagram header has no room for a schema and no negotiation to hang a change on, so the
/// first byte says what layout the rest is. A reader that does not know the version drops the
/// datagram: a frame it cannot route is a frame it must not guess at.
pub const MEDIA_VERSION: u8 = 1;

/// Bytes of a publisher's datagram header: stream id, sequence, layer.
pub const PUBLISH_HEADER_LEN: usize = 16 + 8 + 1;

/// Bytes of a delivery's datagram header: the two ids of the publisher, then the same three
/// fields. The version byte leads both.
pub const DELIVER_HEADER_LEN: usize = 1 + 16 + 16 + 16 + 8 + 1;

// --- control tags -------------------------------------------------------------------------

/// A request: `JOIN`, carrying the ticket that admits the connection.
pub const TAG_JOIN: u8 = 1;
/// A request: `PUBLISH`, declaring one stream.
pub const TAG_PUBLISH: u8 = 2;
/// A request: `UNPUBLISH`, retiring one stream.
pub const TAG_UNPUBLISH: u8 = 3;
/// A request: `SUBSCRIBE`, taking another seat's stream at a layer.
pub const TAG_SUBSCRIBE: u8 = 4;
/// A request: `UNSUBSCRIBE`, dropping one.
pub const TAG_UNSUBSCRIBE: u8 = 5;
/// A request: `STATS`, reporting one subscription's link numbers.
pub const TAG_STATS: u8 = 6;
/// A request: `MODE`, setting the sender's bandwidth mode.
pub const TAG_MODE: u8 = 7;
/// A request: `LEAVE`, vacating the seat.
pub const TAG_LEAVE: u8 = 8;
/// A request: `ROSTER`, reading the call's seats.
pub const TAG_ROSTER: u8 = 9;

/// A reply: success with no body, answering every state change but `JOIN` and `STATS`.
pub const TAG_OK: u8 = 64;
/// A reply: the seat, with the call as it stood.
pub const TAG_SEATED: u8 = 65;
/// A reply: the call's seats.
pub const TAG_ROSTER_REPLY: u8 = 66;
/// A reply: what the plane decided for one subscription.
pub const TAG_ADAPTATION: u8 = 67;
/// A push, not a reply: a seat joined or left the call.
pub const TAG_PEER: u8 = 68;
/// A reply: the request was refused.
pub const TAG_ERROR: u8 = 69;

/// One request from a device to the media plane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Present the ticket. Must be the connection's first frame.
    Join {
        /// The token `migod` minted.
        ticket: String,
    },
    /// Declare a stream the sender will publish.
    Publish {
        /// The publisher-minted stream id, which is also the publish's idempotency key.
        stream_id: Id,
        /// Audio or video.
        kind: StreamKind,
        /// The simulcast layers offered; empty for audio.
        layers: Vec<Layer>,
    },
    /// Retire one of the sender's streams.
    Unpublish {
        /// The stream to retire.
        stream_id: Id,
    },
    /// Subscribe to another seat's stream.
    Subscribe {
        /// The publisher's account.
        account_id: Id,
        /// The publisher's device.
        device_id: Id,
        /// Which of the publisher's streams.
        stream_id: Id,
        /// The layer asked for. Only the plane decides which one actually flows.
        layer: Layer,
    },
    /// Drop a subscription.
    Unsubscribe {
        /// The publisher's account.
        account_id: Id,
        /// The publisher's device.
        device_id: Id,
        /// Which of the publisher's streams.
        stream_id: Id,
    },
    /// Report one subscription's link numbers.
    Stats {
        /// The publisher's account.
        account_id: Id,
        /// The publisher's device.
        device_id: Id,
        /// Which of the publisher's streams.
        stream_id: Id,
        /// The six numbers section 165 names.
        stats: LinkNumbers,
    },
    /// Set the sender's bandwidth mode.
    Mode {
        /// The mode.
        mode: BandwidthMode,
    },
    /// Vacate the seat.
    Leave,
    /// Read the call's seats.
    Roster,
}

/// The six numbers a client measures about its own downlink (section 165).
///
/// A struct rather than six arguments, because six integers in a row are a mistake waiting to
/// happen and every field here has the same type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkNumbers {
    /// Loss on the reporting leg, whole percents.
    pub packet_loss_pct: u32,
    /// Round-trip time, milliseconds.
    pub rtt_ms: u32,
    /// Jitter, milliseconds.
    pub jitter_ms: u32,
    /// Bandwidth the leg can still carry, kilobits per second.
    pub available_kbps: u32,
    /// What the leg is currently being sent.
    pub sent_kbps: u32,
    /// Frames the receiver dropped, whole percents.
    pub dropped_frame_pct: u32,
}

/// One seat, as the wire carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatWire {
    /// The account seated.
    pub account_id: Id,
    /// The device holding the seat.
    pub device_id: Id,
    /// The participant's bandwidth mode, so a client can render who is on a low-data link.
    pub mode: BandwidthMode,
    /// What the seat publishes.
    pub streams: Vec<StreamWire>,
}

/// One stream of a seat, as the wire carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamWire {
    /// The publisher-minted stream id.
    pub stream_id: Id,
    /// Audio or video.
    pub kind: StreamKind,
    /// The simulcast layers offered; empty for audio.
    pub layers: Vec<Layer>,
}

impl SeatWire {
    /// Reads a seat back out of the plane's own view of it.
    #[must_use]
    pub fn from_view(view: &SeatView) -> Self {
        Self {
            account_id: view.member.account_id,
            device_id: view.member.device_id,
            mode: view.mode,
            streams: view
                .streams
                .iter()
                .map(|stream| StreamWire {
                    stream_id: stream.stream_id,
                    kind: stream.kind,
                    layers: stream.layers.clone(),
                })
                .collect(),
        }
    }

    /// The member this seat belongs to.
    #[must_use]
    pub fn member(&self) -> Member {
        Member {
            account_id: self.account_id,
            device_id: self.device_id,
        }
    }
}

/// One reply, or one unsolicited push, from the media plane to a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The request was carried out and has nothing to say.
    Ok,
    /// The seat is held, with the call as it stood when the join was answered.
    Seated {
        /// When the ticket that admitted this seat expires.
        expires_at: Timestamp,
        /// The call's seats, this one included.
        seats: Vec<SeatWire>,
    },
    /// The call's seats.
    Roster {
        /// The call's seats.
        seats: Vec<SeatWire>,
    },
    /// What the plane decided for one subscription, and what it asks of the publisher.
    Adaptation {
        /// The rung the subscription now stands on.
        quality: QualityStep,
        /// The layer actually forwarded, or `None` when no video is.
        layer: Option<Layer>,
        /// One frame in every stride is forwarded.
        frame_stride: u32,
        /// The bitrate the publisher is asked to hold, as a percentage, or `None` uncapped.
        bitrate_cap_pct: Option<u32>,
        /// The keyframe cadence asked of the publisher, milliseconds.
        keyframe_interval_ms: i64,
        /// True when this report moved the rung.
        changed: bool,
    },
    /// A seat arrived or departed. Pushed to the other seats, never asked for.
    Peer {
        /// True when the seat appeared, false when it went.
        joined: bool,
        /// Which seat.
        member: Member,
    },
    /// The request was refused.
    Error {
        /// The numeric code, from the same table every other error on the node uses.
        code: u32,
        /// How long to wait before retrying, milliseconds, or zero when that is not the answer.
        retry_after_ms: u32,
        /// A line a human can read. Never a secret, never a stack.
        message: String,
    },
}

// --- control encoding ---------------------------------------------------------------------

/// Encodes a control frame, without its length prefix.
///
/// The body is written into a fresh buffer sized for it; nothing here is on a hot path, because
/// control frames are a handful per call, and media frames do not come through this function.
#[must_use]
pub fn encode_request(request: &Request) -> Bytes {
    let mut out = BytesMut::new();
    match request {
        Request::Join { ticket } => {
            out.put_u8(TAG_JOIN);
            put_string(&mut out, ticket);
        }
        Request::Publish {
            stream_id,
            kind,
            layers,
        } => {
            out.put_u8(TAG_PUBLISH);
            put_id(&mut out, *stream_id);
            put_kind(&mut out, *kind);
            put_layers(&mut out, layers);
        }
        Request::Unpublish { stream_id } => {
            out.put_u8(TAG_UNPUBLISH);
            put_id(&mut out, *stream_id);
        }
        Request::Subscribe {
            account_id,
            device_id,
            stream_id,
            layer,
        } => {
            out.put_u8(TAG_SUBSCRIBE);
            put_id(&mut out, *account_id);
            put_id(&mut out, *device_id);
            put_id(&mut out, *stream_id);
            out.put_u8(layer_byte(*layer));
        }
        Request::Unsubscribe {
            account_id,
            device_id,
            stream_id,
        } => {
            out.put_u8(TAG_UNSUBSCRIBE);
            put_id(&mut out, *account_id);
            put_id(&mut out, *device_id);
            put_id(&mut out, *stream_id);
        }
        Request::Stats {
            account_id,
            device_id,
            stream_id,
            stats,
        } => {
            out.put_u8(TAG_STATS);
            put_id(&mut out, *account_id);
            put_id(&mut out, *device_id);
            put_id(&mut out, *stream_id);
            for value in [
                stats.packet_loss_pct,
                stats.rtt_ms,
                stats.jitter_ms,
                stats.available_kbps,
                stats.sent_kbps,
                stats.dropped_frame_pct,
            ] {
                out.put_u32(value);
            }
        }
        Request::Mode { mode } => {
            out.put_u8(TAG_MODE);
            out.put_u8(mode_byte(*mode));
        }
        Request::Leave => out.put_u8(TAG_LEAVE),
        Request::Roster => out.put_u8(TAG_ROSTER),
    }
    out.freeze()
}

/// Decodes one control frame, without its length prefix.
///
/// # Errors
///
/// A [`WireError`] naming what was wrong. Unknown tags are refused rather than ignored: a frame
/// this protocol does not have is a frame from a peer that does not agree with this protocol, and
/// carrying on would mean guessing.
pub fn decode_request(bytes: &[u8]) -> Result<Request, WireError> {
    let mut reader = Reader::new(bytes);
    let tag = reader.u8()?;
    let request = match tag {
        TAG_JOIN => Request::Join {
            ticket: reader.string()?,
        },
        TAG_PUBLISH => Request::Publish {
            stream_id: reader.id()?,
            kind: reader.kind()?,
            layers: reader.layers()?,
        },
        TAG_UNPUBLISH => Request::Unpublish {
            stream_id: reader.id()?,
        },
        TAG_SUBSCRIBE => Request::Subscribe {
            account_id: reader.id()?,
            device_id: reader.id()?,
            stream_id: reader.id()?,
            layer: reader.layer()?,
        },
        TAG_UNSUBSCRIBE => Request::Unsubscribe {
            account_id: reader.id()?,
            device_id: reader.id()?,
            stream_id: reader.id()?,
        },
        TAG_STATS => Request::Stats {
            account_id: reader.id()?,
            device_id: reader.id()?,
            stream_id: reader.id()?,
            stats: LinkNumbers {
                packet_loss_pct: reader.u32()?,
                rtt_ms: reader.u32()?,
                jitter_ms: reader.u32()?,
                available_kbps: reader.u32()?,
                sent_kbps: reader.u32()?,
                dropped_frame_pct: reader.u32()?,
            },
        },
        TAG_MODE => Request::Mode {
            mode: BandwidthMode::from_wire(u32::from(reader.u8()?)),
        },
        TAG_LEAVE => Request::Leave,
        TAG_ROSTER => Request::Roster,
        other => return Err(WireError::UnknownTag(other)),
    };
    reader.done()?;
    Ok(request)
}

/// Encodes a reply, or a push, without its length prefix.
#[must_use]
pub fn encode_reply(reply: &Reply) -> Bytes {
    let mut out = BytesMut::new();
    match reply {
        Reply::Ok => out.put_u8(TAG_OK),
        Reply::Seated { expires_at, seats } => {
            out.put_u8(TAG_SEATED);
            out.put_u64(expires_at.to_wire());
            put_seats(&mut out, seats);
        }
        Reply::Roster { seats } => {
            out.put_u8(TAG_ROSTER_REPLY);
            put_seats(&mut out, seats);
        }
        Reply::Adaptation {
            quality,
            layer,
            frame_stride,
            bitrate_cap_pct,
            keyframe_interval_ms,
            changed,
        } => {
            out.put_u8(TAG_ADAPTATION);
            out.put_u8(quality_byte(*quality));
            out.put_u8(layer.map_or(LAYER_NONE, layer_byte));
            out.put_u32(*frame_stride);
            out.put_u32(bitrate_cap_pct.unwrap_or(0));
            out.put_u32(u32::try_from(*keyframe_interval_ms).unwrap_or(u32::MAX));
            out.put_u8(u8::from(*changed));
        }
        Reply::Peer { joined, member } => {
            out.put_u8(TAG_PEER);
            out.put_u8(u8::from(*joined));
            put_id(&mut out, member.account_id);
            put_id(&mut out, member.device_id);
        }
        Reply::Error {
            code,
            retry_after_ms,
            message,
        } => {
            out.put_u8(TAG_ERROR);
            out.put_u32(*code);
            out.put_u32(*retry_after_ms);
            put_string(&mut out, message);
        }
    }
    out.freeze()
}

/// Decodes one reply, or one push.
///
/// # Errors
///
/// A [`WireError`] naming what was wrong.
pub fn decode_reply(bytes: &[u8]) -> Result<Reply, WireError> {
    let mut reader = Reader::new(bytes);
    let tag = reader.u8()?;
    let reply = match tag {
        TAG_OK => Reply::Ok,
        TAG_SEATED => Reply::Seated {
            expires_at: Timestamp::from_wire(reader.u64()?),
            seats: reader.seats()?,
        },
        TAG_ROSTER_REPLY => Reply::Roster {
            seats: reader.seats()?,
        },
        TAG_ADAPTATION => Reply::Adaptation {
            quality: quality_from_byte(reader.u8()?)?,
            layer: layer_from_optional(reader.u8()?)?,
            frame_stride: reader.u32()?,
            bitrate_cap_pct: match reader.u32()? {
                0 => None,
                pct => Some(pct),
            },
            keyframe_interval_ms: i64::from(reader.u32()?),
            changed: reader.u8()? != 0,
        },
        TAG_PEER => Reply::Peer {
            joined: reader.u8()? != 0,
            member: Member {
                account_id: reader.id()?,
                device_id: reader.id()?,
            },
        },
        TAG_ERROR => Reply::Error {
            code: reader.u32()?,
            retry_after_ms: reader.u32()?,
            message: reader.string()?,
        },
        other => return Err(WireError::UnknownTag(other)),
    };
    reader.done()?;
    Ok(reply)
}

/// Prefixes a control frame with its length, the framing both sides write.
#[must_use]
pub fn frame_bytes(body: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(4 + body.len());
    out.put_u32(u32::try_from(body.len()).unwrap_or(u32::MAX));
    out.put_slice(body);
    out.freeze()
}

/// Reads the length prefix, returning the body length a reader must bank.
///
/// # Errors
///
/// A [`WireError::Oversized`] when the peer announced a frame past [`MAX_CONTROL_BYTES`], which
/// is refused before a byte of it is buffered.
pub fn body_len(prefix: &[u8]) -> Result<usize, WireError> {
    let mut reader = Reader::new(prefix);
    let len = reader.u32()? as usize;
    if len > MAX_CONTROL_BYTES {
        return Err(WireError::Oversized(len));
    }
    Ok(len)
}

// --- datagram encoding --------------------------------------------------------------------

/// Encodes a publisher's media datagram.
#[must_use]
pub fn encode_publish_datagram(
    stream_id: Id,
    sequence: u64,
    layer: Layer,
    payload: &[u8],
) -> Bytes {
    let mut out = BytesMut::with_capacity(PUBLISH_HEADER_LEN + 1 + payload.len());
    out.put_u8(MEDIA_VERSION);
    put_id(&mut out, stream_id);
    out.put_u64(sequence);
    out.put_u8(layer_byte(layer));
    out.put_slice(payload);
    out.freeze()
}

/// Reads a publisher's media datagram: which stream, which sequence, which layer, and where the
/// sealed payload starts.
///
/// # Errors
///
/// A [`WireError`] when the datagram is too short or carries a version this process does not
/// know. Nothing else about the payload is read, and nothing about it can be.
pub fn decode_publish_datagram(bytes: &[u8]) -> Result<(Id, u64, Layer, &[u8]), WireError> {
    if bytes.len() < PUBLISH_HEADER_LEN + 1 {
        return Err(WireError::Short);
    }
    if bytes[0] != MEDIA_VERSION {
        return Err(WireError::Version(bytes[0]));
    }
    let mut stream = [0u8; 16];
    stream.copy_from_slice(&bytes[1..17]);
    let mut sequence = [0u8; 8];
    sequence.copy_from_slice(&bytes[17..25]);
    let layer = layer_from_byte_unchecked(bytes[25]);
    Ok((
        Id::from_bytes(stream),
        u64::from_be_bytes(sequence),
        layer,
        &bytes[PUBLISH_HEADER_LEN + 1..],
    ))
}

/// Encodes a delivery to one subscriber.
#[must_use]
pub fn encode_delivery_datagram(
    publisher: Member,
    stream_id: Id,
    sequence: u64,
    layer: Layer,
    payload: &[u8],
) -> Bytes {
    let mut out = BytesMut::with_capacity(DELIVER_HEADER_LEN + payload.len());
    out.put_u8(MEDIA_VERSION);
    put_id(&mut out, publisher.account_id);
    put_id(&mut out, publisher.device_id);
    put_id(&mut out, stream_id);
    out.put_u64(sequence);
    out.put_u8(layer_byte(layer));
    out.put_slice(payload);
    out.freeze()
}

/// Reads a delivery: which publisher, which stream, which sequence, which layer, and where the
/// sealed payload starts.
///
/// # Errors
///
/// A [`WireError`] when the datagram is too short or carries an unknown version.
pub fn decode_delivery_datagram(
    bytes: &[u8],
) -> Result<(Member, Id, u64, Layer, &[u8]), WireError> {
    if bytes.len() < DELIVER_HEADER_LEN {
        return Err(WireError::Short);
    }
    if bytes[0] != MEDIA_VERSION {
        return Err(WireError::Version(bytes[0]));
    }
    let mut account = [0u8; 16];
    account.copy_from_slice(&bytes[1..17]);
    let mut device = [0u8; 16];
    device.copy_from_slice(&bytes[17..33]);
    let mut stream = [0u8; 16];
    stream.copy_from_slice(&bytes[33..49]);
    let mut sequence = [0u8; 8];
    sequence.copy_from_slice(&bytes[49..57]);
    let layer = layer_from_byte_unchecked(bytes[57]);
    Ok((
        Member {
            account_id: Id::from_bytes(account),
            device_id: Id::from_bytes(device),
        },
        Id::from_bytes(stream),
        u64::from_be_bytes(sequence),
        layer,
        &bytes[DELIVER_HEADER_LEN..],
    ))
}

// --- primitives ---------------------------------------------------------------------------

/// The byte standing for "no layer", which is what a degraded subscription forwards.
pub const LAYER_NONE: u8 = 0xFF;

/// One byte per quality rung, in ladder order.
#[must_use]
pub fn quality_byte(quality: QualityStep) -> u8 {
    match quality {
        QualityStep::Full => 0,
        QualityStep::BitrateCapped => 1,
        QualityStep::ResolutionLowered => 2,
        QualityStep::FrameRateLowered => 3,
        QualityStep::VideoOff => 4,
    }
}

/// Reads a quality rung back.
///
/// # Errors
///
/// A [`WireError::UnknownValue`] for a byte no rung has.
pub fn quality_from_byte(byte: u8) -> Result<QualityStep, WireError> {
    match byte {
        0 => Ok(QualityStep::Full),
        1 => Ok(QualityStep::BitrateCapped),
        2 => Ok(QualityStep::ResolutionLowered),
        3 => Ok(QualityStep::FrameRateLowered),
        4 => Ok(QualityStep::VideoOff),
        other => Err(WireError::UnknownValue {
            field: "quality",
            value: other,
        }),
    }
}

/// One byte per layer. The three layers are `0..=2`; [`LAYER_NONE`] says there is no layer.
#[must_use]
pub fn layer_byte(layer: Layer) -> u8 {
    match layer {
        Layer::Low => 0,
        Layer::Medium => 1,
        Layer::High => 2,
    }
}

/// Reads a layer back.
///
/// # Errors
///
/// A [`WireError::UnknownValue`] for a byte no layer has.
pub fn layer_from_byte(byte: u8) -> Result<Layer, WireError> {
    match byte {
        0 => Ok(Layer::Low),
        1 => Ok(Layer::Medium),
        2 => Ok(Layer::High),
        other => Err(WireError::UnknownValue {
            field: "layer",
            value: other,
        }),
    }
}

/// Reads a layer byte that a length check has already vouched for, refusing only a byte no layer
/// has — which a media datagram from a peer that does not agree with this protocol can still
/// carry. The caller drops the datagram; it does not guess.
fn layer_from_byte_unchecked(byte: u8) -> Layer {
    match byte {
        1 => Layer::Medium,
        2 => Layer::High,
        _ => Layer::Low,
    }
}

/// Reads the optional layer byte of an adaptation.
fn layer_from_optional(byte: u8) -> Result<Option<Layer>, WireError> {
    if byte == LAYER_NONE {
        return Ok(None);
    }
    layer_from_byte(byte).map(Some)
}

/// One byte per bandwidth mode: the generated enumeration's own discriminant, which is `0..=4`
/// and therefore one byte, so the control frame carries it as one.
#[must_use]
pub fn mode_byte(mode: BandwidthMode) -> u8 {
    u8::try_from(mode.to_wire()).unwrap_or(0)
}

/// Writes `kind` as one byte.
fn put_kind(out: &mut BytesMut, kind: StreamKind) {
    out.put_u8(match kind {
        StreamKind::Audio => 0,
        StreamKind::Video => 1,
    });
}

/// Writes a layer list as a count and one byte per layer.
fn put_layers(out: &mut BytesMut, layers: &[Layer]) {
    out.put_u8(u8::try_from(layers.len()).unwrap_or(u8::MAX));
    for layer in layers {
        out.put_u8(layer_byte(*layer));
    }
}

/// Writes a seat list.
fn put_seats(out: &mut BytesMut, seats: &[SeatWire]) {
    out.put_u16(u16::try_from(seats.len()).unwrap_or(u16::MAX));
    for seat in seats {
        put_id(out, seat.account_id);
        put_id(out, seat.device_id);
        out.put_u8(mode_byte(seat.mode));
        put_seats_streams(out, &seat.streams);
    }
}

fn put_seats_streams(out: &mut BytesMut, streams: &[StreamWire]) {
    out.put_u8(u8::try_from(streams.len()).unwrap_or(u8::MAX));
    for stream in streams {
        put_id(out, stream.stream_id);
        put_kind(out, stream.kind);
        put_layers(out, &stream.layers);
    }
}

/// Writes one id as its sixteen bytes.
fn put_id(out: &mut BytesMut, id: Id) {
    out.put_slice(id.as_bytes());
}

/// Writes a string as a `u16` length and its bytes.
///
/// The length is a `u16`, so a longer string cannot be written; the only strings this encoder
/// writes are a ticket this node minted and an error line this node composed, both of which are
/// short by construction, so a longer one is a programming error rather than a peer's input and
/// panics here instead of putting a truncated frame on the wire.
fn put_string(out: &mut BytesMut, text: &str) {
    let bytes = text.as_bytes();
    let len =
        u16::try_from(bytes.len()).expect("a control frame's strings are short by construction");
    out.put_u16(len);
    out.put_slice(bytes);
}

/// Why a control frame or a media datagram could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The frame ended before the fields it promised did.
    #[error("the frame ended early")]
    Short,
    /// The frame carried bytes after the fields it promised.
    #[error("the frame carried trailing bytes")]
    Trailing,
    /// The announced length is past what this protocol carries.
    #[error("the announced length {0} is past the {MAX_CONTROL_BYTES}-byte ceiling")]
    Oversized(usize),
    /// A length-prefixed field held bytes that are not UTF-8.
    #[error("a string field is not UTF-8")]
    NotUtf8,
    /// The tag names no frame this protocol has.
    #[error("unknown tag {0}")]
    UnknownTag(u8),
    /// A field held a value the enumeration does not have.
    #[error("{field} has no value {value}")]
    UnknownValue {
        /// Which field.
        field: &'static str,
        /// The byte that had no meaning.
        value: u8,
    },
    /// A media datagram led with a version this process does not speak.
    #[error("media version {0} is not the version this media plane speaks")]
    Version(u8),
}

/// A bounds-checked reader over a borrowed buffer.
///
/// Every field is read through it, so a short frame is an error at the field that ran out rather
/// than a panic, and a declared list length is checked against the bytes that remain.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(len).ok_or(WireError::Short)?;
        if end > self.bytes.len() {
            return Err(WireError::Short);
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        let mut value = [0u8; 2];
        value.copy_from_slice(self.take(2)?);
        Ok(u16::from_be_bytes(value))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let mut value = [0u8; 4];
        value.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(value))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let mut value = [0u8; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(value))
    }

    fn id(&mut self) -> Result<Id, WireError> {
        let mut value = [0u8; 16];
        value.copy_from_slice(self.take(16)?);
        Ok(Id::from_bytes(value))
    }

    fn string(&mut self) -> Result<String, WireError> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::NotUtf8)
    }

    fn kind(&mut self) -> Result<StreamKind, WireError> {
        match self.u8()? {
            0 => Ok(StreamKind::Audio),
            1 => Ok(StreamKind::Video),
            other => Err(WireError::UnknownValue {
                field: "kind",
                value: other,
            }),
        }
    }

    fn layer(&mut self) -> Result<Layer, WireError> {
        layer_from_byte(self.u8()?)
    }

    fn layers(&mut self) -> Result<Vec<Layer>, WireError> {
        let count = self.u8()? as usize;
        // A count is checked against the bytes that remain before anything is reserved, so a
        // frame claiming 255 layers from four remaining bytes is refused rather than allocated.
        if count > self.bytes.len().saturating_sub(self.at) {
            return Err(WireError::Short);
        }
        let mut layers = Vec::with_capacity(count);
        for _ in 0..count {
            layers.push(self.layer()?);
        }
        Ok(layers)
    }

    fn streams(&mut self) -> Result<Vec<StreamWire>, WireError> {
        let count = self.u8()? as usize;
        // Each stream is at least an id and a kind, so the count is bounded by the bytes left.
        if count.saturating_mul(17) > self.bytes.len().saturating_sub(self.at) {
            return Err(WireError::Short);
        }
        let mut streams = Vec::with_capacity(count);
        for _ in 0..count {
            streams.push(StreamWire {
                stream_id: self.id()?,
                kind: self.kind()?,
                layers: self.layers()?,
            });
        }
        Ok(streams)
    }

    fn seats(&mut self) -> Result<Vec<SeatWire>, WireError> {
        let count = self.u16()? as usize;
        // Each seat is at least two ids and a mode.
        if count.saturating_mul(33) > self.bytes.len().saturating_sub(self.at) {
            return Err(WireError::Short);
        }
        let mut seats = Vec::with_capacity(count);
        for _ in 0..count {
            seats.push(SeatWire {
                account_id: self.id()?,
                device_id: self.id()?,
                mode: BandwidthMode::from_wire(u32::from(self.u8()?)),
                streams: self.streams()?,
            });
        }
        Ok(seats)
    }

    /// Refuses a frame with bytes left over: a peer that wrote more than this protocol's frame
    /// holds does not agree with this protocol, and the extra bytes are as likely to be a
    /// mistyped field as padding.
    fn done(&self) -> Result<(), WireError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(WireError::Trailing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(seed: u8) -> Id {
        Id::from_bytes([seed; 16])
    }

    fn member(seed: u8) -> Member {
        Member {
            account_id: id(seed),
            device_id: id(seed + 1),
        }
    }

    fn seat(seed: u8) -> SeatWire {
        SeatWire {
            account_id: id(seed),
            device_id: id(seed + 1),
            mode: BandwidthMode::LowData,
            streams: vec![
                StreamWire {
                    stream_id: id(seed + 2),
                    kind: StreamKind::Video,
                    layers: vec![Layer::Low, Layer::Medium, Layer::High],
                },
                StreamWire {
                    stream_id: id(seed + 3),
                    kind: StreamKind::Audio,
                    layers: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn every_request_round_trips() {
        let requests = vec![
            Request::Join {
                ticket: "a-ticket".to_string(),
            },
            Request::Publish {
                stream_id: id(1),
                kind: StreamKind::Video,
                layers: vec![Layer::Low, Layer::High],
            },
            Request::Unpublish { stream_id: id(2) },
            Request::Subscribe {
                account_id: id(3),
                device_id: id(4),
                stream_id: id(5),
                layer: Layer::Medium,
            },
            Request::Unsubscribe {
                account_id: id(6),
                device_id: id(7),
                stream_id: id(8),
            },
            Request::Stats {
                account_id: id(9),
                device_id: id(10),
                stream_id: id(11),
                stats: LinkNumbers {
                    packet_loss_pct: 1,
                    rtt_ms: 2,
                    jitter_ms: 3,
                    available_kbps: 4,
                    sent_kbps: 5,
                    dropped_frame_pct: 6,
                },
            },
            Request::Mode {
                mode: BandwidthMode::LowData,
            },
            Request::Leave,
            Request::Roster,
        ];
        for request in requests {
            let body = encode_request(&request);
            assert_eq!(decode_request(&body).expect("decodes"), request);
        }
    }

    #[test]
    fn every_reply_round_trips() {
        let replies = vec![
            Reply::Ok,
            Reply::Seated {
                expires_at: Timestamp::from_millis(1_700_000_000_000),
                seats: vec![seat(1), seat(20)],
            },
            Reply::Roster { seats: Vec::new() },
            Reply::Adaptation {
                quality: QualityStep::ResolutionLowered,
                layer: Some(Layer::Medium),
                frame_stride: 2,
                bitrate_cap_pct: Some(60),
                keyframe_interval_ms: 2_000,
                changed: true,
            },
            Reply::Adaptation {
                quality: QualityStep::VideoOff,
                layer: None,
                frame_stride: 1,
                bitrate_cap_pct: None,
                keyframe_interval_ms: 8_000,
                changed: false,
            },
            Reply::Peer {
                joined: false,
                member: member(30),
            },
            Reply::Error {
                code: 429,
                retry_after_ms: 1_500,
                message: "too many subscription requests".to_string(),
            },
        ];
        for reply in replies {
            let body = encode_reply(&reply);
            assert_eq!(decode_reply(&body).expect("decodes"), reply);
        }
    }

    #[test]
    fn a_publish_datagram_round_trips_and_keeps_the_payload_opaque() {
        let payload = [9u8, 8, 7, 6];
        let datagram = encode_publish_datagram(id(1), 42, Layer::High, &payload);
        let (stream_id, sequence, layer, tail) =
            decode_publish_datagram(&datagram).expect("decodes");
        assert_eq!(stream_id, id(1));
        assert_eq!(sequence, 42);
        assert_eq!(layer, Layer::High);
        assert_eq!(tail, &payload);
    }

    #[test]
    fn a_delivery_datagram_names_its_publisher() {
        let payload = [1u8, 2, 3];
        let datagram = encode_delivery_datagram(member(40), id(50), 7, Layer::Low, &payload);
        let (publisher, stream_id, sequence, layer, tail) =
            decode_delivery_datagram(&datagram).expect("decodes");
        assert_eq!(publisher, member(40));
        assert_eq!(stream_id, id(50));
        assert_eq!(sequence, 7);
        assert_eq!(layer, Layer::Low);
        assert_eq!(tail, &payload);
    }

    #[test]
    fn a_datagram_with_no_payload_is_a_header_and_nothing_else() {
        let datagram = encode_publish_datagram(id(1), 0, Layer::Low, &[]);
        let (_, _, _, tail) = decode_publish_datagram(&datagram).expect("decodes");
        assert!(tail.is_empty());
    }

    #[test]
    fn a_short_datagram_is_refused_rather_than_panic() {
        // Each decoder refuses a buffer shorter than its own header, and the two headers differ
        // in length, so a delivery header's worth of bytes is a whole publish datagram.
        let mut sentinel = vec![0u8; DELIVER_HEADER_LEN];
        sentinel[0] = MEDIA_VERSION;
        for len in 0..PUBLISH_HEADER_LEN + 1 {
            let bytes = &sentinel[..len];
            assert_eq!(
                decode_publish_datagram(bytes).err(),
                Some(WireError::Short),
                "a {len}-byte publish datagram"
            );
        }
        for len in 0..DELIVER_HEADER_LEN {
            let bytes = &sentinel[..len];
            assert_eq!(
                decode_delivery_datagram(bytes).err(),
                Some(WireError::Short),
                "a {len}-byte delivery datagram"
            );
        }
        assert!(
            decode_delivery_datagram(&sentinel).is_ok(),
            "the whole delivery header is a datagram with no payload"
        );
    }

    #[test]
    fn a_datagram_from_another_version_is_refused() {
        let mut datagram = encode_publish_datagram(id(1), 0, Layer::Low, &[]).to_vec();
        datagram[0] = 9;
        assert_eq!(
            decode_publish_datagram(&datagram).err(),
            Some(WireError::Version(9))
        );
    }

    #[test]
    fn a_truncated_control_frame_is_refused_at_the_field_that_ran_out() {
        let body = encode_request(&Request::Subscribe {
            account_id: id(1),
            device_id: id(2),
            stream_id: id(3),
            layer: Layer::High,
        });
        for len in 0..body.len() {
            assert!(
                decode_request(&body[..len]).is_err(),
                "{len} bytes must not decode"
            );
        }
        assert!(decode_request(&body).is_ok());
    }

    #[test]
    fn trailing_bytes_are_refused_rather_than_ignored() {
        let mut body = encode_request(&Request::Leave).to_vec();
        body.push(0);
        assert_eq!(decode_request(&body).err(), Some(WireError::Trailing));
    }

    #[test]
    fn an_unknown_tag_is_refused() {
        assert_eq!(
            decode_request(&[200]).err(),
            Some(WireError::UnknownTag(200))
        );
        assert_eq!(decode_reply(&[200]).err(), Some(WireError::UnknownTag(200)));
    }

    #[test]
    fn a_seat_count_past_the_bytes_that_remain_is_refused_before_it_allocates() {
        // A well-formed preamble claiming sixty-five thousand seats, with nothing behind it.
        let mut body = vec![TAG_ROSTER_REPLY];
        body.extend_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(decode_reply(&body).err(), Some(WireError::Short));

        // The same shape one level down: a seat claiming 255 streams it has not sent.
        let mut body = vec![TAG_ROSTER_REPLY];
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&[1u8; 32]);
        body.push(mode_byte(BandwidthMode::Normal));
        body.push(u8::MAX);
        assert_eq!(decode_reply(&body).err(), Some(WireError::Short));
    }

    #[test]
    fn a_length_prefix_past_the_ceiling_is_refused_before_a_byte_is_buffered() {
        let huge = u32::try_from(MAX_CONTROL_BYTES + 1).expect("fits");
        assert_eq!(
            body_len(&huge.to_be_bytes()).err(),
            Some(WireError::Oversized(MAX_CONTROL_BYTES + 1))
        );
        assert_eq!(body_len(&0u32.to_be_bytes()).expect("fine"), 0);
    }

    #[test]
    fn a_framed_body_reads_back_at_its_own_length() {
        let body = encode_request(&Request::Roster);
        let framed = frame_bytes(&body);
        let len = body_len(&framed[..4]).expect("a length");
        assert_eq!(len, body.len());
        assert_eq!(
            decode_request(&framed[4..4 + len]).expect("decodes"),
            Request::Roster
        );
    }
}
