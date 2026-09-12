//! The call engine: voice calls end to end, signalled through the server, media direct.
//!
//! # The two planes
//!
//! A call is two planes with nothing in common but the [`Id`] of the call. The *signalling* plane
//! is the server: invite, answer, decline, cancel, end, and the relaying of sealed SDP and ICE
//! blobs — the server routes them by device id but cannot read them, because an SDP body carries
//! DTLS fingerprints and ICE candidates carry the two parties' network addresses, and a
//! signalling server that could read them would learn exactly what the end-to-end promise exists
//! to protect. The *media* plane is WebRTC between the two devices, encrypted end to end by
//! construction and never touching a Migo server.
//!
//! The glue between the planes is the call key: 32 random bytes the caller mints, sends to the
//! callee through the E2EE **message** layer (before the invite, so it wins the race), and both
//! sides then use to seal and open every signalling blob of that call — [`call_signal`] is the
//! cross-client contract for that seal, shared field for field with the web and Android clients.
//!
//! # This file's shape
//!
//! The engine is state in [`Calls`] plus methods on the worker, because every transition either
//! sends a frame (needs the gateway) or emits an event (needs the sink) — the same single-owner
//! discipline the rest of [`super`] follows. Everything asynchronous that is not the main loop —
//! ICE gathering callbacks, transport state callbacks, timers — reports back as a [`CallTick`]
//! on one channel, which the worker's select loop drains beside commands and frames, so no
//! transition ever races another.
//!
//! # Web parity
//!
//! The flows mirror the web client's `call-manager.tsx` decision for decision: both sides fetch
//! TURN (a call that must relay fails either way, but a call that only needed STUN must not be
//! refused because the relay list was unreachable), the caller holds its ICE candidates until the
//! answer's relay names the answering device, a disconnected transport gets a thirty-second
//! reconnect window before the call ends as a network failure, and an accept waits up to five
//! seconds for the call key rather than failing a call the caller placed in the correct order.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use migo_core::{Id, OsRandom, Timestamp};
use migo_protocol::{MessageKind, Opcode};
use tokio::sync::mpsc;

use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_PCMU, MIME_TYPE_VP8};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_remote::TrackRemote;

use super::call_audio::{self, Resampler};
use super::call_signal::{
    self, answers_ringing_call, decode_ice_batch, decode_sdp_description, encode_call_key_event,
    encode_ice_batch, encode_sdp_description, ends_ringing_call, generate_call_key,
    incoming_invite_disposition, invite_end_reason, open_call_signal, ring_timeout,
    seal_call_signal, CallDeclineReason, CallEndReason, CallMediaKind, CallState, IceCandidateJson,
    ANSWERED_ELSEWHERE_MESSAGE, CALL_KEY_EVENT, CALL_KEY_LEN, INVITE_RINGING, MISSED_CALL_MESSAGE,
};
use super::call_video;
use super::{Event, Sink, Worker};
use crate::crypto::content::{self, Content};
use crate::model::ToastKind;

/// How long gathered ICE candidates linger before one relay carries them: a trickle leaves as
/// few frames as it can, and a batch that waited 250 ms costs the same as one that left at once.
const ICE_LINGER: Duration = Duration::from_millis(250);

/// How long a disconnected transport gets before the call ends as a network failure. A blip is
/// not an end; media coming back cancels the window.
const RECONNECT_WINDOW: Duration = Duration::from_secs(30);

/// How long an accept waits for the call's key before giving up on answering. The caller sends
/// the key message and *then* the invite, so the wait is for frames crossing badly on a slow
/// connection, not for the common path — five seconds is far under the invite's own expiry and
/// far over any honest reordering.
const CALL_KEY_WAIT: Duration = Duration::from_secs(5);

/// How long the TURN fetch gets before the call proceeds on the STUN fallback alone. The fetch
/// is one frame on a live connection; two seconds is generous, and a call must not sit silent
/// waiting on a list it can survive without.
const TURN_WAIT: Duration = Duration::from_secs(2);

/// The public STUN fallback every peer connection carries. The server's TURN list comes from
/// configuration and may legitimately be empty; a STUN server costs nothing and is what lets a
/// direct connection find its public reflexive address at all, so without it calls work only on
/// the same LAN.
const STUN_FALLBACK: &str = "stun:stun.l.google.com:19302";

/// Everything the worker's select loop needs to wake for, from timers and WebRTC callbacks.
///
/// One channel rather than spawned tasks acting directly, because every transition reads or
/// writes engine state and the main loop is its only owner — a callback that mutated calls in
/// place would race the very loop it reports to. A tick arriving after its call ended is
/// harmless: every handler first checks that its call is still in the state the tick was armed
/// against.
pub(crate) enum CallTick {
    /// One local ICE candidate gathered, or `None` that gathering finished.
    Candidate {
        call_id: Id,
        candidate: Option<RTCIceCandidate>,
    },
    /// The peer connection's transport moved.
    Transport {
        call_id: Id,
        state: RTCPeerConnectionState,
    },
    /// The ICE batch's linger elapsed: whatever is batched is all this trickle holds.
    IceLinger { call_id: Id },
    /// The local mirror of the invite's expiry fired — the caller's or the callee's.
    RingExpiry { call_id: Id },
    /// The reconnect window closed on a still-disconnected transport.
    ReconnectWindow { call_id: Id },
    /// An accept gave up waiting for the call key.
    KeyWait { call_id: Id },
    /// The call key arrived for an accept that was waiting on it.
    KeyArrived { call_id: Id },
    /// The TURN fetch did not answer in time; proceed on the STUN fallback.
    TurnTimeout,
}

/// Arms one tick: sleeps, then reports.
fn arm_later(tick_tx: &mpsc::UnboundedSender<CallTick>, tick: CallTick, after: Duration) {
    let tick_tx = tick_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        let _ = tick_tx.send(tick);
    });
}

/// The ICE servers for one call's peer connection: the configured TURN relays, then the public
/// STUN fallback. An entry with an empty username is an anonymous relay; the credential fields
/// stay empty rather than invented.
fn ice_servers_from(turn: &[migo_protocol::TurnServer]) -> Vec<RTCIceServer> {
    let mut servers: Vec<RTCIceServer> = turn
        .iter()
        .map(|server| RTCIceServer {
            urls: vec![server.url.clone()],
            username: server.username.clone(),
            credential: server.credential.clone(),
        })
        .collect();
    servers.push(RTCIceServer {
        urls: vec![STUN_FALLBACK.to_owned()],
        username: String::new(),
        credential: String::new(),
    });
    servers
}

/// Feeds the remote track into the speaker: every RTP packet the far end sent, decoded and
/// resampled to whatever rate this device's output actually runs at.
///
/// A read that fails is the track closing — the call ended, or the transport died — and the
/// pump simply stops; the speaker going quiet is the honest rendering of both. The sender sits
/// behind a mutex because the WebRTC handler that spawns this pump must be `Sync` and a bare
/// channel sender is not.
async fn playback_pump(
    track: Arc<TrackRemote>,
    speaker: Arc<Mutex<std::sync::mpsc::Sender<Vec<i16>>>>,
    rate: u32,
) {
    let mut resampler = Resampler::new(call_audio::CALL_SAMPLE_RATE, rate);
    loop {
        let Ok((packet, _)) = track.read_rtp().await else {
            return;
        };
        let linear = call_audio::ulaw_decode(&packet.payload);
        let mut out = Vec::with_capacity(linear.len() * 2);
        resampler.process(&linear, &mut out);
        match speaker.lock() {
            Ok(speaker) => {
                if speaker.send(out).is_err() {
                    return; // the call tore down
                }
            }
            Err(_) => return,
        }
    }
}

