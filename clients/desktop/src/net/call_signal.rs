//! The call-signalling contract: the seal, the call key's channel, and the words
//! a call is allowed to say — a port of `clients/web/src/lib/migo/call-signal.ts`,
//! mirrored by Android's `com.migo.core.domain.CallSignal`.
//!
//! # Two planes, one shared id
//!
//! A call has a signalling plane and a media plane, and they meet in exactly one
//! place: the call id. Everything on the signalling plane — the offer, the answer,
//! the ICE batches — is an opaque blob the server relays without reading; the
//! media plane is WebRTC, end-to-end by construction. The seal here is what makes
//! the first sentence true: a blob sealed for one call cannot be replayed into
//! another, because the call id rides in the AEAD's associated data.
//!
//! # The seal is a cross-client byte contract
//!
//! A blob sealed by the web build must open here and the other way round, so the
//! envelope is fixed the way the web suite pins it (`clients/web/test/calls.test.tsx`):
//! version byte 2, then the house AEAD output (`nonce || ciphertext || tag`), with
//! associated data `migo-call-signal:{callId}` — the id's Crockford base32 text,
//! which is exactly what [`Id`]'s `Display` writes. The legacy version-1 envelope
//! (a version byte, a 32-byte key slot, a 12-byte nonce slot, then plaintext — the
//! pre-key design, which was never encrypted) is *opened* so history stays
//! readable, and never written.
//!
//! # The JSON shapes are contracts too
//!
//! An SDP blob is `{"type":"offer","sdp":"…"}` and an ICE batch is a JSON array of
//! candidate inits with camelCase field names — the shape every browser's
//! `RTCIceCandidate.toJSON()` produces and every libwebrtc client reads. The
//! `webrtc` crate's own `RTCIceCandidateInit` serialises `sdp_mid` in snake_case,
//! so ICE travels through [`IceCandidateJson`], this port of the Android mirror,
//! whose field spellings are the wire's own.

use std::fmt;
use std::time::Duration;

use migo_core::{Id, OsRandom, Random, Timestamp, ID_BYTE_LEN};
use migo_crypto::aead::{self, SymmetricKey};
use serde::{Deserialize, Serialize};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

/// The control-event name a call key rides under, inside the E2EE group layer.
///
/// The web and Android clients send and read this same string, so it is a
/// cross-client constant, not a local naming choice.
pub const CALL_KEY_EVENT: &str = "call-key";

/// The version byte this build writes on every sealed signal.
const SEALED_VERSION: u8 = 2;

/// The legacy envelope's fixed prefix: version byte, 32-byte key slot, 12-byte
/// nonce slot. Everything after it is plaintext, because that design sealed
/// nothing — which is why it was replaced.
const LEGACY_PREFIX: usize = 1 + 32 + 12;

/// The call key's length in bytes. A house-AEAD key: 32 bytes.
pub const CALL_KEY_LEN: usize = 32;

/// The note a callee sees when a ring they never answered expired.
pub const MISSED_CALL_MESSAGE: &str = "Missed call";

/// The note a device sees when a sibling device answered the call it was ringing.
pub const ANSWERED_ELSEWHERE_MESSAGE: &str = "Answered on another device";

/// Something sealed or shaped for the call plane could not be read.
///
/// A refusal rather than a guess: an envelope this build cannot parse is either
/// corruption or a future version, and answering either with best-effort bytes
/// would hand a broken call a plausible-looking SDP instead of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSignalFormatError(String);

impl fmt::Display for CallSignalFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CallSignalFormatError {}

fn refused(what: impl Into<String>) -> CallSignalFormatError {
    CallSignalFormatError(what.into())
}

/// The AEAD domain for one call: `migo-call-signal:{callId}`, in the id's own
/// text form. Byte-identical across clients because [`Id`]'s `Display` is the
/// Crockford base32 text everywhere.
fn domain(call_id: &Id) -> Vec<u8> {
    format!("migo-call-signal:{call_id}").into_bytes()
}

