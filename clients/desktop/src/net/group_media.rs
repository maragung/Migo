//! The group call's media plane on the desktop: a mesh of peer connections, one per other seat.
//!
//! §163's seat and keys live in [`super::group_call`]; this is what the seat is *for*. The server
//! relays a sealed blob between two devices and nothing more — it has no mixer, no SFU, and no
//! interest in the media — so the shape that carries a group call's sound between N people is the
//! one every other Migo client builds: each device holds one peer connection to each other seat,
//! and the media goes directly between them, encrypted end to end by DTLS like a 1:1 call's.
//!
//! # What is shared between the links, and what is not
//!
//! **One microphone, one capture, N senders.** The microphone is a device, not a link: a second
//! capture for every seat would open the same hardware N times and send N copies of the same
//! sound. So the capture pump is spawned once, into one [`TrackLocalStaticSample`], and that one
//! track is added to every peer connection — webrtc-rs binds a per-connection RTP sender to the
//! same local track, and one `write_sample` fans out to all of them.
//!
//! **One speaker, one mixer.** This is the part that cannot be shared naively. A [`Speaker`]'s
//! `frames` channel is a FIFO drained by its own device thread: N playback pumps writing into it
//! would *serialize* the peers — the second seat's audio queued behind the first's, playing late
//! and never together — where a group call needs them summed. So each link's decoded audio goes
//! into its own bucket in [`Mixer`], and one 20 ms tick pulls one frame from every bucket, adds
//! them sample by sample, and hands the sum to the speaker. That is also where a peer that has
//! fallen behind is bounded: a bucket holding more than [`MIX_BACKLOG_FRAMES`] loses its oldest
//! samples, because latency in a call is worth less than completeness.
//!
//! **Video is received, never sent.** This build has no camera (see `call::proceed_answer`'s own
//! note), so every link carries a RecvOnly video m-line: it decodes and renders whatever the other
//! seat publishes, and publishes nothing back. The transceiver must exist before the description
//! is built — webrtc-rs matches every remote m-line to a local transceiver when assembling an
//! answer, and an unmatched video m-line fails the whole description, audio and all.
//!
//! # Glare, and why the loser rebuilds instead of rolling back
//!
//! Two devices can both believe they should dial when their rosters disagree for a moment — a
//! join announces itself to the conversation's topic and the two snapshots need not be identical
//! at the same instant. [`dialer_for`] settles it from the roster every device holds, and
//! [`keep_my_offer`] breaks the tie by device id when the rosters still disagree. The web client's
//! loser rolls its pending offer back and answers; **this crate cannot**, and the reason is worth
//! recording because it looks like it should: `RTCSessionDescription` has no `rollback()`
//! constructor, and although `set_local_description` does handle [`RTCSdpType::Rollback`], its
//! transition table accepts that type only from `have-remote-offer` — a *local* offer cannot be
//! rolled back at all (`check_next_signaling_state` returns
//! `ErrSignalingStateProposedTransitionInvalid` for `have-local-offer` + `SetLocal(rollback)`,
//! and `ErrSignalingStateCannotRollback` from `stable`). So the loser does the thing a rollback
//! is *for*: it closes the pending link and builds a fresh one as the answerer. A fresh peer
//! connection is `stable` with no pending description, which is exactly the state a rollback
//! would have restored — at the price of one ICE gathering round, on a tie that only happens
//! while two rosters disagree.
//!
//! # Sealing is not this module's business
//!
//! The mesh emits *plaintext* signalling ([`GroupOutbound`]) and the worker seals each frame under
//! the call's frame key before it goes on the wire — because the key lives with the seat, beside
//! this plane rather than inside it, and because a key rotation must be able to rebuild the links
//! without the plane ever holding key material. The inbound half is the same split: the worker
//! opens a relay under the frame key, and hands this plane the plaintext bytes or nothing at all.
//! A frame the key cannot open is not this plane's — it is a key ask or a join distribution, and
//! [`super::group_call::Worker::on_group_call_relay`] leaves it to the half that can read it.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use migo_core::Id;
use tokio::sync::mpsc;

use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_PCMU, MIME_TYPE_VP8};
use webrtc::api::{APIBuilder, API};
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

use super::call::{arm_later, spawn_capture_pump, CallTick};
use super::call_audio::{self, Resampler};
use super::call_signal::{
    decode_ice_batch, decode_sdp_description, encode_ice_batch, encode_sdp_description,
    IceCandidateJson,
};
use super::call_video::{self, VideoSlot};

/// How long gathered candidates linger before one relay carries them — the 1:1 engine's own
/// budget, for the same reason: a trickle leaves as few frames as it can, and the relay is
/// charged per frame.
const ICE_LINGER: Duration = Duration::from_millis(250);

/// The mixing tick's period. 20 ms is one frame at every rate a call uses: it is the packetisation
/// interval the capture side already packs to, and a longer tick would add latency to every
/// speaker for no gain in arithmetic.
const MIX_TICK: Duration = Duration::from_millis(20);

/// How much audio one peer may have waiting before the oldest samples are dropped: four frames is
/// 80 ms, past which a peer's sound is arriving late enough that losing its start is the better
/// trade. A bucket is a delay line, not a recording.
const MIX_BACKLOG_FRAMES: usize = 4;

/// The PCMU clock the wire speaks, and the payload type every client names for it.
const PCMU_PAYLOAD_TYPE: u8 = 0;