/// Feeds the microphone into the local track: resampled to the wire's 8 kHz, muted to silence
/// rather than absence, and packed into whole 20 ms frames so the packetizer sees one uniform
/// stream. Runs on its own thread because the microphone channel blocks, and blocking an async
/// runtime thread is the microphone's to do only from the outside. The thread ends when the
/// microphone's guard drops and the channel closes.
fn spawn_capture_pump(
    microphone: std::sync::mpsc::Receiver<Vec<i16>>,
    rate: u32,
    muted: Arc<AtomicBool>,
    sample_tx: mpsc::UnboundedSender<Sample>,
) {
    // Only resample when the device rate differs from the wire's: a same-rate stream through a
    // linear interpolator would come out measurably worse than a straight copy.
    let mut resampler = (rate != call_audio::CALL_SAMPLE_RATE)
        .then(|| Resampler::new(rate, call_audio::CALL_SAMPLE_RATE));
    let _ = std::thread::Builder::new()
        .name("migo-call-mic".to_owned())
        .spawn(move || {
            // The samples of the frame being assembled: whole 20 ms frames leave, a partial
            // one waits for the next chunk. A call's audio is a stream, and a packet per
            // fragment of a fragment is signalling the far end never asked for.
            let mut frame: Vec<i16> = Vec::with_capacity(call_audio::FRAME_SAMPLES);
            while let Ok(chunk) = microphone.recv() {
                let mut linear = chunk;
                if let Some(resampler) = resampler.as_mut() {
                    let mut resampled = Vec::with_capacity(linear.len() * 2);
                    resampler.process(&linear, &mut resampled);
                    linear = resampled;
                }
                frame.extend(linear);
                while frame.len() >= call_audio::FRAME_SAMPLES {
                    let samples: Vec<i16> = frame.drain(..call_audio::FRAME_SAMPLES).collect();
                    // Mute is silence, not absence: the far end hears a quiet line, not a
                    // call that sounds hung up — the same semantics a disabled track has on
                    // the web. The quiet line is bound to a name so it outlives the
                    // statement that borrows it.
                    let quiet = muted.load(Ordering::Relaxed);
                    let silence = vec![0i16; samples.len()];
                    let bytes = call_audio::ulaw_encode(if quiet { &silence } else { &samples });
                    let sample = Sample {
                        data: Bytes::from(bytes),
                        duration: Duration::from_secs_f64(
                            samples.len() as f64 / f64::from(call_audio::CALL_SAMPLE_RATE),
                        ),
                        ..Sample::default()
                    };
                    if sample_tx.send(sample).is_err() {
                        return; // the writer task is gone; the call tore down
                    }
                }
            }
        });
}

/// What the overlay shows for one call: everything the UI renders, in one plain shape, so no
/// peer connection, key, or candidate ever crosses into UI code.
#[derive(Debug, Clone)]
pub struct CallView {
    /// The other account: the callee for a call this device placed, the caller otherwise.
    pub peer: Id,
    /// Whether this device placed the call — decides whose "Cancel"/"Decline" button shows and
    /// whether the ring says "Calling…" or "Incoming".
    pub outgoing: bool,
    /// What the call carries; a video invite from a newer client is answered as audio.
    pub kind: CallMediaKind,
    /// The phase, a projection of the wire's states plus the pre-invite placement.
    pub phase: CallPhase,
    /// Whether this side's microphone is muted.
    pub muted: bool,
    /// When media first connected, in unix ms — the duration timer's zero, kept through the
    /// ended screen so the last line can state how long the call was.
    pub started_at: Option<Timestamp>,
    /// When the call ended, in unix ms; `None` while it lives.
    pub ended_at: Option<Timestamp>,
    /// The line under the phase on an ended call: the reason, or a note this build knows.
    pub line: Option<String>,
    /// The remote's decoded video, when the call carries any: the overlay polls this every
    /// repaint while the call is connected. `None` for the pre-call screens (a ring, a
    /// placement, an accept) — there is no track yet to decode.
    pub video: Option<call_video::VideoSlot>,
}

/// The overlay's phases: the wire's states plus the ones the server never sees — this device's
/// own placement (before any invite exists) and the callee's ring (a call this device is not
/// in yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    /// Ringing: an outgoing call the callee has not answered, or an incoming one this device
    /// has not answered. `outgoing` on the view says which.
    Ringing,
    /// Answered; SDP and ICE are being exchanged but media is not flowing yet.
    Connecting,
    /// Media is flowing.
    Connected,
    /// The transport dropped; the reconnect window is running.
    Reconnecting,
    /// Over. `line` on the view states why.
    Ended,
}

/// An inbound invite ringing on this device, before any answer.
struct IncomingRing {
    call_id: Id,
    caller_id: Id,
    caller_device: Id,
    kind: CallMediaKind,
    sealed_offer: Vec<u8>,
}

/// A placement between the call button and the invite: the key is out, the TURN fetch is in
/// flight, and no call exists on the server yet.
struct Placing {
    conversation_id: Id,
    callee_id: Id,
    call_id: Id,
    kind: CallMediaKind,
    key: [u8; CALL_KEY_LEN],
}

/// An accept between the answer button and the answer: waiting on the call key, then the TURN
/// fetch, before any frame this call could be refused by goes out.
struct Answering {
    call_id: Id,
    caller_id: Id,
    caller_device: Id,
    kind: CallMediaKind,
    sealed_offer: Vec<u8>,
    /// `None` while the call key has not arrived; the KeyWait tick bounds the wait.
    key: Option<[u8; CALL_KEY_LEN]>,
}

/// One call this device is in, either role, from the invite's acceptance to dismissal.
struct TrackedCall {
    call_id: Id,
    /// The other account: the callee for a caller, the caller for a callee.
    peer: Id,
    is_caller: bool,
    kind: CallMediaKind,
    state: CallState,
    end_reason: Option<CallEndReason>,
    /// The invite's own refusal vocabulary when the call never rang, kept beside the end reason
    /// because a blocked refusal must read differently from a declined one.
    invite_status: Option<u32>,
    /// When media first connected; set exactly once.
    started_at: Option<Timestamp>,
    /// When this side began setting the call up, for the one-time setup-time report.
    setup_start: Timestamp,
    /// Whether the one-time setup-time report has gone out.
    stats_sent: bool,
    muted: bool,
    /// Shared with the capture pump: mute is silence, substituted at the source.
    muted_flag: Arc<AtomicBool>,
    /// The sealing key of this call; every signal blob opens under exactly it.
    key: [u8; CALL_KEY_LEN],
    pc: Arc<RTCPeerConnection>,
    microphone: Option<call_audio::Microphone>,
    speaker: Option<call_audio::Speaker>,
    /// The latest decoded frame of the remote's video, when the call carries any. Allocated
    /// for every call (the peer's description decides what arrives); read by the overlay
    /// through the view, written only by the video pump.
    video: call_video::VideoSlot,
    /// The peer device relays are addressed to; the caller learns it from the answer, the
    /// callee knew it from the invite.
    peer_device: Option<Id>,
    /// Candidates gathered but not relayed, waiting on the linger or a target device.
    ice_batch: Vec<IceCandidateJson>,
    /// Whether a linger timer is pending for the batch.
    ice_linger_armed: bool,
    /// Candidates the peer relayed before this side's remote description existed.
    held_ice: Vec<RTCIceCandidateInit>,
    remote_description_set: bool,
    /// Whether this side's own end frame has gone out; a network death and a hang-up must not
    /// pay for the same exit twice.
    end_sent: bool,
    /// When the call ended, for the frozen duration on the ended screen.
    ended_at: Option<Timestamp>,
}

impl TrackedCall {
    /// Drops the audio devices: the call is over, the microphone must not stay open a moment
    /// longer than the conversation did. The peer connection closes in a spawned task — its
    /// close is asynchronous and the worker's answer to the user must not wait on it.
    fn release_media(&mut self) {
        self.microphone = None;
        self.speaker = None;
        let pc = self.pc.clone();
        tokio::spawn(async move {
            let _ = pc.close().await;
        });
    }
}

/// The engine's whole state. One call at most: a person is in one conversation at a time, and a
/// second call while one lives is answered Busy by the invite's own disposition logic.
pub(crate) struct Calls {
    tick_tx: mpsc::UnboundedSender<CallTick>,
    /// The receiver half, parked in the worker's select loop between iterations.
    ticks: Option<mpsc::UnboundedReceiver<CallTick>>,
    active: Option<TrackedCall>,
    ringing: Option<IncomingRing>,
    placing: Option<Placing>,
    answering: Option<Answering>,
    /// The sealing key of each call this session has seen, by call id. Written by the caller
    /// (which minted it) and by the message layer (which adopts the caller's key event); an
    /// entry leaves with its call.
    keys: HashMap<Id, [u8; CALL_KEY_LEN]>,
    /// The call a TURN fetch is in flight for; the reply names no call of its own.
    pending_turn: Option<Id>,
}

impl Calls {
    pub(super) fn new() -> Self {
        let (tick_tx, ticks) = mpsc::unbounded_channel();
        Self {
            tick_tx,
            ticks: Some(ticks),
            active: None,
            ringing: None,
            placing: None,
            answering: None,
            keys: HashMap::new(),
            pending_turn: None,
        }
    }