/// Seals one signalling payload for `call_id` under `key`.
///
/// The output is the version-2 envelope: `2 || nonce || ciphertext || tag`. A
/// fresh random nonce every seal, so two seals of one payload differ.
pub fn seal_call_signal(
    payload: &[u8],
    key: &[u8; CALL_KEY_LEN],
    call_id: &Id,
) -> Result<Vec<u8>, CallSignalFormatError> {
    let key = SymmetricKey::from_bytes(*key);
    let sealed = aead::seal(&key, &domain(call_id), payload, &mut OsRandom)
        .map_err(|error| refused(format!("could not seal a call signal: {error}")))?;
    let mut out = Vec::with_capacity(1 + sealed.len());
    out.push(SEALED_VERSION);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Opens one sealed signal for `call_id` under `key`.
///
/// Version 2 must authenticate under exactly this key and exactly this call —
/// a blob from another call, or a flipped byte anywhere in it, is a refusal.
/// The legacy version 1 opens under any key, because it was never encrypted;
/// it is read for history's sake and never written.
pub fn open_call_signal(
    sealed: &[u8],
    key: &[u8; CALL_KEY_LEN],
    call_id: &Id,
) -> Result<Vec<u8>, CallSignalFormatError> {
    match sealed.first() {
        None => Err(refused("an empty envelope is not a call signal")),
        Some(&2) => {
            let key = SymmetricKey::from_bytes(*key);
            aead::open(&key, &domain(call_id), &sealed[1..])
                .map_err(|_| refused("this call signal does not open for this call"))
        }
        Some(&1) => {
            if sealed.len() < LEGACY_PREFIX {
                return Err(refused(
                    "a legacy envelope shorter than its own framing slots",
                ));
            }
            Ok(sealed[LEGACY_PREFIX..].to_vec())
        }
        Some(&other) => Err(refused(format!(
            "an unknown call-signal envelope version {other}"
        ))),
    }
}

/// Mints a fresh call key: 32 bytes from the OS generator.
///
/// The key seals every signal of one call and nothing else — no ratchet, no
/// rotation, because a call is minutes long and the key dies with it.
pub fn generate_call_key() -> [u8; CALL_KEY_LEN] {
    let mut key = [0u8; CALL_KEY_LEN];
    OsRandom.fill_bytes(&mut key);
    key
}

/// Encodes the call-key control event: 16 id bytes then 32 key bytes.
///
/// This rides the E2EE group message layer before the invite, so the callee can
/// unseal the offer the moment it rings — the key must win the race against the
/// invite, which is why the caller sends it first.
pub fn encode_call_key_event(call_id: &Id, key: &[u8; CALL_KEY_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ID_BYTE_LEN + CALL_KEY_LEN);
    out.extend_from_slice(call_id.as_bytes());
    out.extend_from_slice(key);
    out
}

/// Decodes a call-key control event, or `None` when the bytes are not one.
///
/// `None` rather than an error because the input is an untrusted control event:
/// a wrong width is dropped, not diagnosed — there is nothing a user could do
/// with the diagnosis.
pub fn decode_call_key_event(data: &[u8]) -> Option<(Id, [u8; CALL_KEY_LEN])> {
    if data.len() != ID_BYTE_LEN + CALL_KEY_LEN {
        return None;
    }
    let (id_bytes, key_bytes) = data.split_at(ID_BYTE_LEN);
    let mut key = [0u8; CALL_KEY_LEN];
    key.copy_from_slice(key_bytes);
    Some((Id::from_bytes(id_bytes.try_into().ok()?), key))
}

/// Encodes an SDP description as the sealed payload's JSON: `{"type":…,"sdp":…}`.
///
/// The `webrtc` crate's own serde derives are the contract here — its test suite
/// pins `{"type":"offer","sdp":"sdp"}` byte for byte — so no local shape is
/// interposed.
pub fn encode_sdp_description(
    description: &RTCSessionDescription,
) -> Result<Vec<u8>, CallSignalFormatError> {
    serde_json::to_vec(description).map_err(|_| refused("could not encode an SDP description"))
}

/// Decodes an SDP description from its sealed-payload JSON.
///
/// The `parsed` field is skipped by serde and left empty; the peer connection
/// parses the SDP text itself when the description is applied, so the caller
/// hands the decoded value straight to `set_local_description` /
/// `set_remote_description`.
pub fn decode_sdp_description(
    bytes: &[u8],
) -> Result<RTCSessionDescription, CallSignalFormatError> {
    serde_json::from_slice(bytes).map_err(|_| refused("this payload is not an SDP description"))
}

/// One ICE candidate as the wire spells it — the Android mirror of the browser's
/// `RTCIceCandidate.toJSON()`, field for field.
///
/// Every field but the line index is optional: an end-of-gathering notification
/// is an entry with neither candidate nor mid, the sentinel the web build sends
/// so its peer knows gathering finished. Nulls are omitted rather than written,
/// and the line index is always written (defaulting to 0), matching Android's
/// `explicitNulls = false, encodeDefaults = true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceCandidateJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    #[serde(rename = "sdpMid", default, skip_serializing_if = "Option::is_none")]
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex", default)]
    pub sdp_mline_index: u16,
    #[serde(
        rename = "usernameFragment",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub username_fragment: Option<String>,
}