/// VP8's conventional dynamic payload type — the first slot, and the number every browser's VP8
/// offer names, so an answer's numbers match the offer's without a remap.
const VP8_PAYLOAD_TYPE: u8 = 96;

/// The video clock is always 90 kHz: timestamps are frame times, not sample counts.
const VIDEO_CLOCK_RATE: u32 = 90_000;

/// Which seated device dials a peer, from the roster alone: the *later* seat dials the earlier one.
///
/// This has to be computable by both sides from facts they already hold, or both dial and both
/// wait — the roster is that fact, and its order is the server's own join order, which every
/// seated device hears identically. The later joiner dials because it is the one that knows a
/// link is missing; the earlier seat is already busy with everyone who arrived before it.
///
/// A peer whose device is not in the roster is not dialed at all: a frame from a device this seat
/// has no projection for is a frame from a seat that has not been announced to us yet, and its
/// own arrival will bring the link.
pub(crate) fn dialer_for(seats: &[(Id, Id)], my_account: Id, peer_device: Id) -> bool {
    let Some(peer_index) = seats.iter().position(|(_, device)| *device == peer_device) else {
        return false;
    };
    let Some(my_index) = seats.iter().position(|(account, _)| *account == my_account) else {
        return false;
    };
    my_index > peer_index
}

/// Whether this device keeps its own offer when both sides dialed at once: the lower device id
/// wins. Arbitrary but *shared* — both sides read the same two ids and reach opposite verdicts,
/// which is the only property a glare rule needs.
pub(crate) fn keep_my_offer(my_device: Id, peer_device: Id) -> bool {
    my_device < peer_device
}

/// One signalling frame the mesh wants sent, before the worker seals it: which seat it is for,
/// and the plaintext the peer's own codec will read. The seal is applied above this module (see
/// the module docs) because the key belongs to the seat, not the plane.
#[derive(Debug, Clone)]
pub(crate) enum GroupOutbound {
    /// An offer or an answer, JSON-encoded the way every Migo client spells one.
    Sdp { to_device: Id, plaintext: Vec<u8> },
    /// A batch of gathered candidates, JSON-encoded the same way.
    Ice { to_device: Id, plaintext: Vec<u8> },
}

/// One async report from a link's callbacks, delivered on the worker's own tick channel so no
/// WebRTC callback ever touches engine state: the same discipline the 1:1 engine keeps, for the
/// same reason.
#[derive(Debug)]
pub(crate) enum GroupTick {
    /// One local candidate gathered, or `None` that gathering finished.
    Candidate {
        device: Id,
        candidate: Option<RTCIceCandidate>,
    },
    /// The link's transport moved.
    Transport {
        device: Id,
        state: RTCPeerConnectionState,
    },
    /// The ICE batch's linger elapsed: whatever is batched is all this trickle holds.
    IceLinger { device: Id },
}

/// One other seat's link, as the roster screen renders it: the peer, whether media is flowing,
/// and the slot its picture lands in — the read side the group-call screen stage will take.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct GroupLinkView {
    /// The peer account this link carries.
    pub user: Id,
    /// The peer device this link carries.
    pub device: Id,
    /// Whether the transport has reached `Connected`.
    pub connected: bool,
    /// Where the peer's decoded picture lands, if it publishes one. The overlay reads it on every
    /// repaint; a link whose peer sends no video simply never fills it.
    pub video: VideoSlot,
}

/// The summing mixer N links share: one bucket per peer, one tick that turns them into the single
/// stream a speaker can play. See the module docs for why a shared queue cannot do this job.
pub(crate) struct Mixer {
    /// One delay line per peer, at the speaker's rate. Keyed by device so a link's departure can
    /// take its bucket with it rather than leaving a silent line summed forever.
    buckets: Mutex<HashMap<Id, VecDeque<i16>>>,
    /// The rate the buckets hold, and the rate the speaker expects.
    rate: u32,
}

impl Mixer {
    fn new(rate: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            rate,
        }
    }

    /// How many samples one tick hands the speaker: one 20 ms frame at the buckets' rate.
    fn frame_len(&self) -> usize {
        (self.rate as usize / 50).max(1)
    }

    /// Adds one peer's freshly decoded audio to its own line. A lock held by a poisoned mutex is
    /// dropped rather than panicked on: a mixer nobody can write to is a call that has gone quiet,
    /// not a reason to take the process down with it.
    fn push(&self, device: Id, samples: Vec<i16>) {
        let Ok(mut buckets) = self.buckets.lock() else {
            return;
        };
        let bucket = buckets.entry(device).or_default();
        bucket.extend(samples);
        while bucket.len() > MIX_BACKLOG_FRAMES * self.frame_len() {
            bucket.pop_front();
        }
    }

    /// Forgets every line: the mesh is going away and its buckets would otherwise outlive it.
    fn clear(&self) {
        if let Ok(mut buckets) = self.buckets.lock() {
            buckets.clear();
        }
    }

    /// Forgets a departed peer's line.
    fn forget(&self, device: Id) {
        if let Ok(mut buckets) = self.buckets.lock() {
            buckets.remove(&device);
        }
    }

    /// One tick's worth of sound: every peer's next frame, summed.
    fn take_frame(&self) -> Vec<i16> {
        let frame = self.frame_len();
        match self.buckets.lock() {
            Ok(mut buckets) => mix_into(&mut buckets, frame),
            // Nothing can be read, so nothing is played — silence is the honest rendering.
            Err(_) => vec![0i16; frame],
        }
    }
}