    /// The select loop's tick arm: the next tick, or parked forever if the receiver was never
    /// installed. A separate method so the loop borrows only this struct, the same discipline
    /// the frame and beat arms follow.
    pub(super) async fn next_tick(&mut self) -> Option<CallTick> {
        match self.ticks.as_mut() {
            Some(ticks) => ticks.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Whether a call occupies this device — an ended call still on screen does not block a new
    /// one, but a placement, an accept, or a ring does.
    fn busy(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|call| call.state != CallState::Ended)
            || self.ringing.is_some()
            || self.placing.is_some()
            || self.answering.is_some()
    }

    /// The overlay's view of the tracked call. The ended line and timestamp are computed once
    /// here rather than held twice in the struct, so they can never disagree with the state.
    fn active_view(&self) -> Option<CallView> {
        let call = self.active.as_ref()?;
        let ended = call.state == CallState::Ended;
        Some(CallView {
            peer: call.peer,
            outgoing: call.is_caller,
            kind: call.kind,
            phase: match call.state {
                CallState::Ringing => CallPhase::Ringing,
                CallState::Connecting => CallPhase::Connecting,
                CallState::Connected => CallPhase::Connected,
                CallState::Reconnecting => CallPhase::Reconnecting,
                CallState::Ended => CallPhase::Ended,
            },
            muted: call.muted,
            started_at: call.started_at,
            ended_at: call.ended_at,
            line: ended
                .then(|| call_signal::ended_reason_line(call.invite_status, call.end_reason)),
            video: Some(call.video.clone()),
        })
    }

    /// The overlay's view of an incoming ring: a call this device is not in yet.
    fn ring_view(&self) -> Option<CallView> {
        let ring = self.ringing.as_ref()?;
        Some(CallView {
            peer: ring.caller_id,
            outgoing: false,
            kind: ring.kind,
            phase: CallPhase::Ringing,
            muted: false,
            started_at: None,
            ended_at: None,
            line: None,
            video: None,
        })
    }

    /// The overlay's view of a placement: "Calling…" from the first synchronous step, so the
    /// button's press is answered by the screen even before any invite exists.
    fn placing_view(&self) -> Option<CallView> {
        let place = self.placing.as_ref()?;
        Some(CallView {
            peer: place.callee_id,
            outgoing: true,
            kind: place.kind,
            phase: CallPhase::Ringing,
            muted: false,
            started_at: None,
            ended_at: None,
            line: None,
            video: None,
        })
    }

    /// The overlay's view of an accept in flight: the ring is retired and Connecting is the
    /// honest phase for the turn between the answer button and the answer itself.
    fn answering_view(&self) -> Option<CallView> {
        let answer = self.answering.as_ref()?;
        Some(CallView {
            peer: answer.caller_id,
            outgoing: false,
            kind: answer.kind,
            phase: CallPhase::Connecting,
            muted: false,
            started_at: None,
            ended_at: None,
            line: None,
            video: None,
        })
    }

    /// Everything an overlay could show right now: the live call if one is tracked, else the
    /// accept, else the ring, else the placement, else nothing.
    pub(super) fn view(&self) -> Option<CallView> {
        self.active_view()
            .or_else(|| self.answering_view())
            .or_else(|| self.ring_view())
            .or_else(|| self.placing_view())
    }

    /// Emits the current view, or closes the overlay when nothing is showing.
    fn emit(&self, sink: &Sink) {
        match self.view() {
            Some(view) => sink.send(Event::Call(view)),
            None => sink.send(Event::CallGone),
        }
    }
}

impl Worker {
    /// This device's own id, for the relay filter a sealed blob is not ours without.
    fn device_id(&self) -> Option<Id> {
        self.signed.as_ref().map(|signed| signed.account.device_id)
    }

    // --- the call key ---

    /// Adopts a call key that arrived through the message layer, waking an accept that is
    /// waiting on it. Returns `true` when the content was a call key — the caller then
    /// suppresses it, because a control event is not a message any conversation should render
    /// (the alternative is the "unsupported message" note, a bug the web client fixed the same
    /// way).
    ///
    /// Sync by necessity: it runs inside message decryption. The TURN fetch the woken accept
    /// needs is armed as a tick, which the loop that owns the gateway picks up.
    pub(super) fn adopt_call_key(&mut self, content: &Content) -> bool {
        let Content::ControlEvent { event: name, data } = content else {
            return false;
        };
        if name != CALL_KEY_EVENT {
            return false;
        }
        let Some(data) = data else {
            return false;
        };
        let Some((call_id, key)) = call_signal::decode_call_key_event(data) else {
            return false;
        };
        self.calls.keys.insert(call_id, key);
        // Wake an accept that was waiting for exactly this key; anything else keeps the key for
        // the invite it is about to be needed by.
        let waiting = self
            .calls
            .answering
            .as_ref()
            .is_some_and(|answer| answer.call_id == call_id && answer.key.is_none());
        if waiting {
            if let Some(answer) = self.calls.answering.as_mut() {
                answer.key = Some(key);
            }
            let _ = self.calls.tick_tx.send(CallTick::KeyArrived { call_id });
        }
        true
    }

    // --- placing a call ---

    /// Places a voice call: key first, then TURN, then media, offer, and the invite itself.
    pub(super) async fn start_call(&mut self, conversation_id: Id, callee_id: Id) {
        if self.signed.is_none() {
            return;
        }
        if self.calls.busy() {
            self.sink.toast("already in a call", ToastKind::Error);
            return;
        }
        let mut random = OsRandom;
        let call_id = Id::generate_at(Timestamp::now(), &mut random);
        let key = generate_call_key();
        self.calls.keys.insert(call_id, key);

        // The key must win the race against the invite: the callee opens the offer with it, so
        // it travels the message layer first, sealed for the callee's devices alone.
        if !self
            .send_call_key(conversation_id, callee_id, call_id, &key)
            .await
        {
            self.calls.keys.remove(&call_id);
            self.sink.toast(
                "fetching keys for this call, try again in a moment",
                ToastKind::Info,
            );
            return;
        }

        self.calls.placing = Some(Placing {
            conversation_id,
            callee_id,
            call_id,
            kind: CallMediaKind::Audio,
            key,
        });
        self.calls.emit(&self.sink);
        self.calls.pending_turn = Some(call_id);
        arm_later(&self.calls.tick_tx, CallTick::TurnTimeout, TURN_WAIT);
        let request = migo_protocol::CallTurnFetch { call_id };
        self.request(Opcode::CallTurnFetch, &request).await;
    }