/// Encodes an ICE batch: a JSON array of candidate inits.
///
/// A batch, not one frame per candidate, because candidates arrive in bursts and
/// the relay is charged per frame — the web client batches with a short linger
/// for the same reason.
pub fn encode_ice_batch(batch: &[IceCandidateJson]) -> Result<Vec<u8>, CallSignalFormatError> {
    serde_json::to_vec(batch).map_err(|_| refused("could not encode an ICE batch"))
}

/// Decodes an ICE batch from its sealed-payload JSON.
///
/// A payload that is not an array — an SDP object where a batch belongs, say —
/// is a refusal, not an empty batch.
pub fn decode_ice_batch(bytes: &[u8]) -> Result<Vec<IceCandidateJson>, CallSignalFormatError> {
    serde_json::from_slice(bytes).map_err(|_| refused("this payload is not an ICE batch"))
}

/// A call's state, as the wire numbers it.
///
/// `from_wire` refuses the unknown rather than guessing: a state this build has
/// no name for is a newer server's vocabulary, and inventing a display for it
/// would be a guess wearing a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Ringing,
    Connecting,
    Connected,
    Reconnecting,
    Ended,
}

impl CallState {
    /// The known states, by their wire numbers. `None` for anything else.
    #[must_use]
    pub const fn from_wire(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Ringing,
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Reconnecting,
            4 => Self::Ended,
            _ => return None,
        })
    }
}

/// Why a call ended, as the wire numbers it.
///
/// `Busy` is a *decline* reason that surfaces as an end reason: the callee's
/// devices were occupied, nobody refused, and a retry is welcome. The caller's
/// screen must not read it as a hang-up or as a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEndReason {
    ByCaller,
    ByCallee,
    Declined,
    NoAnswer,
    Failed,
    Network,
    Busy,
}

impl CallEndReason {
    /// The known reasons, by their wire numbers. `None` for anything else.
    #[must_use]
    pub const fn from_wire(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::ByCaller,
            1 => Self::ByCallee,
            2 => Self::Declined,
            3 => Self::NoAnswer,
            4 => Self::Failed,
            5 => Self::Network,
            6 => Self::Busy,
            _ => return None,
        })
    }

    /// This reason's wire number, for the frames this client sends.
    #[must_use]
    pub const fn to_wire(self) -> u32 {
        match self {
            Self::ByCaller => 0,
            Self::ByCallee => 1,
            Self::Declined => 2,
            Self::NoAnswer => 3,
            Self::Failed => 4,
            Self::Network => 5,
            Self::Busy => 6,
        }
    }
}

/// Why a callee declined, as the wire numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDeclineReason {
    Busy,
    Declined,
}

impl CallDeclineReason {
    /// This reason's wire number, for the frame this client sends.
    #[must_use]
    pub const fn to_wire(self) -> u32 {
        match self {
            Self::Busy => 0,
            Self::Declined => 1,
        }
    }
}

/// What kind of media a call carries, as the wire numbers it.
///
/// Video is a future this build does not place, but a video invite from a newer
/// client still rings and answers — degraded to audio by `from_wire`, never
/// refused, because the call as audio beats no call at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallMediaKind {
    Audio,
    Video,
}

impl CallMediaKind {
    /// The known kinds, by their wire numbers; anything unknown is Audio.
    #[must_use]
    pub const fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::Video,
            _ => Self::Audio,
        }
    }
}