/// One frame of the mix: pulls up to `frame` samples from every bucket and adds them together.
///
/// The addition saturates rather than wrapping. Two people talking at once on a loud link would
/// otherwise fold a peak into its own opposite — the one failure mode of summing that a listener
/// hears as a click, and the one a plain `+` on `i16` can produce. A bucket holding less than a
/// frame contributes what it has and is not padded: a peer whose audio has not arrived yet is
/// silent for the rest of this frame, not held back by it.
fn mix_into(buckets: &mut HashMap<Id, VecDeque<i16>>, frame: usize) -> Vec<i16> {
    let mut out = vec![0i16; frame];
    for bucket in buckets.values_mut() {
        let take = bucket.len().min(frame);
        for (slot, sample) in out.iter_mut().zip(bucket.drain(..take)) {
            *slot = slot.saturating_add(sample);
        }
    }
    out
}

/// One peer connection in the mesh, and what is fixed about it for the whole of its life.
struct GroupLink {
    /// The peer account, for the overlay's name lookup.
    user: Id,
    /// The peer device: the mesh's own map key, carried here so a view never has to re-derive it
    /// from the transport — which would be a different fact than the one the roster named.
    device: Id,
    /// The transport to that peer's device.
    pc: Arc<RTCPeerConnection>,
    /// Whether the offer left. Fixed at construction because the side that dials is the side that
    /// builds the link: an answer for a link that never offered is a frame for a link that already
    /// ended, and which of the two a link is does not change after the fact.
    dialed: bool,
    /// Where the peer's picture lands.
    video: VideoSlot,
    /// What a description or a candidate moves. Behind a lock because a link is shared by handle
    /// rather than owned: the map holds one `Arc` and [`GroupMesh::link_for`] hands the same one
    /// to a caller that must then await on it, and a value borrowed for that long cannot be
    /// mutated in place. The lock is never contended — every field under it is touched from the
    /// worker's single task — and it is never held across an await.
    negotiation: Mutex<Negotiation>,
}

/// The negotiation state one link carries between its descriptions.
#[derive(Default)]
struct Negotiation {
    /// Whether the peer's description has been applied. Until it has, this side's candidates are
    /// held rather than sent: webrtc-rs drops a candidate added with no remote description, and a
    /// batch sent then would be a frame the peer could not use.
    remote_set: bool,
    /// Whether a linger timer is pending for the batch, so one burst arms one timer.
    ice_linger_armed: bool,
    /// Candidates gathered before the peer's description existed, held for it: an ICE candidate
    /// cannot be added to a connection with no remote description, and dropping them would lose
    /// the only path that worked.
    held_ice: Vec<IceCandidateJson>,
}

impl GroupLink {
    /// The link's negotiation state.
    ///
    /// A poisoned lock gives its data back rather than panicking: nothing under these sections can
    /// panic, so a poisoned lock means some unrelated task panicked while holding it, and a call
    /// that went silent for that reason is a worse outcome than the panic itself.
    fn negotiation(&self) -> MutexGuard<'_, Negotiation> {
        self.negotiation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether the peer's description has landed, read without holding the lock.
    fn remote_set(&self) -> bool {
        self.negotiation().remote_set
    }
}

/// The mesh: every link this seat holds, the one microphone they share, and the mixer their audio
/// is summed in.
pub(crate) struct GroupMesh {
    /// This account — the one the roster's dialer rule reads.
    account_id: Id,
    /// This device — the one the glare rule reads, and the one a frame is addressed to.
    device_id: Id,
    /// The API every link is built from: one media engine, so every link speaks the same codecs.
    /// Not `Clone`, and not needed to be — `new_peer_connection` borrows it.
    api: API,
    /// The ICE servers every link gets: the group call's relays, then the STUN fallback. Held so
    /// a link rebuilt after a glare or a key rotation needs no second fetch.
    ice_servers: Vec<RTCIceServer>,
    /// The seat's roster, `(account, device)` in the server's join order, ourselves included.
    seats: Vec<(Id, Id)>,
    /// The links, by peer device.
    links: HashMap<Id, Arc<GroupLink>>,
    /// The one local audio track every link sends: one capture, N senders.
    mic_track: Arc<TrackLocalStaticSample>,
    /// What the links' audio is summed in.
    mixer: Arc<Mixer>,
    /// Whether this device is muted. Silence rather than absence, exactly as the 1:1 engine and
    /// the web client's disabled track do it: the peers hear a quiet line, not a call that hung up.
    muted: Arc<AtomicBool>,
    /// Kept for its `Drop`: dropping it stops capture, and it must outlive every link that sends
    /// the track it feeds.
    _microphone: call_audio::Microphone,
    /// Kept for the same reason: dropping it closes the output device.
    _speaker: call_audio::Speaker,
    /// Signalling the mesh wants sent, drained by the worker and sealed there.
    outbound: Vec<GroupOutbound>,
    /// Where the callbacks and timers report. Carries [`CallTick::Group`], so the mesh shares the
    /// 1:1 engine's one tick channel and its one select loop rather than racing it.
    tick_tx: mpsc::UnboundedSender<CallTick>,
}