    /// Sends the call key through the E2EE message layer, sealed for the callee's devices.
    ///
    /// The audience is the callee's devices — the one account the invite will name — rather
    /// than the conversation's whole membership: a call key is for the person being called, and
    /// in a group conversation a key the whole membership could open is a key nobody needed.
    /// Distributions of this device's sender-key chain ride the pairwise channel ahead of the
    /// sealed event, exactly the way a first text send builds its audience.
    ///
    /// Returns `false` when the callee's devices are not known yet — the fetch is out, the
    /// placement stops, and the call button is pressed again once the bundles arrive, the same
    /// honest retry a first text send asks for.
    async fn send_call_key(
        &mut self,
        conversation_id: Id,
        callee_id: Id,
        call_id: Id,
        key: &[u8; CALL_KEY_LEN],
    ) -> bool {
        let Ok(plaintext) = content::encode(
            &Content::ControlEvent {
                event: CALL_KEY_EVENT.to_owned(),
                data: Some(encode_call_key_event(&call_id, key)),
            },
            true,
        ) else {
            return false;
        };
        let devices: Vec<Id> = match self
            .signed
            .as_ref()
            .and_then(|signed| signed.devices.get(&callee_id))
        {
            Some(devices) if !devices.is_empty() => devices.clone(),
            _ => {
                let request = migo_protocol::KeyBundleRequest {
                    user_id: callee_id,
                    device_id: None,
                };
                self.request(Opcode::KeyBundleFetch, &request).await;
                return false;
            }
        };

        // The pairwise layer carries the distributions; the group layer carries the key. Each
        // distribution holds the chain key as of now, so it is taken before the content seal —
        // the same late-joiner property a first text send pays for.
        let distribution = match self.signed.as_mut() {
            Some(signed) => signed.groups.distribution(conversation_id),
            None => return false,
        };
        for device in &devices {
            let Some(signed) = self.signed.as_mut() else {
                return false;
            };
            if !signed.groups.needs_distribution(conversation_id, *device) {
                continue;
            }
            let bundle = signed.bundles.get(device).cloned();
            let Ok(control) = content::encode(
                &Content::ControlEvent {
                    event: "sender-key".to_owned(),
                    data: Some(distribution.clone()),
                },
                true,
            ) else {
                continue;
            };
            let envelope =
                match signed
                    .sessions
                    .seal(conversation_id, *device, bundle.as_ref(), &control)
                {
                    Ok(envelope) => envelope,
                    // A device whose bundle will not start a session is skipped, not fatal: its
                    // next send re-offers one.
                    Err(_) => continue,
                };
            let Ok(bytes) = envelope.encode() else {
                continue;
            };
            let exchange = migo_protocol::MessageSend {
                message_id: Id::generate_at(Timestamp::now(), &mut OsRandom),
                conversation_id,
                kind: MessageKind::KeyExchange,
                envelope: bytes,
                reply_to: None,
                expires_in_ms: None,
                sender_key_id: None,
            };
            self.request(Opcode::MessageSend, &exchange).await;
            if let Some(signed) = self.signed.as_mut() {
                signed.groups.mark_distributed(conversation_id, *device);
            }
        }

        // One MESSAGE_SEND for the whole event: sealed once, fanned out by the server to every
        // device the distributions reached. `seal` reports failure as an `Err` and absence of a
        // signed session as a `None`, and both collapse here to "the event did not go out".
        let Some(sealed) = self
            .signed
            .as_mut()
            .and_then(|signed| signed.groups.seal(conversation_id, &plaintext).ok())
        else {
            return false;
        };
        let message = migo_protocol::MessageSend {
            message_id: Id::generate_at(Timestamp::now(), &mut OsRandom),
            conversation_id,
            kind: MessageKind::System,
            envelope: sealed.envelope,
            reply_to: None,
            expires_in_ms: None,
            sender_key_id: Some(sealed.chain_id),
        };
        self.request(Opcode::MessageSend, &message).await;
        true
    }