/// The invite statuses the caller's own invite can come back with.
pub const INVITE_RINGING: u32 = 0;
/// The `CallInviteResult` status that means the callee said no.
///
/// Read only by the cfg(test) label tests: the engine itself folds every non-ringing
/// status through [`invite_end_reason`], which maps the declined status onto the
/// `Declined` end reason without naming it.
#[allow(dead_code)]
pub const INVITE_DECLINED: u32 = 1;
pub const INVITE_EXPIRED: u32 = 2;
pub const INVITE_BLOCKED: u32 = 3;
pub const INVITE_BUSY: u32 = 4;

/// The end reason an invite status becomes when the invite never rang.
///
/// The reason enum has no Blocked member: the wire drew that distinction in the
/// invite status, and the screen must not state a human refusal that never
/// happened — a blocked invite reads [`INVITE_BLOCKED`] and is shown as
/// "Unavailable", not as "Declined".
#[must_use]
pub const fn invite_end_reason(status: u32) -> CallEndReason {
    match status {
        INVITE_EXPIRED => CallEndReason::NoAnswer,
        INVITE_BUSY => CallEndReason::Busy,
        _ => CallEndReason::Declined,
    }
}

/// How long the local ring mirror has left, clamped at zero.
///
/// Mirrors the invite's expiry on both sides so the callee's ring dies at the
/// same instant the caller's does, without a server round trip. The clamp at
/// zero is what makes a late reply fire at once rather than after a negative
/// sleep.
#[must_use]
pub fn ring_timeout(expires_at: Timestamp, now: Timestamp) -> Duration {
    let remaining = (expires_at.as_unix_ms() - now.as_unix_ms()).max(0);
    Duration::from_millis(remaining as u64)
}

/// Whether a state event means the ringing call was answered — by this device
/// or a sibling — which retires the ring without ending the call.
#[must_use]
pub fn answers_ringing_call(state: u32, ringing: Option<Id>) -> bool {
    match ringing {
        None => false,
        Some(_) => matches!(
            CallState::from_wire(state),
            Some(CallState::Connecting | CallState::Connected)
        ),
    }
}

/// Whether a state event means the ringing call is over.
#[must_use]
pub fn ends_ringing_call(state: u32, ringing: Option<Id>) -> bool {
    match ringing {
        None => false,
        Some(_) => matches!(CallState::from_wire(state), Some(CallState::Ended)),
    }
}

/// What this device does with an inbound invite.
///
/// Invites are Critical and at-least-once, so a redelivery of a call already
/// ringing is not news and must not ring twice — one call sounding like two is
/// indistinguishable from harassment by a client that is merely buggy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingInviteDisposition {
    /// Fresh, unexpired, and this device is free: show the ring.
    Ring,
    /// Not ours to answer: already expired in flight, or a redelivery of a call
    /// this device is already ringing or answering.
    Ignore,
    /// This device is occupied by another call: answer Busy, which stops the new
    /// caller's ring without implying that anybody refused.
    DeclineBusy,
}

/// Decides an inbound invite's disposition against this device's occupancy.
#[must_use]
pub fn incoming_invite_disposition(
    expires_at: Timestamp,
    call_id: Id,
    ringing_call_id: Option<Id>,
    active_call_id: Option<Id>,
    busy: bool,
    now: Timestamp,
) -> IncomingInviteDisposition {
    // Expired in flight: rings nobody, declines nobody.
    if expires_at.as_unix_ms() <= now.as_unix_ms() {
        return IncomingInviteDisposition::Ignore;
    }
    // The same call already ringing here, or already answered here: a
    // redelivery, never news.
    if ringing_call_id == Some(call_id) || active_call_id == Some(call_id) {
        return IncomingInviteDisposition::Ignore;
    }
    // Occupied by another call, or still placing one: Busy.
    if busy || ringing_call_id.is_some() || active_call_id.is_some() {
        return IncomingInviteDisposition::DeclineBusy;
    }
    IncomingInviteDisposition::Ring
}

/// A call duration as `M:SS` — minutes unbounded, negative time floored.
#[must_use]
pub fn format_call_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The six states a call screen can show.
///
/// A subset of the machine of [`CallState`]: *Degraded* is not a wire fact but a
/// display judgement (quality holding media back — always false in this voice
/// build, with the plumbing here for the statistics feed that will flip it), and
/// the wire's states map on with the caller's ring shown as "Calling…" rather
/// than the callee's "Ringing".
///
/// Read only by the cfg(test) label tests: this build's overlay composes its
/// phase line from the worker's own projection (which knows "Calling…" from
/// "Ringing" by role, and the duration from the timestamps), and the enum is the
/// shared vocabulary the video build's screen will adopt.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDisplayState {
    Ringing,
    Connecting,
    Connected,
    Reconnecting,
    Degraded,
    Ended,
}