impl GroupMesh {
    /// Opens the mesh: the media engine, the audio devices, the shared capture, and the mixing
    /// tick. No link exists yet — seats are dialed by [`GroupMesh::sync_seats`] as the roster
    /// arrives, so a joiner who lands alone holds a mesh and no connections.
    pub(crate) fn open(
        account_id: Id,
        device_id: Id,
        ice_servers: Vec<RTCIceServer>,
        tick_tx: mpsc::UnboundedSender<CallTick>,
    ) -> Result<Self, String> {
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
                    payload_type: PCMU_PAYLOAD_TYPE,
                    ..Default::default()
                },
                RTPCodecType::Audio,
            )
            .map_err(|error| format!("could not register the call's codec: {error}"))?;
        engine
            .register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        mime_type: MIME_TYPE_VP8.to_owned(),
                        clock_rate: VIDEO_CLOCK_RATE,
                        channels: 0,
                        ..Default::default()
                    },
                    payload_type: VP8_PAYLOAD_TYPE,
                    ..Default::default()
                },
                RTPCodecType::Video,
            )
            .map_err(|error| format!("could not register the call's video codec: {error}"))?;
        let api = APIBuilder::new().with_media_engine(engine).build();

        // The devices are opened before the API is used in anger, so a machine with no audio
        // fails before the seat is told it is in a call it cannot hear.
        let mut microphone = call_audio::open_microphone()
            .map_err(|error| format!("could not open the microphone: {error}"))?;
        let speaker = call_audio::open_speaker()
            .map_err(|error| format!("could not open the speaker: {error}"))?;

        let mic_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_PCMU.to_owned(),
                ..Default::default()
            },
            "audio".to_owned(),
            "migo".to_owned(),
        ));
        let muted = Arc::new(AtomicBool::new(false));

        // One sample writer between the blocking capture pump and the awaitable track. The track
        // fans every sample out to every link bound to it, so this task is the whole of the
        // outbound audio path for the mesh however many peers there are.
        let (sample_tx, mut sample_rx) = mpsc::unbounded_channel::<Sample>();
        let writer_track = mic_track.clone();
        tokio::spawn(async move {
            while let Some(sample) = sample_rx.recv().await {
                let _ = writer_track.write_sample(&sample).await;
            }
        });
        let mic_frames = microphone.take_frames();
        spawn_capture_pump(mic_frames, microphone.rate, muted.clone(), sample_tx);

        // The mixer, and the tick that empties it into the speaker. One task for the whole mesh:
        // the summing is what makes N peers one stream, and a per-link task would put them back
        // in the queue they came out of.
        let mixer = Arc::new(Mixer::new(speaker.rate));
        let speaker_tx = speaker.frames.clone();
        let tick_mixer = mixer.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(MIX_TICK);
            // A tick that arrives late is a tick to run now, not a backlog to work through: the
            // mixer is a live delay line, and catching up on missed ticks would play the past.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if speaker_tx.send(tick_mixer.take_frame()).is_err() {
                    return; // the output device is gone; the call is over
                }
            }
        });

        Ok(Self {
            account_id,
            device_id,
            api,
            ice_servers,
            seats: Vec::new(),
            links: HashMap::new(),
            mic_track,
            mixer,
            muted,
            _microphone: microphone,
            _speaker: speaker,
            outbound: Vec::new(),
            tick_tx,
        })
    }

    /// This device's mute state, for the roster screen's own button.
    ///
    /// Unreached until the group-call screen lands: the seat's UI today is a join/leave
    /// affordance and a participant count (see `ui::chat`), and the tiles, the per-seat mute, and
    /// the link phases are their own stage. These three accessors are that stage's read side, kept
    /// here with the state they read rather than invented there.
    #[allow(dead_code)]
    pub(crate) fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Sets the mute state. Silence, not absence — see the field's own note.
    #[allow(dead_code)]
    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// The links as the roster screen renders them, with the slot each peer's picture lands in.
    #[allow(dead_code)]
    pub(crate) fn views(&self) -> Vec<GroupLinkView> {
        self.links
            .values()
            .map(|link| GroupLinkView {
                user: link.user,
                device: link.device,
                connected: link.pc.connection_state() == RTCPeerConnectionState::Connected,
                video: link.video.clone(),
            })
            .collect()
    }

    /// Takes the signalling the mesh wants sent. The worker seals each frame under the call's
    /// frame key and sends it to the device it names.
    pub(crate) fn take_outbound(&mut self) -> Vec<GroupOutbound> {
        std::mem::take(&mut self.outbound)
    }

    /// Applies a roster: adopts the seats, drops the links to seats that left, and dials the seats
    /// this device owes an offer to.
    ///
    /// Called on every roster event, so it must be idempotent in the strong sense — a snapshot
    /// that names exactly the seats already linked moves nothing, and the same seat announced
    /// twice does not dial twice.
    pub(crate) async fn sync_seats(&mut self, seats: Vec<(Id, Id)>) {
        self.seats = seats;
        let wanted: Vec<(Id, Id)> = self
            .seats
            .iter()
            .copied()
            .filter(|(_, device)| *device != self.device_id)
            .collect();

        // Every seat that left loses its link and its bucket: a departed peer's connection would
        // otherwise stay open, and its delay line would go on being summed as silence.
        let gone: Vec<Id> = self
            .links
            .keys()
            .copied()
            .filter(|device| !wanted.iter().any(|(_, d)| d == device))
            .collect();
        for device in gone {
            self.close_link(device);
        }

        for (account, device) in wanted {
            if self.links.contains_key(&device) {
                continue;
            }
            if !dialer_for(&self.seats, self.account_id, device) {
                continue; // the earlier seat dials us; our offer would only glare
            }
            self.dial(account, device).await;
        }
    }

    /// The link an arriving description belongs to, built if the description is an offer for a
    /// seat this side holds no link to yet.
    ///
    /// The roster and the offer cross on the wire: a peer that joined a moment after our snapshot
    /// had no entry to dial, and its own offer is the first we hear of it. Dropping it would leave
    /// a link dead on both sides with no retry — so an offer from a *known* seat builds the link
    /// the roster would have built, exactly as the web client's `onSdp` does.
    ///
    /// # Glare
    ///
    /// Two devices can both dial when their rosters disagree for a moment, and the second offer to
    /// arrive finds a link with a local offer already outstanding — a state in which the crate's
    /// transition table refuses a remote offer outright (`have-local-offer` + `SetRemote(offer)`),
    /// so leaving it to the peer connection would deadlock the pair: each side waiting on an answer
    /// the other is waiting to be asked for. [`keep_my_offer`] settles it by device id, and the
    /// loser closes its pending link and builds a fresh one as the answerer — the thing a rollback
    /// is *for*, and the only spelling of it this crate allows (see the module docs).
    async fn link_for(&mut self, from_device: Id, is_offer: bool) -> Option<Arc<GroupLink>> {
        let glare = is_offer
            && !keep_my_offer(self.device_id, from_device)
            && self
                .links
                .get(&from_device)
                .is_some_and(|link| link.dialed && !link.remote_set());
        if glare {
            self.close_link(from_device);
        } else if let Some(link) = self.links.get(&from_device) {
            return Some(link.clone());
        }
        if !is_offer {
            // An answer for a link this side never offered — or one it just gave up — belongs to a
            // negotiation that is over, and there is nothing to build for it.
            return None;
        }
        let (account, _) = *self
            .seats
            .iter()
            .find(|(_, device)| *device == from_device)?;
        let link = self.new_link(account, from_device, false).await?;
        // `insert` hands back whatever it displaced, so the guard needs no second lookup and cannot
        // be written wrong: a link that landed while this one was being built is kept, and the new
        // one is closed. Two transports to one peer is two offers for one answer.
        if let Some(existing) = self.links.insert(from_device, link.clone()) {
            let _ = link.pc.close().await;
            return Some(existing);
        }
        Some(link)
    }

    /// Applies one peer's description, plaintext because the worker already opened it.
    ///
    /// Answers whether the frame was this mesh's to act on, so the worker can tell a media relay
    /// from a signalling frame it still owes to the seat's key paths.
    pub(crate) async fn on_remote_sdp(&mut self, from_device: Id, plaintext: &[u8]) -> bool {
        let Ok(description) = decode_sdp_description(plaintext) else {
            return false;
        };
        let is_offer = description.sdp_type == RTCSdpType::Offer;
        let Some(link) = self.link_for(from_device, is_offer).await else {
            return false;
        };
        match description.sdp_type {
            RTCSdpType::Offer => {
                // One description per link, and an offer is never the answer to one. A link this
                // side dialed is waiting for the peer's *answer*, so an offer arriving on it is
                // the other side of a glare — the loser of the tie, whose own offer we are
                // deliberately not answering. Dropping it is what makes the winner the winner;
                // the loser rebuilds when our offer reaches it. A renegotiated offer over a
                // settled link is a future flow, and applying one would answer a question nobody
                // asked.
                if link.remote_set() || link.dialed {
                    return true;
                }
                if link.pc.set_remote_description(description).await.is_err() {
                    return true; // the link stays connecting; a key change or a peer gives up
                }
                let Ok(answer) = link.pc.create_answer(None).await else {
                    return true;
                };
                if link.pc.set_local_description(answer.clone()).await.is_err() {
                    return true;
                }
                if let Ok(plaintext) = encode_sdp_description(&answer) {
                    self.outbound.push(GroupOutbound::Sdp {
                        to_device: from_device,
                        plaintext,
                    });
                }
                self.drain_held_ice(&link).await;
                self.flush_ice(from_device).await;
                true
            }
            RTCSdpType::Answer => {
                // An answer to an offer this side never sent is a frame for a link that already
                // ended, or one from a peer that dialed a link we are not the dialer of. Both are
                // no-ops rather than errors.
                if !link.dialed || link.remote_set() {
                    return true;
                }
                if link.pc.set_remote_description(description).await.is_err() {
                    return true;
                }
                self.drain_held_ice(&link).await;
                self.flush_ice(from_device).await;
                true
            }
            // A rollback or a pranswer is not a description this build produces or expects: the
            // peers are the same three clients, and none of them sends one.
            _ => true,
        }
    }

    /// Applies one peer's candidates, plaintext because the worker already opened the batch.
    ///
    /// Candidates that arrive before the peer's description are held, not dropped: an ICE
    /// candidate cannot be added to a connection with no remote description, and the candidate
    /// that arrives first is often the one that would have connected.
    pub(crate) async fn on_remote_ice(&mut self, from_device: Id, plaintext: &[u8]) {
        let Ok(batch) = decode_ice_batch(plaintext) else {
            return;
        };
        // The link is cloned out of the map so the awaits below borrow nothing of this mesh: the
        // worker's loop is the only thing running, and it must be free to take `&mut self` again
        // the moment these candidates are in.
        let Some(link) = self.links.get(&from_device).cloned() else {
            return;
        };
        {
            let mut negotiation = link.negotiation();
            if !negotiation.remote_set {
                // Held, not dropped, for the reason the function's own note gives.
                negotiation.held_ice.extend(batch);
                return;
            }
        }
        let pc = link.pc.clone();
        for candidate in batch {
            let _ = pc
                .add_ice_candidate(RTCIceCandidateInit {
                    candidate: candidate.candidate.unwrap_or_default(),
                    sdp_mid: candidate.sdp_mid,
                    sdp_mline_index: Some(candidate.sdp_mline_index),
                    username_fragment: candidate.username_fragment,
                })
                .await;
        }
    }

    /// One callback or timer report, applied to the link it names. A tick for a device this mesh
    /// no longer links is dropped: the link it was armed against is gone.
    pub(crate) async fn on_tick(&mut self, tick: GroupTick) {
        match tick {
            GroupTick::Candidate { device, candidate } => match candidate {
                // Gathering finished: whatever is batched is all there will be.
                None => self.flush_ice(device).await,
                Some(candidate) => {
                    // A candidate that will not convert is dropped, not guessed at: the next one,
                    // or the connection's own gathering, carries the link.
                    let Ok(init) = candidate.to_json() else {
                        return;
                    };
                    let arm = match self.links.get(&device) {
                        Some(link) => {
                            let mut negotiation = link.negotiation();
                            negotiation.held_ice.push(IceCandidateJson {
                                candidate: Some(init.candidate),
                                sdp_mid: init.sdp_mid,
                                sdp_mline_index: init.sdp_mline_index.unwrap_or(0),
                                username_fragment: init.username_fragment,
                            });
                            if negotiation.ice_linger_armed {
                                false
                            } else {
                                negotiation.ice_linger_armed = true;
                                true
                            }
                        }
                        None => return,
                    };
                    // The first candidate of a burst arms the linger; the rest ride it, so the
                    // batch leaves as one frame — which is what the relay is charged for.
                    if arm {
                        arm_later(
                            &self.tick_tx,
                            CallTick::Group(GroupTick::IceLinger { device }),
                            ICE_LINGER,
                        );
                    }
                }
            },
            GroupTick::IceLinger { device } => self.flush_ice(device).await,
            GroupTick::Transport { device, state } => {
                if state == RTCPeerConnectionState::Failed
                    || state == RTCPeerConnectionState::Closed
                {
                    // A failed link is closed and rebuilt on the next roster sync rather than
                    // left half-open: a transport that failed will not come back on its own, and
                    // the seat is still in the call.
                    self.close_link(device);
                }
            }
        }
    }

    /// Sends whatever is batched for one link as one relay.
    ///
    /// Nothing leaves without a remote description: webrtc-rs drops a candidate added before one,
    /// and a batch sent then would be a frame the peer could not use. The batch waits in
    /// `held_ice` and leaves with the first flush after the description lands instead.
    async fn flush_ice(&mut self, device: Id) {
        let Some(link) = self.links.get(&device).cloned() else {
            return;
        };
        let batch = {
            let mut negotiation = link.negotiation();
            if !negotiation.remote_set || negotiation.held_ice.is_empty() {
                return;
            }
            negotiation.ice_linger_armed = false;
            std::mem::take(&mut negotiation.held_ice)
        };
        // A batch that will not encode is dropped, not retried: the next candidate or the
        // connection's own gathering carries the link.
        if let Ok(plaintext) = encode_ice_batch(&batch) {
            self.outbound.push(GroupOutbound::Ice {
                to_device: device,
                plaintext,
            });
        }
    }

    /// Applies the candidates held while the peer's description was missing.
    async fn drain_held_ice(&mut self, link: &Arc<GroupLink>) {
        // The flag flips first and unconditionally, because it is what `flush_ice` gates this
        // side's own candidates on: a description that arrived with no candidate held for it yet
        // would otherwise leave our batch waiting on a flag that never moved, and the link
        // one-way — heard, but not hearing.
        let batch = {
            let mut negotiation = link.negotiation();
            negotiation.remote_set = true;
            std::mem::take(&mut negotiation.held_ice)
        };
        for candidate in batch {
            let _ = link
                .pc
                .add_ice_candidate(RTCIceCandidateInit {
                    candidate: candidate.candidate.unwrap_or_default(),
                    sdp_mid: candidate.sdp_mid,
                    sdp_mline_index: Some(candidate.sdp_mline_index),
                    username_fragment: candidate.username_fragment,
                })
                .await;
        }
    }

    /// Builds a link and, when this side is the dialer, sends its offer.
    async fn dial(&mut self, account: Id, device: Id) {
        let Some(link) = self.new_link(account, device, true).await else {
            return;
        };
        self.links.insert(device, link.clone());
        let Ok(offer) = link.pc.create_offer(None).await else {
            self.close_link(device);
            return;
        };
        if link.pc.set_local_description(offer.clone()).await.is_err() {
            self.close_link(device);
            return;
        }
        if let Ok(plaintext) = encode_sdp_description(&offer) {
            self.outbound.push(GroupOutbound::Sdp {
                to_device: device,
                plaintext,
            });
        }
    }

    /// Builds one peer connection: the audio track every link sends, a RecvOnly video m-line for
    /// the picture it receives, the pumps, and the callbacks that report as ticks.
    ///
    /// The link is *not* inserted here — the caller decides whether it won the race — so a link
    /// that loses is closed without ever having been reachable. `dialed` says which side of the
    /// negotiation this link is: the dialer builds it and then sends the offer, the answerer
    /// builds it in reply to one, and nothing about the link changes that after the fact.
    async fn new_link(&mut self, account: Id, device: Id, dialed: bool) -> Option<Arc<GroupLink>> {
        let pc = self
            .api
            .new_peer_connection(RTCConfiguration {
                ice_servers: self.ice_servers.clone(),
                ..Default::default()
            })
            .await
            .ok()?;
        let pc = Arc::new(pc);

        // The one shared microphone track, added to this link. The track is the same Arc every
        // link holds: the capture writes once and every sender forwards it.
        if pc.add_track(self.mic_track.clone()).await.is_err() {
            let _ = pc.close().await;
            return None;
        }
        // The video m-line, receive-only: this build has no camera, and the peer needs a
        // transceiver to match before any of its descriptions will assemble.
        if pc
            .add_transceiver_from_kind(
                RTPCodecType::Video,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await
            .is_err()
        {
            let _ = pc.close().await;
            return None;
        }

        let video: VideoSlot = Arc::default();
        let link = Arc::new(GroupLink {
            user: account,
            device,
            pc: pc.clone(),
            dialed,
            video: video.clone(),
            negotiation: Mutex::new(Negotiation::default()),
        });

        let tick_tx = self.tick_tx.clone();
        pc.on_ice_candidate(Box::new(move |candidate| {
            let tick_tx = tick_tx.clone();
            Box::pin(async move {
                let _ = tick_tx.send(CallTick::Group(GroupTick::Candidate { device, candidate }));
            })
        }));
        let tick_tx = self.tick_tx.clone();
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let tick_tx = tick_tx.clone();
            Box::pin(async move {
                let _ = tick_tx.send(CallTick::Group(GroupTick::Transport { device, state }));
            })
        }));

        // The peer's tracks: audio into the mixer's own bucket for this device, video into this
        // link's slot. The peer connection crosses into the video pump by weak reference, not a
        // clone — the pump outlives the link's handlers, and a strong reference held by a handler
        // the closed connection never clears would keep a dead transport alive.
        let mixer = self.mixer.clone();
        let rate = self.mixer.rate;
        let video_pc = Arc::downgrade(&pc);
        pc.on_track(Box::new(move |track, _receiver, _transceiver| {
            let mixer = mixer.clone();
            let video = video.clone();
            let video_pc = video_pc.clone();
            Box::pin(async move {
                if track.kind() == RTPCodecType::Video {
                    tokio::spawn(async move {
                        let media_ssrc = track.ssrc();
                        let pli = move || {
                            let request = PictureLossIndication {
                                sender_ssrc: media_ssrc,
                                media_ssrc,
                            };
                            if let Some(pc) = video_pc.upgrade() {
                                tokio::spawn(async move {
                                    let packet: Box<
                                        dyn webrtc::rtcp::packet::Packet + Send + Sync,
                                    > = Box::new(request);
                                    let _ = pc.write_rtcp(&[packet]).await;
                                });
                            }
                        };
                        call_video::video_pump(track, video, Arc::new(pli)).await;
                    });
                } else {
                    tokio::spawn(async move {
                        link_audio_pump(track, mixer, device, rate).await;
                    });
                }
            })
        }));

        Some(link)
    }

    /// Closes one link and forgets it, bucket and all. Idempotent: a departure, a failed
    /// transport, and a glare rebuild can all name the same device, and the second one finds
    /// nothing to do.
    ///
    /// Synchronous on purpose: the only await is the close, which is spawned because the worker's
    /// answer to the user must not wait on it — and the seat-teardown paths that call this (an
    /// offline gateway, a sign-out, a conversation's teardown) are synchronous themselves and
    /// must not have to become async to end a call.
    pub(crate) fn close_link(&mut self, device: Id) {
        if let Some(link) = self.links.remove(&device) {
            let pc = link.pc.clone();
            tokio::spawn(async move {
                let _ = pc.close().await;
            });
        }
        self.mixer.forget(device);
    }

    /// Closes every link and stops the media. Called when the seat ends, whatever ended it.
    pub(crate) fn shutdown(&mut self) {
        let links: Vec<Arc<GroupLink>> = self.links.drain().map(|(_, link)| link).collect();
        for link in links {
            let pc = link.pc.clone();
            tokio::spawn(async move {
                let _ = pc.close().await;
            });
        }
        self.mixer.clear();
    }

    /// Forgets the links that have not connected, so the roster's next sync rebuilds them.
    ///
    /// Called when the frame key's epoch advances. A description already in flight was sealed at
    /// the epoch it left under, and the epoch is the AEAD's associated data — so a link whose
    /// negotiation never finished is exactly the link whose description will now fail to open, and
    /// rebuilding it is the only way it ever connects. A *connected* link is untouched: the frame
    /// key seals signalling, not media, and a rotation does not renegotiate a link already carrying
    /// sound.
    ///
    /// The known limit, shared with the web client's own reset: if only one side's link was
    /// unconnected, only that side rebuilds, and the dialer rule means the rebuilt side is the one
    /// that waits. The next roster movement — any join or departure — re-dials it, and a roster
    /// that never moves again is a call where every remaining link is already up.
    pub(crate) async fn reset_unconnected(&mut self) {
        let stale: Vec<Id> = self
            .links
            .iter()
            .filter(|(_, link)| link.pc.connection_state() != RTCPeerConnectionState::Connected)
            .map(|(device, _)| *device)
            .collect();
        for device in stale {
            self.close_link(device);
        }
    }
}