    /// Builds the call's media once the ICE servers are known: the peer connection over them,
    /// the PCMU track, the audio devices, and the pumps that move sound between them and the
    /// wire. Both roles build exactly this; only the description exchange differs.
    ///
    /// PCMU for audio because it is the one codec every Migo client speaks. VP8 for video
    /// because it is the one the web client's browser always offers — a video invite answered
    /// by this build must decode the sender's actual stream, not a codec wishlist.
    async fn build_media(
        &self,
        call_id: Id,
        servers: Vec<RTCIceServer>,
    ) -> Result<
        (
            Arc<RTCPeerConnection>,
            call_audio::Microphone,
            call_audio::Speaker,
            Arc<AtomicBool>,
            call_video::VideoSlot,
        ),
        String,
    > {
        let mut engine = MediaEngine::default();
        engine
            .register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        mime_type: MIME_TYPE_PCMU.to_owned(),
                        clock_rate: call_audio::CALL_SAMPLE_RATE,
                        channels: 0,
                        ..Default::default()
                    },
                    payload_type: 0,
                    ..Default::default()
                },
                RTPCodecType::Audio,
            )
            .map_err(|error| format!("could not register the call's codec: {error}"))?;
        // Payload type 96 is the video convention — the first dynamic slot, and the one every
        // browser's VP8 offer names, so the answer's numbers match the offer's without a
        // remap. The clock is always 90 kHz for video: timestamps are frame times, not sample
        // counts.
        engine
            .register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        mime_type: MIME_TYPE_VP8.to_owned(),
                        clock_rate: 90_000,
                        channels: 0,
                        ..Default::default()
                    },
                    payload_type: 96,
                    ..Default::default()
                },
                RTPCodecType::Video,
            )
            .map_err(|error| format!("could not register the call's video codec: {error}"))?;
        let api = APIBuilder::new().with_media_engine(engine).build();
        let pc = api
            .new_peer_connection(RTCConfiguration {
                ice_servers: servers,
                ..Default::default()
            })
            .await
            .map_err(|error| format!("could not open the call's transport: {error}"))?;
        let pc = Arc::new(pc);

        let track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_PCMU.to_owned(),
                ..Default::default()
            },
            "audio".to_owned(),
            "migo".to_owned(),
        ));
        if let Err(error) = pc.add_track(track.clone()).await {
            let _ = pc.close().await;
            return Err(format!("could not add the call's audio: {error}"));
        }

        // The microphone and speaker are opened last so a machine with no audio devices fails
        // before any peer connection half-built around it is kept.
        let mut microphone = match call_audio::open_microphone() {
            Ok(microphone) => microphone,
            Err(error) => {
                let _ = pc.close().await;
                return Err(format!("could not open the microphone: {error}"));
            }
        };
        let speaker = match call_audio::open_speaker() {
            Ok(speaker) => speaker,
            Err(error) => {
                let _ = pc.close().await;
                return Err(format!("could not open the speaker: {error}"));
            }
        };
        let muted = Arc::new(AtomicBool::new(false));

        // One sample writer between the blocking capture pump and the awaitable track: the
        // pump hands whole frames to a channel, and this task pays the await.
        let (sample_tx, mut sample_rx) = mpsc::unbounded_channel::<Sample>();
        tokio::spawn(async move {
            while let Some(sample) = sample_rx.recv().await {
                let _ = track.write_sample(&sample).await;
            }
        });

        // The callbacks report as ticks; the loop that owns the engine decides what they mean.
        let tick_tx = self.calls.tick_tx.clone();
        pc.on_ice_candidate(Box::new(move |candidate| {
            let tick_tx = tick_tx.clone();
            Box::pin(async move {
                let _ = tick_tx.send(CallTick::Candidate { call_id, candidate });
            })
        }));
        // The speaker's sender crosses into the callback through a mutex because a handler must
        // be Sync and a bare channel sender is not; the lock is held per packet, never across
        // an await.
        let speaker_tx = Arc::new(Mutex::new(speaker.frames.clone()));
        let speaker_rate = speaker.rate;
        // The video slot the pump parks decoded frames in; the overlay reads it every repaint.
        // Allocated for every call — audio included — because which tracks arrive is not known
        // until the peer's description does, and a slot that might not exist is a video path
        // that might not start.
        let video_slot: call_video::VideoSlot = Arc::default();
        // The peer connection crosses into the video pump by weak reference, not a clone: the
        // pump outlives this builder (it runs until the track closes), and a strong reference
        // held by a handler the closed connection never clears would keep the whole transport
        // alive after teardown. The pump upgrades once per keyframe request and stops when the
        // upgrade fails — a closed connection has no keyframes left to ask for.
        let video_pc = Arc::downgrade(&pc);
        pc.on_track(Box::new(move |track, _receiver, _transceiver| {
            let speaker_tx = speaker_tx.clone();
            let video_slot = video_slot.clone();
            let video_pc = video_pc.clone();
            Box::pin(async move {
                if track.kind() == RTPCodecType::Video {
                    tokio::spawn(async move {
                        let pc = video_pc;
                        let media_ssrc = track.ssrc();
                        let pli = move || {
                            let request = PictureLossIndication {
                                sender_ssrc: media_ssrc,
                                media_ssrc,
                            };
                            if let Some(pc) = pc.upgrade() {
                                tokio::spawn(async move {
                                    let packet: Box<
                                        dyn webrtc::rtcp::packet::Packet + Send + Sync,
                                    > = Box::new(request);
                                    let _ = pc.write_rtcp(&[packet]).await;
                                });
                            }
                        };
                        call_video::video_pump(track, video_slot, Arc::new(pli)).await;
                    });
                } else {
                    tokio::spawn(async move {
                        playback_pump(track, speaker_tx, speaker_rate).await;
                    });
                }
            })
        }));
        let tick_tx = self.calls.tick_tx.clone();
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let tick_tx = tick_tx.clone();
            Box::pin(async move {
                let _ = tick_tx.send(CallTick::Transport { call_id, state });
            })
        }));

        // The receiver crosses to the capture pump by value; the guard struct stays here, and
        // its drop at call end is the signal the whole capture chain unwinds on.
        let mic_frames = microphone.take_frames();
        spawn_capture_pump(mic_frames, microphone.rate, muted.clone(), sample_tx);
        Ok((pc, microphone, speaker, muted, video_slot))
    }

    /// The placement's second half, on the TURN answer or its timeout: media, offer, invite.
    async fn proceed_placement(&mut self, servers: Vec<RTCIceServer>) {
        let Some(place) = self.calls.placing.take() else {
            return;
        };
        let abandon = |worker: &mut Worker, call_id: Id, reason: String| {
            worker.calls.keys.remove(&call_id);
            worker.sink.toast(reason, ToastKind::Error);
            worker.calls.emit(&worker.sink);
        };
        let (pc, microphone, speaker, muted_flag, video) =
            match self.build_media(place.call_id, servers).await {
                Ok(media) => media,
                Err(reason) => {
                    abandon(self, place.call_id, reason);
                    return;
                }
            };
        let offer = match pc.create_offer(None).await {
            Ok(offer) => offer,
            Err(reason) => {
                let _ = pc.close().await;
                abandon(
                    self,
                    place.call_id,
                    format!("could not describe the call: {reason}"),
                );
                return;
            }
        };
        if let Err(reason) = pc.set_local_description(offer.clone()).await {
            let _ = pc.close().await;
            abandon(
                self,
                place.call_id,
                format!("could not start the call's negotiation: {reason}"),
            );
            return;
        }
        let sealed = encode_sdp_description(&offer)
            .and_then(|bytes| seal_call_signal(&bytes, &place.key, &place.call_id));
        let sealed = match sealed {
            Ok(sealed) => sealed,
            Err(reason) => {
                let _ = pc.close().await;
                abandon(self, place.call_id, reason.to_string());
                return;
            }
        };
        let Some(my_device) = self.device_id() else {
            let _ = pc.close().await;
            return;
        };
        let invite = migo_protocol::CallInvite {
            call_id: place.call_id,
            conversation_id: place.conversation_id,
            callee_id: place.callee_id,
            media_kind: 0,
            caller_device: my_device,
            // Zero is the cross-client contract: no codec flags, no features, nothing a peer
            // could mistake for a promise this build does not keep.
            capabilities: 0,
            sealed_offer: sealed,
        };
        self.calls.active = Some(TrackedCall {
            call_id: place.call_id,
            peer: place.callee_id,
            is_caller: true,
            kind: place.kind,
            state: CallState::Ringing,
            end_reason: None,
            invite_status: None,
            started_at: None,
            setup_start: Timestamp::now(),
            stats_sent: false,
            muted: false,
            muted_flag,
            key: place.key,
            pc,
            microphone: Some(microphone),
            speaker: Some(speaker),
            video,
            peer_device: None,
            ice_batch: Vec::new(),
            ice_linger_armed: false,
            held_ice: Vec::new(),
            remote_description_set: false,
            end_sent: false,
            ended_at: None,
        });
        self.request(Opcode::CallInvite, &invite).await;
        self.calls.emit(&self.sink);
    }

    // --- answering ---

    /// Answers the ringing call: the key (waited for if it crossed badly), TURN, then the peer's
    /// offer applied and our answer sealed, relayed, and registered with the server.
    pub(super) async fn accept_call(&mut self) {
        let Some(ring) = self.calls.ringing.take() else {
            return;
        };
        let call_id = ring.call_id;
        let key = self.calls.keys.get(&call_id).copied();
        self.calls.answering = Some(Answering {
            call_id,
            caller_id: ring.caller_id,
            caller_device: ring.caller_device,
            kind: ring.kind,
            sealed_offer: ring.sealed_offer,
            key,
        });
        self.calls.emit(&self.sink);
        if key.is_some() {
            self.calls.pending_turn = Some(call_id);
            arm_later(&self.calls.tick_tx, CallTick::TurnTimeout, TURN_WAIT);
            let request = migo_protocol::CallTurnFetch { call_id };
            self.request(Opcode::CallTurnFetch, &request).await;
        } else {
            // The key crossed badly; the accept waits for it, bounded, rather than failing a
            // call the caller placed in the correct order.
            arm_later(
                &self.calls.tick_tx,
                CallTick::KeyWait { call_id },
                CALL_KEY_WAIT,
            );
        }
    }

    /// The accept's way of saying "occupied or unable, not unwilling" while it still can: the
    /// answer never reached the server, so as far as it knows this device is still ringing, and
    /// a Busy decline is what retires that ring honestly. The peer connection, when one was
    /// already built, is closed by the caller before this runs.
    async fn abort_answer_busy(&mut self, call_id: Id, reason: String) {
        self.calls.keys.remove(&call_id);
        let decline = migo_protocol::CallDecline {
            call_id,
            reason: CallDeclineReason::Busy.to_wire(),
        };
        self.request(Opcode::CallDecline, &decline).await;
        self.sink.toast(reason, ToastKind::Error);
        self.calls.emit(&self.sink);
    }

    /// The accept's second half: media, the offer applied, the answer out.
    async fn proceed_answer(&mut self, servers: Vec<RTCIceServer>) {
        let Some(answer) = self.calls.answering.take() else {
            return;
        };
        let Some(key) = answer.key else {
            return;
        };
        let (pc, microphone, speaker, muted_flag, video) =
            match self.build_media(answer.call_id, servers).await {
                Ok(media) => media,
                Err(reason) => {
                    self.abort_answer_busy(answer.call_id, reason).await;
                    return;
                }
            };
        // A video invite is answered with a receive-only video m-line: this build decodes and
        // renders the caller's picture but has no camera to send back. The transceiver must
        // exist before the answer is built — webrtc-rs matches every remote m-line to a local
        // transceiver (by MID, then by kind-and-direction) when assembling the answer, and a
        // video m-line with nothing to match fails the whole answer, audio and all. RecvOnly
        // is what satisfies a Sendrecv video offer without promising a track this build
        // cannot add.
        if answer.kind == CallMediaKind::Video {
            if let Err(reason) = pc
                .add_transceiver_from_kind(
                    RTPCodecType::Video,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Recvonly,
                        send_encodings: vec![],
                    }),
                )
                .await
            {
                let _ = pc.close().await;
                self.abort_answer_busy(
                    answer.call_id,
                    format!("could not answer the call's video: {reason}"),
                )
                .await;
                return;
            }
        }
        let offer = open_call_signal(&answer.sealed_offer, &key, &answer.call_id)
            .and_then(|bytes| decode_sdp_description(&bytes));
        let offer = match offer {
            Ok(offer) => offer,
            Err(reason) => {
                let _ = pc.close().await;
                self.abort_answer_busy(answer.call_id, reason.to_string())
                    .await;
                return;
            }
        };
        if let Err(reason) = pc.set_remote_description(offer).await {
            let _ = pc.close().await;
            self.abort_answer_busy(
                answer.call_id,
                format!("could not apply the caller's offer: {reason}"),
            )
            .await;
            return;
        }
        let description = match pc.create_answer(None).await {
            Ok(description) => description,
            Err(reason) => {
                let _ = pc.close().await;
                self.abort_answer_busy(
                    answer.call_id,
                    format!("could not answer the call: {reason}"),
                )
                .await;
                return;
            }
        };
        if let Err(reason) = pc.set_local_description(description.clone()).await {
            let _ = pc.close().await;
            self.abort_answer_busy(
                answer.call_id,
                format!("could not answer the call: {reason}"),
            )
            .await;
            return;
        }
        let sealed = encode_sdp_description(&description)
            .and_then(|bytes| seal_call_signal(&bytes, &key, &answer.call_id));
        let sealed = match sealed {
            Ok(sealed) => sealed,
            Err(reason) => {
                let _ = pc.close().await;
                self.abort_answer_busy(answer.call_id, reason.to_string())
                    .await;
                return;
            }
        };
        let Some(my_device) = self.device_id() else {
            let _ = pc.close().await;
            return;
        };
        // CALL_ANSWER tells the server the call is answered (it stamps this connection's own
        // device — the frame's claim is advisory); the answer itself reaches the caller's
        // stack through a CALL_SDP relay, which needs the caller's device id — the one fact
        // the invite event named and this path has held ever since.
        let answer_frame = migo_protocol::CallAnswer {
            call_id: answer.call_id,
            callee_device: my_device,
            sealed_answer: sealed.clone(),
        };
        self.request(Opcode::CallAnswer, &answer_frame).await;
        let relay = migo_protocol::CallSdp {
            call_id: answer.call_id,
            from_device: my_device,
            to_device: answer.caller_device,
            sealed_sdp: sealed,
        };
        self.request(Opcode::CallSdp, &relay).await;

        self.calls.active = Some(TrackedCall {
            call_id: answer.call_id,
            peer: answer.caller_id,
            is_caller: false,
            kind: answer.kind,
            state: CallState::Connecting,
            end_reason: None,
            invite_status: None,
            started_at: None,
            setup_start: Timestamp::now(),
            stats_sent: false,
            muted: false,
            muted_flag,
            key,
            pc,
            microphone: Some(microphone),
            speaker: Some(speaker),
            video,
            peer_device: Some(answer.caller_device),
            ice_batch: Vec::new(),
            ice_linger_armed: false,
            held_ice: Vec::new(),
            remote_description_set: true,
            end_sent: false,
            ended_at: None,
        });
        self.calls.emit(&self.sink);
    }

    // --- the UI's other buttons ---

    /// Declines the ringing call: the human's "no".
    pub(super) async fn decline_call(&mut self) {
        let Some(ring) = self.calls.ringing.take() else {
            return;
        };
        self.calls.keys.remove(&ring.call_id);
        let decline = migo_protocol::CallDecline {
            call_id: ring.call_id,
            reason: CallDeclineReason::Declined.to_wire(),
        };
        self.request(Opcode::CallDecline, &decline).await;
        self.calls.emit(&self.sink);
    }

    /// Hangs up: a call still ringing out is cancelled (the caller's pre-answer exit), an
    /// established call is ended with this side's reason, and a ring still showing is declined.
    pub(super) async fn end_call(&mut self) {
        if self.calls.ringing.is_some() {
            self.decline_call().await;
            return;
        }
        if let Some(place) = self.calls.placing.take() {
            // Nothing was invited yet; there is nothing to cancel on the wire.
            self.calls.keys.remove(&place.call_id);
            self.calls.emit(&self.sink);
            return;
        }
        if let Some(answer) = self.calls.answering.take() {
            // The answer never went out; retiring the accept retires the call.
            self.calls.keys.remove(&answer.call_id);
            self.calls.emit(&self.sink);
            return;
        }
        let decision = {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            if call.state == CallState::Ended || call.end_sent {
                return;
            }
            call.end_sent = true;
            let reason = if call.is_caller {
                CallEndReason::ByCaller
            } else {
                CallEndReason::ByCallee
            };
            (
                call.call_id,
                reason,
                call.is_caller && call.state == CallState::Ringing,
            )
        };
        let (call_id, reason, still_ringing) = decision;
        // Cancel is the caller's pre-answer exit; after the answer lands the same intent is
        // End — and the callee's hang-up of a connecting call is End too, because the answer
        // already told the server the call exists.
        if still_ringing {
            let cancel = migo_protocol::CallCancel { call_id };
            self.request(Opcode::CallCancel, &cancel).await;
        } else {
            let end = migo_protocol::CallEnd {
                call_id,
                reason: reason.to_wire(),
            };
            self.request(Opcode::CallEnd, &end).await;
        }
        self.finish_active(reason);
    }

    /// Mutes or unmutes this side's microphone: silence is substituted at the capture source,
    /// so the far end hears a quiet line, not a call that sounds hung up.
    pub(super) fn toggle_call_mute(&mut self) {
        let Some(call) = self.calls.active.as_mut() else {
            return;
        };
        if call.state == CallState::Ended {
            return;
        }
        call.muted = !call.muted;
        call.muted_flag.store(call.muted, Ordering::Relaxed);
        self.calls.emit(&self.sink);
    }

    /// Dismisses an ended call from the overlay: the screen that states how it went is owed a
    /// moment to be read, and only the human reading it closes it.
    pub(super) fn dismiss_call(&mut self) {
        let ended = self
            .calls
            .active
            .as_ref()
            .is_some_and(|call| call.state == CallState::Ended);
        if ended {
            self.calls.active = None;
            self.calls.emit(&self.sink);
        }
    }

    // --- the frame arms ---

    /// The caller's own invite answered: ringing means the callee is being rung and the expiry
    /// is the moment an unanswered invite ends itself; anything else means the call never rang,
    /// and the status says why without a human refusal having happened.
    pub(super) async fn on_call_invite_result(&mut self, frame: &migo_protocol::Frame) {
        let Ok(result) = super::gateway::decode::<migo_protocol::CallInviteResult>(frame) else {
            return;
        };
        let decision = {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            if call.call_id != result.call_id || call.state != CallState::Ringing {
                return;
            }
            if result.status == INVITE_RINGING {
                None
            } else {
                call.invite_status = Some(result.status);
                Some(invite_end_reason(result.status))
            }
        };
        match decision {
            None => arm_later(
                &self.calls.tick_tx,
                CallTick::RingExpiry {
                    call_id: result.call_id,
                },
                ring_timeout(result.expires_at, Timestamp::now()),
            ),
            Some(reason) => self.finish_active(reason),
        }
    }

    /// An invite from another account: ring us, answer Busy if this device is occupied, and
    /// ignore the redeliveries at-least-once delivery guarantees will bring.
    pub(super) async fn on_call_invite_event(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = super::gateway::decode::<migo_protocol::CallInviteEvent>(frame) else {
            return;
        };
        let disposition = incoming_invite_disposition(
            event.expires_at,
            event.call_id,
            self.calls.ringing.as_ref().map(|ring| ring.call_id),
            self.calls.active.as_ref().map(|call| call.call_id),
            self.calls.busy(),
            Timestamp::now(),
        );
        match disposition {
            call_signal::IncomingInviteDisposition::Ignore => {}
            call_signal::IncomingInviteDisposition::DeclineBusy => {
                let decline = migo_protocol::CallDecline {
                    call_id: event.call_id,
                    reason: CallDeclineReason::Busy.to_wire(),
                };
                self.request(Opcode::CallDecline, &decline).await;
            }
            call_signal::IncomingInviteDisposition::Ring => {
                arm_later(
                    &self.calls.tick_tx,
                    CallTick::RingExpiry {
                        call_id: event.call_id,
                    },
                    ring_timeout(event.expires_at, Timestamp::now()),
                );
                self.calls.ringing = Some(IncomingRing {
                    call_id: event.call_id,
                    caller_id: event.caller_id,
                    caller_device: event.caller_device,
                    kind: CallMediaKind::from_wire(event.media_kind),
                    sealed_offer: event.sealed_offer,
                });
                self.calls.emit(&self.sink);
            }
        }
    }

    /// An SDP relay: for a caller this is the answer naming the device everything now
    /// addresses; for a callee the offer arrived inside the invite, so an SDP relay that names
    /// this device and is not the answer is a renegotiation this build does not speak and
    /// ignores rather than guesses at.
    pub(super) async fn on_call_sdp(&mut self, frame: &migo_protocol::Frame) {
        let Ok(relay) = super::gateway::decode::<migo_protocol::CallSdp>(frame) else {
            return;
        };
        let Some(my_device) = self.device_id() else {
            return;
        };
        // A relay is addressed to one device; one sealed for another of this account's devices
        // is not ours to open.
        if relay.to_device != my_device {
            return;
        }
        let context = {
            let Some(call) = self.calls.active.as_ref() else {
                return;
            };
            if call.call_id != relay.call_id || !call.is_caller || call.remote_description_set {
                return;
            }
            (call.key, call.pc.clone(), call.state == CallState::Ringing)
        };
        let (key, pc, was_ringing) = context;
        let description = open_call_signal(&relay.sealed_sdp, &key, &relay.call_id)
            .and_then(|bytes| decode_sdp_description(&bytes));
        let Ok(description) = description else {
            self.finish_active(CallEndReason::Failed);
            return;
        };
        if description.sdp_type != RTCSdpType::Answer {
            return;
        }
        if pc.set_remote_description(description).await.is_err() {
            self.finish_active(CallEndReason::Failed);
            return;
        }
        let from_device = relay.from_device;
        {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            call.remote_description_set = true;
            call.peer_device = Some(from_device);
            if was_ringing {
                // The answer landed, so the invite's expiry has no call left to end: the local
                // mirror's tick will find a state it was not armed against and do nothing.
                call.state = CallState::Connecting;
            }
        }
        self.drain_held_ice().await;
        self.flush_ice().await;
        self.calls.emit(&self.sink);
    }

    /// A batch of the peer's candidates, applied now or held until the remote description
    /// exists. One malformed batch is dropped, not fatal: the next batch or the connection's
    /// own gathering carries the call.
    pub(super) async fn on_call_ice(&mut self, frame: &migo_protocol::Frame) {
        let Ok(relay) = super::gateway::decode::<migo_protocol::CallIce>(frame) else {
            return;
        };
        let Some(my_device) = self.device_id() else {
            return;
        };
        if relay.to_device != my_device {
            return;
        }
        let context = {
            let Some(call) = self.calls.active.as_ref() else {
                return;
            };
            if call.call_id != relay.call_id
                || call.state == CallState::Ended
                || !call.remote_description_set
            {
                return;
            }
            (call.key, call.pc.clone())
        };
        let (key, pc) = context;
        let batch = open_call_signal(&relay.sealed_candidates, &key, &relay.call_id)
            .and_then(|bytes| decode_ice_batch(&bytes));
        let Ok(batch) = batch else {
            return;
        };
        for candidate in batch {
            let Some(text) = candidate.candidate else {
                // The end-of-gathering sentinel: nothing to add, nothing left to wait for.
                continue;
            };
            let init = RTCIceCandidateInit {
                candidate: text,
                sdp_mid: candidate.sdp_mid,
                sdp_mline_index: Some(candidate.sdp_mline_index),
                username_fragment: candidate.username_fragment,
            };
            // A candidate the connection no longer wants is normal near the end of gathering;
            // the far end's own connectivity checks carry the call regardless.
            let _ = pc.add_ice_candidate(init).await;
        }
    }

    /// The server's authoritative state transitions: for the tracked call as a rule, plus the
    /// two events that can name a call this device never tracked — its ring answered by a
    /// sibling, or ended before anyone answered here.
    pub(super) async fn on_call_state(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = super::gateway::decode::<migo_protocol::CallStateEvent>(frame) else {
            return;
        };
        // A state a newer server added is not ours to guess at, and not ours to end a live call
        // over; the helpers below refuse the unknown the same way.
        if CallState::from_wire(event.state).is_none() {
            return;
        }
        let ringing_id = self.calls.ringing.as_ref().map(|ring| ring.call_id);
        if ends_ringing_call(event.state, ringing_id) {
            // The call this ring belongs to ended before anyone answered here — the caller
            // canceled, or the invite expired. Retire the ring and state the fact: a screen
            // that keeps ringing a dead call teaches its user to distrust every ring after it.
            let ring = self.calls.ringing.take();
            if let Some(ring) = ring {
                self.calls.keys.remove(&ring.call_id);
            }
            self.sink.toast(MISSED_CALL_MESSAGE, ToastKind::Info);
            self.calls.emit(&self.sink);
            return;
        }
        if answers_ringing_call(event.state, ringing_id) {
            // Another device on this account answered. The server rings every device and
            // publishes the Connecting state to both parties, so this one hears the call move
            // on without it — retire the ring and say where the call went, because "missed"
            // would be a lie about a call that connected.
            let ring = self.calls.ringing.take();
            if let Some(ring) = ring {
                self.calls.keys.remove(&ring.call_id);
            }
            self.sink.toast(ANSWERED_ELSEWHERE_MESSAGE, ToastKind::Info);
            self.calls.emit(&self.sink);
            return;
        }
        let action = {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            if call.call_id != event.call_id || call.state == CallState::Ended {
                return;
            }
            let Some(state) = CallState::from_wire(event.state) else {
                return;
            };
            match state {
                CallState::Ended => {
                    let reason = event.reason.and_then(CallEndReason::from_wire);
                    if let Some(reason) = reason {
                        call.end_reason = Some(reason);
                    }
                    Action::FinishLocal
                }
                CallState::Connected => Action::Connected,
                CallState::Ringing => Action::Nothing,
                CallState::Connecting | CallState::Reconnecting => {
                    call.state = state;
                    Action::Nothing
                }
            }
        };
        match action {
            Action::Nothing => {}
            Action::FinishLocal => self.finish_active_local(),
            Action::Connected => self.mark_connected().await,
            Action::ArmReconnect | Action::NetworkEnd => {}
        }
        self.calls.emit(&self.sink);
    }

    /// The TURN fetch's answer: the reply names no call of its own, so the pending id says
    /// which placement or accept it completes.
    pub(super) async fn on_call_turn(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = super::gateway::decode::<migo_protocol::CallTurnResponse>(frame) else {
            return;
        };
        let Some(id) = self.calls.pending_turn.take() else {
            return;
        };
        let servers = ice_servers_from(&response.servers);
        if self
            .calls
            .placing
            .as_ref()
            .is_some_and(|place| place.call_id == id)
        {
            self.proceed_placement(servers).await;
        } else if self
            .calls
            .answering
            .as_ref()
            .is_some_and(|answer| answer.call_id == id && answer.key.is_some())
        {
            self.proceed_answer(servers).await;
        }
    }

    // --- the tick arm ---

    /// One tick from a timer or a WebRTC callback, applied to the state it finds.
    pub(super) async fn on_call_tick(&mut self, tick: CallTick) {
        match tick {
            CallTick::Candidate { call_id, candidate } => match candidate {
                // Gathering finished: whatever is batched is all there will be.
                None => self.flush_ice().await,
                Some(candidate) => {
                    let arm = {
                        let Some(call) = self.calls.active.as_mut() else {
                            return;
                        };
                        if call.call_id != call_id || call.state == CallState::Ended {
                            return;
                        }
                        let Ok(init) = candidate.to_json() else {
                            return;
                        };
                        call.ice_batch.push(IceCandidateJson {
                            candidate: Some(init.candidate),
                            sdp_mid: init.sdp_mid,
                            sdp_mline_index: init.sdp_mline_index.unwrap_or(0),
                            username_fragment: init.username_fragment,
                        });
                        if call.ice_linger_armed {
                            false
                        } else {
                            call.ice_linger_armed = true;
                            true
                        }
                    };
                    if arm {
                        arm_later(
                            &self.calls.tick_tx,
                            CallTick::IceLinger { call_id },
                            ICE_LINGER,
                        );
                    }
                }
            },
            CallTick::IceLinger { call_id } => {
                let flush = {
                    let Some(call) = self.calls.active.as_mut() else {
                        return;
                    };
                    if call.call_id != call_id {
                        return;
                    }
                    call.ice_linger_armed = false;
                    call.state != CallState::Ended
                };
                if flush {
                    self.flush_ice().await;
                }
            }
            CallTick::Transport { call_id, state } => {
                let action = {
                    let Some(call) = self.calls.active.as_mut() else {
                        return;
                    };
                    if call.call_id != call_id || call.state == CallState::Ended {
                        return;
                    }
                    match state {
                        RTCPeerConnectionState::Connected => Action::Connected,
                        RTCPeerConnectionState::Disconnected => {
                            if call.state != CallState::Reconnecting {
                                call.state = CallState::Reconnecting;
                                Action::ArmReconnect
                            } else {
                                Action::Nothing
                            }
                        }
                        RTCPeerConnectionState::Failed => Action::NetworkEnd,
                        _ => Action::Nothing,
                    }
                };
                match action {
                    Action::Nothing => {}
                    Action::Connected => self.mark_connected().await,
                    Action::ArmReconnect => arm_later(
                        &self.calls.tick_tx,
                        CallTick::ReconnectWindow { call_id },
                        RECONNECT_WINDOW,
                    ),
                    Action::NetworkEnd => self.end_active_network().await,
                    Action::FinishLocal => {}
                }
                self.calls.emit(&self.sink);
            }
            CallTick::ReconnectWindow { call_id } => {
                let still_down = self.calls.active.as_ref().is_some_and(|call| {
                    call.call_id == call_id && call.state == CallState::Reconnecting
                });
                if still_down {
                    self.end_active_network().await;
                }
            }
            CallTick::RingExpiry { call_id } => {
                // The caller's mirror: the call still ringing ends here as NoAnswer, and a
                // cancel tells the server — which sweeps its own expiry regardless, but its
                // Ended can be late or lost from this screen's point of view.
                let caller_ringing = self.calls.active.as_ref().is_some_and(|call| {
                    call.call_id == call_id && call.state == CallState::Ringing
                });
                if caller_ringing {
                    let cancel = migo_protocol::CallCancel { call_id };
                    self.request(Opcode::CallCancel, &cancel).await;
                    self.finish_active(CallEndReason::NoAnswer);
                    return;
                }
                // The callee's mirror: a ring must never outlive the invite that backs it.
                let ringing = self
                    .calls
                    .ringing
                    .as_ref()
                    .is_some_and(|ring| ring.call_id == call_id);
                if ringing {
                    let ring = self.calls.ringing.take();
                    if let Some(ring) = ring {
                        self.calls.keys.remove(&ring.call_id);
                    }
                    self.sink.toast(MISSED_CALL_MESSAGE, ToastKind::Info);
                    self.calls.emit(&self.sink);
                }
            }
            CallTick::KeyWait { call_id } => {
                let still_waiting = self
                    .calls
                    .answering
                    .as_ref()
                    .is_some_and(|answer| answer.call_id == call_id && answer.key.is_none());
                if still_waiting {
                    // The key never arrived: give the caller their "no" and show the failure
                    // here — a ring that can never be picked up is worse than a decline. The
                    // answer never reached the server, so Busy is the honest reason.
                    self.calls.answering = None;
                    let decline = migo_protocol::CallDecline {
                        call_id,
                        reason: CallDeclineReason::Busy.to_wire(),
                    };
                    self.request(Opcode::CallDecline, &decline).await;
                    self.calls.keys.remove(&call_id);
                    self.sink
                        .toast("could not answer the call", ToastKind::Error);
                    self.calls.emit(&self.sink);
                }
            }
            CallTick::KeyArrived { call_id } => {
                let ready = self
                    .calls
                    .answering
                    .as_ref()
                    .is_some_and(|answer| answer.call_id == call_id && answer.key.is_some());
                if ready {
                    self.calls.pending_turn = Some(call_id);
                    arm_later(&self.calls.tick_tx, CallTick::TurnTimeout, TURN_WAIT);
                    let request = migo_protocol::CallTurnFetch { call_id };
                    self.request(Opcode::CallTurnFetch, &request).await;
                }
            }
            CallTick::TurnTimeout => {
                // The fetch did not answer in time; the call proceeds on the STUN fallback
                // alone — a call that must relay will fail to connect either way, but a call
                // that only needed STUN must not be refused over a list it can survive
                // without.
                let Some(id) = self.calls.pending_turn.take() else {
                    return;
                };
                let servers = ice_servers_from(&[]);
                if self
                    .calls
                    .placing
                    .as_ref()
                    .is_some_and(|place| place.call_id == id)
                {
                    self.proceed_placement(servers).await;
                } else if self
                    .calls
                    .answering
                    .as_ref()
                    .is_some_and(|answer| answer.call_id == id && answer.key.is_some())
                {
                    self.proceed_answer(servers).await;
                }
            }
        }
    }

    // --- the shared moves ---

    /// Sends the gathered candidate batch if it can be addressed and sealed; otherwise it stays
    /// queued — a caller's batch routinely outlives several linger ticks before the answer
    /// names the device it is for.
    async fn flush_ice(&mut self) {
        let Some(my_device) = self.device_id() else {
            return;
        };
        let sealed = {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            if call.state == CallState::Ended || call.ice_batch.is_empty() {
                return;
            }
            let Some(target) = call.peer_device else {
                return;
            };
            let batch = std::mem::take(&mut call.ice_batch);
            match encode_ice_batch(&batch)
                .and_then(|bytes| seal_call_signal(&bytes, &call.key, &call.call_id))
            {
                Ok(sealed) => Some((call.call_id, target, sealed)),
                // A batch that will not encode is dropped, not retried: the next batch or the
                // connection's own gathering carries the call.
                Err(_) => None,
            }
        };
        let Some((call_id, target, sealed)) = sealed else {
            return;
        };
        let relay = migo_protocol::CallIce {
            call_id,
            from_device: my_device,
            to_device: target,
            sealed_candidates: sealed,
        };
        self.request(Opcode::CallIce, &relay).await;
    }

    /// Applies the candidates the peer sent before this side's remote description existed.
    async fn drain_held_ice(&mut self) {
        let held = {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            if !call.remote_description_set {
                return;
            }
            (std::mem::take(&mut call.held_ice), call.pc.clone())
        };
        let (held, pc) = held;
        for init in held {
            let _ = pc.add_ice_candidate(init).await;
        }
    }

    /// Marks the call connected: the duration timer's zero is set exactly once, and the
    /// setup-time report goes out exactly once beside it. CALL_STATS is droppable — a lost
    /// report costs nothing — and carries only what this build measured, which is the setup
    /// time; the transport's own numbers are the WebRTC stack's to know, not ours to guess at.
    async fn mark_connected(&mut self) {
        let stats = {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            if call.state == CallState::Connected {
                return;
            }
            call.state = CallState::Connected;
            if call.started_at.is_none() {
                call.started_at = Some(Timestamp::now());
            }
            if call.stats_sent {
                None
            } else {
                call.stats_sent = true;
                let setup_ms = Timestamp::now().saturating_since(call.setup_start);
                Some(migo_protocol::CallStats {
                    call_id: call.call_id,
                    setup_ms: Some(setup_ms.min(u32::MAX as u64) as u32),
                    rtt_ms: None,
                    packet_loss: None,
                    jitter_ms: None,
                    used_turn: None,
                })
            }
        };
        if let Some(stats) = stats {
            self.request(Opcode::CallStats, &stats).await;
        }
        self.calls.emit(&self.sink);
    }

    /// Ends the tracked call locally with a reason, keeping `started_at` for the duration line.
    /// No frame is sent — the wire half, when there is one, is the callers' business.
    fn finish_active(&mut self, reason: CallEndReason) {
        let Some(call) = self.calls.active.as_mut() else {
            return;
        };
        if call.state == CallState::Ended {
            return;
        }
        call.state = CallState::Ended;
        call.end_reason = Some(reason);
        call.ended_at = Some(Timestamp::now());
        call.release_media();
        let id = call.call_id;
        self.calls.keys.remove(&id);
        self.calls.emit(&self.sink);
    }

    /// The state event's Ended: the reason arrived on the wire, and may be absent — an ended
    /// call with no reason is "Call ended", not a failure somebody invented.
    fn finish_active_local(&mut self) {
        let Some(call) = self.calls.active.as_mut() else {
            return;
        };
        if call.state == CallState::Ended {
            return;
        }
        call.state = CallState::Ended;
        call.ended_at = Some(Timestamp::now());
        call.release_media();
        let id = call.call_id;
        self.calls.keys.remove(&id);
        self.calls.emit(&self.sink);
    }

    /// A network death of the tracked call: end it here, and tell the peer when the gateway is
    /// still there to carry it — the peer otherwise waits out its whole reconnect window for a
    /// call this side already gave up on.
    async fn end_active_network(&mut self) {
        let end = {
            let Some(call) = self.calls.active.as_mut() else {
                return;
            };
            if call.state == CallState::Ended || call.end_sent {
                return;
            }
            call.end_sent = true;
            Some(migo_protocol::CallEnd {
                call_id: call.call_id,
                reason: CallEndReason::Network.to_wire(),
            })
        };
        if let Some(end) = end {
            self.request(Opcode::CallEnd, &end).await;
            self.finish_active(CallEndReason::Network);
        }
    }

    // --- the teardowns the session's own life demands ---

    /// The gateway died mid-call: there is no signalling left to end it with, so it ends here.
    /// A ring outlives the outage — the invite's expiry is the server's clock, not ours — and
    /// a placement or accept in flight is retired, because neither can complete offline.
    pub(super) fn calls_offline(&mut self) {
        self.calls.pending_turn = None;
        self.calls.placing = None;
        self.calls.answering = None;
        if let Some(call) = self.calls.active.as_mut() {
            if call.state != CallState::Ended {
                call.state = CallState::Ended;
                call.end_reason = Some(CallEndReason::Network);
                call.ended_at = Some(Timestamp::now());
                call.release_media();
                let id = call.call_id;
                self.calls.keys.remove(&id);
                self.calls.emit(&self.sink);
            }
        }
    }

    /// Sign-out: nothing of any call survives the session that held it.
    pub(super) fn calls_sign_out(&mut self) {
        self.calls.pending_turn = None;
        self.calls.placing = None;
        self.calls.answering = None;
        self.calls.ringing = None;
        self.calls.keys.clear();
        if let Some(call) = self.calls.active.as_mut() {
            if call.state != CallState::Ended {
                call.state = CallState::Ended;
                call.ended_at = Some(Timestamp::now());
                call.release_media();
            }
        }
        self.calls.active = None;
        self.calls.emit(&self.sink);
    }
}

/// What the state-event and transport handlers decided, so the borrow that read the call can
/// end before the action that mutates the engine runs.
enum Action {
    Nothing,
    Connected,
    ArmReconnect,
    NetworkEnd,
    FinishLocal,
}