/// The display state for a wire state, given whether quality is degraded.
#[allow(dead_code)] // read only by the cfg(test) label tests, like the enum it answers
#[must_use]
pub const fn display_state_of(state: CallState, degraded: bool) -> CallDisplayState {
    match state {
        CallState::Ringing => CallDisplayState::Ringing,
        CallState::Connecting => CallDisplayState::Connecting,
        CallState::Connected => {
            if degraded {
                CallDisplayState::Degraded
            } else {
                CallDisplayState::Connected
            }
        }
        CallState::Reconnecting => CallDisplayState::Reconnecting,
        CallState::Ended => CallDisplayState::Ended,
    }
}

/// The one-word label of a display state.
#[allow(dead_code)] // read only by the cfg(test) label tests, like the enum it names
#[must_use]
pub const fn call_state_label(state: CallDisplayState) -> &'static str {
    match state {
        CallDisplayState::Ringing => "Ringing",
        CallDisplayState::Connecting => "Connecting\u{2026}",
        CallDisplayState::Connected => "Connected",
        CallDisplayState::Reconnecting => "Reconnecting\u{2026}",
        CallDisplayState::Degraded => "Poor connection \u{2014} video paused",
        CallDisplayState::Ended => "Call ended",
    }
}

/// The label of an end reason, or "Call ended" when there is no reason.
///
/// A declined call, a failed call, and a network death are different facts, and
/// calling them all "Call ended" throws away the one thing the user needs before
/// calling back.
#[must_use]
pub const fn end_reason_label(reason: Option<CallEndReason>) -> &'static str {
    match reason {
        None | Some(CallEndReason::ByCaller | CallEndReason::ByCallee) => "Call ended",
        Some(CallEndReason::Declined) => "Declined",
        Some(CallEndReason::NoAnswer) => "No answer",
        Some(CallEndReason::Failed) => "Failed to connect",
        Some(CallEndReason::Network) => "Connection lost",
        Some(CallEndReason::Busy) => "Busy",
    }
}

/// The full line an ended call shows: the invite status first (a blocked invite
/// says "Unavailable", not a refusal that never happened), then the reason.
#[must_use]
pub fn ended_reason_line(invite_status: Option<u32>, end_reason: Option<CallEndReason>) -> String {
    if invite_status == Some(INVITE_BLOCKED) {
        return "Unavailable".to_owned();
    }
    end_reason_label(end_reason).to_owned()
}