/// Feeds one peer's audio into the mixer's bucket for it: every packet that peer sent, decoded and
/// resampled to whatever rate this device's output runs at.
///
/// A read that fails is the track closing — the peer left, or the link died — and the pump simply
/// stops. Unlike the 1:1 engine's pump, a stopped link does not end a call: the other seats are
/// still there, which is the whole difference between a mesh and a call.
async fn link_audio_pump(track: Arc<TrackRemote>, mixer: Arc<Mixer>, device: Id, rate: u32) {
    // Only resample when the device rate differs from the wire's, exactly as the capture side
    // reasons: a same-rate stream through a linear interpolator comes out measurably worse.
    let mut resampler = (rate != call_audio::CALL_SAMPLE_RATE)
        .then(|| Resampler::new(call_audio::CALL_SAMPLE_RATE, rate));
    loop {
        let Ok((packet, _)) = track.read_rtp().await else {
            return;
        };
        let linear = call_audio::ulaw_decode(&packet.payload);
        match resampler.as_mut() {
            Some(resampler) => {
                let mut out = Vec::with_capacity(linear.len() * 2);
                resampler.process(&linear, &mut out);
                mixer.push(device, out);
            }
            None => mixer.push(device, linear),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> Id {
        let mut bytes = [0u8; migo_core::id::ID_BYTE_LEN];
        bytes[0] = byte;
        Id::from_bytes(bytes)
    }

    #[test]
    fn the_later_seat_dials_the_earlier_one() {
        let seats = vec![(id(1), id(11)), (id(2), id(12)), (id(3), id(13))];
        // The last joiner dials both the seats before it, and neither of them dials it.
        assert!(dialer_for(&seats, id(3), id(11)));
        assert!(dialer_for(&seats, id(3), id(12)));
        assert!(!dialer_for(&seats, id(1), id(12)));
        assert!(!dialer_for(&seats, id(2), id(13)));
    }

    #[test]
    fn exactly_one_side_of_a_pair_dials() {
        let seats = vec![(id(1), id(11)), (id(2), id(12))];
        let first = dialer_for(&seats, id(1), id(12));
        let second = dialer_for(&seats, id(2), id(11));
        assert_ne!(
            first, second,
            "both sides dialing is the glare the rule exists to prevent"
        );
    }

    #[test]
    fn a_seat_that_is_not_on_the_roster_is_not_dialed() {
        let seats = vec![(id(1), id(11)), (id(2), id(12))];
        assert!(!dialer_for(&seats, id(2), id(99)));
        // And a device whose own seat is missing has no index to compare against.
        assert!(!dialer_for(&seats, id(9), id(11)));
    }

    #[test]
    fn the_glare_rule_gives_the_two_sides_opposite_verdicts() {
        let mine = id(5);
        let theirs = id(9);
        assert_ne!(
            keep_my_offer(mine, theirs),
            keep_my_offer(theirs, mine),
            "a tie rule one side reads the same way as the other settles nothing"
        );
        assert!(keep_my_offer(mine, theirs));
    }

    #[test]
    fn the_mix_sums_the_peers_rather_than_serialising_them() {
        let mut buckets: HashMap<Id, VecDeque<i16>> = HashMap::new();
        buckets.insert(id(1), VecDeque::from(vec![100i16, -100, 0, 5]));
        buckets.insert(id(2), VecDeque::from(vec![50i16, 50, 50, 50]));
        let frame = mix_into(&mut buckets, 4);
        assert_eq!(frame, vec![150i16, -50, 50, 55]);
        // Every bucket is drained by exactly one frame, so the next one is silence.
        assert_eq!(mix_into(&mut buckets, 4), vec![0i16; 4]);
    }

    #[test]
    fn a_loud_pair_saturates_instead_of_wrapping() {
        let mut buckets: HashMap<Id, VecDeque<i16>> = HashMap::new();
        buckets.insert(id(1), VecDeque::from(vec![i16::MAX, i16::MIN]));
        buckets.insert(id(2), VecDeque::from(vec![i16::MAX, i16::MIN]));
        assert_eq!(mix_into(&mut buckets, 2), vec![i16::MAX, i16::MIN]);
    }

    #[test]
    fn a_silent_peer_does_not_hold_the_frame_back() {
        let mut buckets: HashMap<Id, VecDeque<i16>> = HashMap::new();
        buckets.insert(id(1), VecDeque::from(vec![7i16]));
        buckets.insert(id(2), VecDeque::new());
        // A bucket with less than a frame contributes what it has and is not padded with a wait.
        assert_eq!(mix_into(&mut buckets, 3), vec![7i16, 0, 0]);
    }

    #[test]
    fn the_backlog_bound_drops_the_oldest_not_the_newest() {
        let mixer = Mixer::new(8_000);
        let frame = mixer.frame_len();
        assert_eq!(frame, 160);
        // A peer that has run ahead loses the start of its queue — latency, not the live edge.
        mixer.push(id(1), vec![1i16; MIX_BACKLOG_FRAMES * frame + 10]);
        let taken = mixer.take_frame();
        assert_eq!(taken.len(), frame);
        assert_eq!(taken[0], 1);
        // What is left is bounded by the backlog, so the queue cannot grow without limit.
        for _ in 0..MIX_BACKLOG_FRAMES {
            let _ = mixer.take_frame();
        }
        assert_eq!(mixer.take_frame(), vec![0i16; frame]);
    }

    #[test]
    fn a_departed_peer_is_forgotten() {
        let mixer = Mixer::new(8_000);
        mixer.push(id(1), vec![9i16; 160]);
        mixer.forget(id(1));
        assert_eq!(mixer.take_frame(), vec![0i16; 160]);
    }
}