/// What a media kind is called on a ring: "Incoming voice call".
#[must_use]
pub const fn media_kind_label(kind: CallMediaKind) -> &'static str {
    match kind {
        CallMediaKind::Audio => "voice call",
        CallMediaKind::Video => "video call",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same id text the web and Android suites use, so the AEAD domain bytes
    /// are byte-identical across the three clients' tests.
    const CALL_TEXT: &str = "0123456789ABCDEFGHJKMNPQRS";
    const OTHER_TEXT: &str = "0123456789ABCDEFGHJKMNPQRT";

    fn call_id() -> Id {
        Id::parse(CALL_TEXT).expect("a valid id text")
    }

    fn other_call_id() -> Id {
        Id::parse(OTHER_TEXT).expect("a valid id text")
    }

    const KEY: [u8; CALL_KEY_LEN] = [1u8; CALL_KEY_LEN];
    const WRONG_KEY: [u8; CALL_KEY_LEN] = [65u8; CALL_KEY_LEN];
    const PAYLOAD: &[u8] = b"v=0\r\no=- 46117317 2 IN IP4 127.0.0.1\r\n";

    /// The version-2 envelope's fixed overhead: version byte, 24-byte nonce,
    /// 16-byte tag.
    const V2_OVERHEAD: usize = 1 + 24 + 16;

    #[test]
    fn a_sealed_signal_is_the_version_byte_then_the_aead_output() {
        let sealed = seal_call_signal(PAYLOAD, &KEY, &call_id()).expect("seals");
        assert_eq!(
            sealed.len(),
            V2_OVERHEAD + PAYLOAD.len(),
            "the envelope is version || nonce || ciphertext || tag"
        );
        assert_eq!(sealed[0], 2);
        assert!(
            !sealed.windows(PAYLOAD.len()).any(|w| w == PAYLOAD),
            "the payload never rides in the clear"
        );
        let opened = open_call_signal(&sealed, &KEY, &call_id()).expect("opens");
        assert_eq!(opened, PAYLOAD);
    }

    #[test]
    fn each_seal_uses_a_fresh_nonce() {
        let call = call_id();
        let first = seal_call_signal(PAYLOAD, &KEY, &call).expect("seals");
        let second = seal_call_signal(PAYLOAD, &KEY, &call).expect("seals");
        assert_ne!(first, second);
        assert_eq!(open_call_signal(&first, &KEY, &call), Ok(PAYLOAD.to_vec()));
        assert_eq!(open_call_signal(&second, &KEY, &call), Ok(PAYLOAD.to_vec()));
    }

    #[test]
    fn a_seal_refuses_to_open_under_the_wrong_key_call_or_after_an_edit() {
        let call = call_id();
        let sealed = seal_call_signal(PAYLOAD, &KEY, &call).expect("seals");
        assert!(open_call_signal(&sealed, &WRONG_KEY, &call).is_err());
        assert!(open_call_signal(&sealed, &KEY, &other_call_id()).is_err());
        let mut edited = sealed.clone();
        let mid = edited.len() / 2;
        edited[mid] = edited[mid].wrapping_add(1);
        assert!(open_call_signal(&edited, &KEY, &call).is_err());
    }

    #[test]
    fn an_envelope_this_build_cannot_read_is_refused_not_misread() {
        let call = call_id();
        assert!(open_call_signal(&[], &KEY, &call).is_err());
        // Too short to hold its own header, but with the version byte set.
        let short = [2u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(open_call_signal(&short, &KEY, &call).is_err());
        // Version 99 and the reserved version 3: a future envelope is refused,
        // not interpreted as legacy plaintext.
        let mut future = vec![99u8];
        future.resize(V2_OVERHEAD + PAYLOAD.len(), 0);
        assert!(open_call_signal(&future, &KEY, &call).is_err());
        let mut v3 = vec![3u8];
        v3.resize(V2_OVERHEAD + PAYLOAD.len(), 0);
        assert!(open_call_signal(&v3, &KEY, &call).is_err());
    }

    #[test]
    fn a_legacy_version1_envelope_opens_under_any_key() {
        let call = call_id();
        let mut legacy = vec![1u8];
        legacy.resize(LEGACY_PREFIX, 0);
        legacy.extend_from_slice(PAYLOAD);
        assert_eq!(
            open_call_signal(&legacy, &KEY, &call),
            Ok(PAYLOAD.to_vec()),
            "it was never encrypted, so any key opens it"
        );
        assert_eq!(
            open_call_signal(&legacy, &WRONG_KEY, &other_call_id()),
            Ok(PAYLOAD.to_vec())
        );
        // Even the legacy envelope must be long enough for its own framing slots.
        let short = [1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(open_call_signal(&short, &KEY, &call).is_err());
    }

    #[test]
    fn a_call_key_event_is_16_id_bytes_then_32_key_bytes() {
        let call = call_id();
        let event = encode_call_key_event(&call, &KEY);
        assert_eq!(event.len(), 48);
        assert_eq!(decode_call_key_event(&event), Some((call, KEY)));
    }

    #[test]
    fn a_call_key_event_of_the_wrong_width_is_dropped() {
        assert_eq!(decode_call_key_event(&[0u8; 16]), None);
        assert_eq!(decode_call_key_event(&[0u8; 49]), None);
    }

    #[test]
    fn two_minted_call_keys_differ() {
        assert_ne!(generate_call_key(), generate_call_key());
    }

    #[test]
    fn an_ice_batch_round_trips_and_its_field_names_are_the_contract() {
        let batch = vec![
            IceCandidateJson {
                candidate: Some(
                    "candidate:1 1 UDP 2130706431 192.168.1.4 8998 typ host".to_owned(),
                ),
                sdp_mid: Some("0".to_owned()),
                sdp_mline_index: 0,
                username_fragment: None,
            },
            // An end-of-gathering notification: neither field, just the sentinel.
            IceCandidateJson {
                candidate: None,
                sdp_mid: None,
                sdp_mline_index: 0,
                username_fragment: None,
            },
        ];
        let bytes = encode_ice_batch(&batch).expect("encodes");
        assert_eq!(decode_ice_batch(&bytes).expect("decodes"), batch);
        let text = String::from_utf8(bytes).expect("the batch is JSON text");
        assert!(
            text.contains("\"sdpMLineIndex\""),
            "the field names are the cross-client contract"
        );
        assert!(
            !text.contains("\"candidate\":null"),
            "an empty candidate is omitted, not the string \"null\""
        );
    }

    #[test]
    fn bytes_that_are_not_the_payload_they_claim_to_be_are_refused() {
        assert!(decode_ice_batch(b"not json").is_err());
        // An SDP object where a batch belongs.
        assert!(decode_ice_batch(br#"{"type":"offer","sdp":"v=0"}"#).is_err());
        assert!(decode_sdp_description(b"not json").is_err());
        assert!(decode_sdp_description(br#"["candidate:1"]"#).is_err());
    }

    #[test]
    fn an_sdp_description_round_trips_through_the_sealed_payload_shape() {
        let description = RTCSessionDescription {
            sdp_type: webrtc::peer_connection::sdp::sdp_type::RTCSdpType::Offer,
            sdp: "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n".to_owned(),
            ..Default::default()
        };
        let bytes = encode_sdp_description(&description).expect("encodes");
        assert_eq!(
            String::from_utf8(bytes.clone()).expect("json text"),
            r#"{"type":"offer","sdp":"v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n"}"#,
            "the JSON shape is the cross-client contract"
        );
        let decoded = decode_sdp_description(&bytes).expect("decodes");
        assert_eq!(decoded.sdp, description.sdp);
        assert_eq!(decoded.sdp_type, description.sdp_type);
    }

    #[test]
    fn a_duration_reads_m_ss_with_minutes_unbounded() {
        assert_eq!(format_call_duration(0), "0:00");
        assert_eq!(format_call_duration(83_000), "1:23");
        assert_eq!(format_call_duration(65_000), "1:05");
        assert_eq!(format_call_duration(3_661_000), "61:01");
    }

    #[test]
    fn a_wire_state_narrows_or_refuses_and_degraded_is_a_display_judgement() {
        assert_eq!(CallState::from_wire(0), Some(CallState::Ringing));
        assert_eq!(CallState::from_wire(2), Some(CallState::Connected));
        assert_eq!(CallState::from_wire(4), Some(CallState::Ended));
        assert_eq!(
            CallState::from_wire(9),
            None,
            "an unknown wire state is never a guess"
        );
        assert_eq!(
            display_state_of(CallState::Connected, false),
            CallDisplayState::Connected
        );
        assert_eq!(
            display_state_of(CallState::Connected, true),
            CallDisplayState::Degraded
        );
        assert_eq!(
            display_state_of(CallState::Ringing, true),
            CallDisplayState::Ringing
        );
        assert_eq!(
            call_state_label(CallDisplayState::Connecting),
            "Connecting\u{2026}"
        );
    }

    #[test]
    fn every_end_reason_has_its_own_line_and_a_blocked_invite_says_unavailable() {
        assert_eq!(end_reason_label(Some(CallEndReason::Declined)), "Declined");
        assert_eq!(end_reason_label(Some(CallEndReason::NoAnswer)), "No answer");
        assert_eq!(
            end_reason_label(Some(CallEndReason::Failed)),
            "Failed to connect"
        );
        assert_eq!(
            end_reason_label(Some(CallEndReason::Network)),
            "Connection lost"
        );
        assert_eq!(end_reason_label(Some(CallEndReason::Busy)), "Busy");
        assert_eq!(end_reason_label(None), "Call ended");
        assert_eq!(
            ended_reason_line(Some(INVITE_BLOCKED), None),
            "Unavailable",
            "the wire drew this distinction in the invite status, not the reason"
        );
        assert_eq!(ended_reason_line(Some(INVITE_DECLINED), None), "Declined");
        assert_eq!(
            ended_reason_line(None, Some(CallEndReason::NoAnswer)),
            "No answer"
        );
    }

    #[test]
    fn an_invite_that_never_rang_maps_to_an_end_reason_and_a_kind_degrades_to_audio() {
        assert_eq!(invite_end_reason(INVITE_EXPIRED), CallEndReason::NoAnswer);
        assert_eq!(invite_end_reason(INVITE_BUSY), CallEndReason::Busy);
        assert_eq!(invite_end_reason(INVITE_DECLINED), CallEndReason::Declined);
        assert_eq!(invite_end_reason(INVITE_BLOCKED), CallEndReason::Declined);
        assert_eq!(CallMediaKind::from_wire(1), CallMediaKind::Video);
        assert_eq!(CallMediaKind::from_wire(0), CallMediaKind::Audio);
        assert_eq!(
            CallMediaKind::from_wire(7),
            CallMediaKind::Audio,
            "an unknown media kind is the call as audio, not no call"
        );
        assert_eq!(media_kind_label(CallMediaKind::Audio), "voice call");
        assert_eq!(media_kind_label(CallMediaKind::Video), "video call");
    }

    #[test]
    fn the_local_ring_mirror_clamps_at_zero_so_a_late_reply_fires_at_once() {
        let now = Timestamp::from_unix_ms(1_774_944_000_000);
        assert_eq!(
            ring_timeout(Timestamp::from_unix_ms(now.as_unix_ms() + 45_000), now),
            Duration::from_millis(45_000)
        );
        assert_eq!(
            ring_timeout(Timestamp::from_unix_ms(now.as_unix_ms() - 1_000), now),
            Duration::ZERO
        );
    }

    #[test]
    fn a_sibling_device_answering_retires_the_ring_without_ending_the_call() {
        let ringing = call_id();
        assert!(answers_ringing_call(1, Some(ringing)));
        assert!(answers_ringing_call(2, Some(ringing)));
        assert!(
            !answers_ringing_call(0, Some(ringing)),
            "a Ringing transition is not an answer"
        );
        assert!(
            !answers_ringing_call(2, Some(other_call_id())),
            "another call's state is not ours"
        );
        assert!(
            !answers_ringing_call(2, None),
            "no ring tracked, nothing to retire"
        );
        assert!(ends_ringing_call(4, Some(ringing)));
        assert!(
            !ends_ringing_call(3, Some(ringing)),
            "a live transition does not end the ring"
        );
        assert!(
            !ends_ringing_call(4, Some(other_call_id())),
            "another call's end is not ours"
        );
    }

    #[test]
    fn an_inbound_invite_is_placed_against_this_devices_occupancy() {
        let call = call_id();
        let other = other_call_id();
        let now = Timestamp::from_unix_ms(1_774_944_000_000);
        let fresh = Timestamp::from_unix_ms(now.as_unix_ms() + 45_000);

        // Expired in flight: rings nobody, declines nobody.
        assert_eq!(
            incoming_invite_disposition(now, call, None, None, false, now),
            IncomingInviteDisposition::Ignore
        );
        // The same call already ringing, or already answered here: a redelivery.
        assert_eq!(
            incoming_invite_disposition(fresh, call, Some(call), None, false, now),
            IncomingInviteDisposition::Ignore
        );
        assert_eq!(
            incoming_invite_disposition(fresh, call, None, Some(call), false, now),
            IncomingInviteDisposition::Ignore
        );
        // Occupied by another call — in progress or still ringing — is Busy.
        assert_eq!(
            incoming_invite_disposition(fresh, other, None, Some(call), false, now),
            IncomingInviteDisposition::DeclineBusy
        );
        assert_eq!(
            incoming_invite_disposition(fresh, other, Some(call), None, false, now),
            IncomingInviteDisposition::DeclineBusy
        );
        assert_eq!(
            incoming_invite_disposition(fresh, other, None, None, true, now),
            IncomingInviteDisposition::DeclineBusy
        );
        // Fresh and free: ring.
        assert_eq!(
            incoming_invite_disposition(fresh, other, None, None, false, now),
            IncomingInviteDisposition::Ring
        );
    }

    /// The domain string is the cross-client contract's join point: the id's own
    /// text, prefixed the same way everywhere.
    #[test]
    fn the_seal_domain_is_the_id_text_after_its_prefix() {
        assert_eq!(
            domain(&call_id()),
            format!("migo-call-signal:{CALL_TEXT}").into_bytes()
        );
    }
}
