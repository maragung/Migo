//! The sessions the media plane is serving, and the frames it moves between them.
//!
//! # Two layers, one authority each
//!
//! [`migo_sfu::Sfu`] is the decision core: who is seated, who publishes what, who subscribed to
//! whom, which layer each subscriber gets. It is pure, deterministic and clockless, and it has no
//! opinion about sockets. This module is everything that needs a socket to mean anything: which
//! *connection* holds a seat, where a delivery goes, and what happens when a subscriber cannot
//! keep up. The core decides, this module delivers, and the seam between them is
//! [`Sfu::forward`](migo_sfu::Sfu::forward)'s `Vec<Delivery>`.
//!
//! # Why a seat is keyed by device and not by connection
//!
//! A seat belongs to an account on a device (section 165). Two connections from the same device
//! are one participant, and the second must take the seat rather than sit beside it — otherwise
//! one person holds two seats and receives every frame twice. So the map is keyed by
//! `(call, device)`, and a new session for a key replaces the one already there.
//!
//! That replacement is what makes the session counter load-bearing. A replaced connection's own
//! task is still running and will, sooner or later, notice its socket died and try to clean up —
//! and an unguarded cleanup would retire the seat its *replacement* is using. Every session
//! therefore carries a [`SessionId`] minted from one monotonic counter, and a teardown removes
//! the map's entry only when the entry still holds that same id. A stale disconnect then removes
//! nothing, which is the correct outcome: the seat it would have retired is somebody else's now.
//!
//! # What a slow subscriber costs
//!
//! Each session owns a bounded channel. A delivery is offered with `try_send`, and when the queue
//! is full the frame is dropped and counted rather than awaited or unbounded: a subscriber whose
//! link has collapsed must cost this process a fixed amount of memory, and a growing backlog
//! behind a dead peer is how one bad link becomes everybody's outage. The dropped frames are
//! visible in `/metrics` — the client's own adaptation (section 165) is what turns that loss into
//! a lower rung, and this module's job is to make the loss observable rather than silent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use migo_core::config::SfuNodeConfig;
use migo_core::metrics::{Counter, Gauge, Registry};
use migo_core::{Error, Id, Timestamp};
use migo_protocol::{fault, BandwidthMode};
use migo_sfu::{
    Adaptation, InboundFrame, JoinOutcome, LinkStats, Member, PublishRequest, Sfu, SfuConfig,
    StreamKind,
};

use crate::wire::{encode_delivery_datagram, encode_reply, LinkNumbers, Reply, Request, SeatWire};

/// A session's own identity, from one monotonic counter.
///
/// Opaque on purpose: nothing outside this module may compare two of them and conclude anything,
/// because the only question they answer is *is the session I am cleaning up the one still in the
/// map*, which only the map can be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(u64);

/// What a session's writer task is handed.
///
/// One channel for both kinds of outbound bytes, so their order is the order the connection
/// observes: a reply to a subscribe can never overtake the media the subscribe just started.
#[derive(Clone, Debug)]
pub enum Outbound {
    /// One control frame, to be written length-prefixed on the control stream.
    Control(Bytes),
    /// One media datagram, to be sent as it is.
    Media(Bytes),
}

/// One seated connection.
#[derive(Clone, Debug)]
pub struct Session {
    /// This session's identity, for the teardown guard.
    pub id: SessionId,
    /// The call it is seated in.
    pub call_id: Id,
    /// The account and device holding the seat.
    pub member: Member,
    /// When the ticket that admitted it expires.
    pub expires_at: Timestamp,
    tx: mpsc::Sender<Outbound>,
}

impl Session {
    /// Offers one outbound item, dropping it when the session's queue is full.
    ///
    /// Returns true when the item was dropped. A closed receiver is not a drop: the session's own
    /// task is on its way out and its teardown is about to retire the seat, so the item is not
    /// lost to congestion but to a disconnect the map is about to notice anyway.
    fn offer(&self, item: Outbound) -> bool {
        matches!(self.tx.try_send(item), Err(TrySendError::Full(_)))
    }
}

/// The node's media plane: the decision core plus the sessions it delivers to.
pub struct Plane {
    sfu: Sfu,
    sessions: Mutex<HashMap<(Id, Id), Session>>,
    next_session: AtomicU64,
    queue: usize,
    meters: Meters,
}

impl Plane {
    /// Builds the plane, refusing a configuration the node could not serve.
    ///
    /// The product limits come from the section the operator wrote and fall back to the plane's
    /// own constants, so there is exactly one default per limit in the workspace and it is the
    /// one the crate that enforces it declares.
    ///
    /// # Errors
    ///
    /// Whatever [`Sfu::new`] refuses: a limit that could not hold, i.e. a deployment error.
    pub fn new(config: &SfuNodeConfig, registry: &Registry) -> Result<Self, Error> {
        let mut product = SfuConfig::default();
        if let Some(max) = config.max_audio_participants {
            product.max_audio_participants = max;
        }
        if let Some(max) = config.max_active_video_streams {
            product.max_active_video_streams = max;
        }
        Ok(Self {
            sfu: Sfu::new(product, registry)?,
            sessions: Mutex::new(HashMap::new()),
            next_session: AtomicU64::new(1),
            queue: config.outbound_queue,
            meters: Meters::new(registry),
        })
    }

    /// The channel capacity a session gets, for the process's startup line.
    #[must_use]
    pub fn queue_capacity(&self) -> usize {
        self.queue
    }

    /// Seats a device and hands back the session that will carry its deliveries, with the
    /// channel the process's writer task drains.
    ///
    /// The seat is taken in the decision core first: a join the core refuses (a full call) never
    /// reaches the map, so a refused connection holds no memory and no seat. On success the
    /// session replaces any earlier one for the same device, and the other seats of the call are
    /// told a participant arrived — but only when the seat is genuinely new, because a reconnect
    /// that the core answered `Rejoined` is a seat everybody already knows about.
    ///
    /// # Errors
    ///
    /// Whatever the core refuses: the call is at its seat ceiling.
    pub fn seat(
        &self,
        call_id: Id,
        member: Member,
        mode: BandwidthMode,
        expires_at: Timestamp,
        now: Timestamp,
    ) -> Result<(Session, mpsc::Receiver<Outbound>), Error> {
        let outcome = self.sfu.join(call_id, member, mode, now)?;
        let id = SessionId(self.next_session.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel(self.queue);
        let session = Session {
            id,
            call_id,
            member,
            expires_at,
            tx,
        };
        {
            let mut sessions = self.sessions.lock();
            sessions.insert((call_id, member.device_id), session.clone());
            self.meters
                .sessions
                .set(i64::try_from(sessions.len()).unwrap_or(i64::MAX));
        }
        self.meters.admitted.inc();
        if outcome == JoinOutcome::Joined {
            self.announce(call_id, member, true);
        }
        Ok((session, rx))
    }

    /// Retires a session's seat, if the map still holds that session.
    ///
    /// A teardown from a connection that has already been replaced retires nothing: the seat it
    /// would have vacated belongs to its replacement. The decision core is told either way —
    /// `leave` is idempotent and answers `Absent` for a seat that is not there — so a stale
    /// teardown can never leave the core and the map disagreeing about who is seated.
    pub fn close(&self, session: &Session) {
        let removed = {
            let mut sessions = self.sessions.lock();
            match sessions.get(&(session.call_id, session.member.device_id)) {
                Some(current) if current.id == session.id => {
                    sessions.remove(&(session.call_id, session.member.device_id));
                    self.meters
                        .sessions
                        .set(i64::try_from(sessions.len()).unwrap_or(i64::MAX));
                    true
                }
                _ => false,
            }
        };
        if !removed {
            return;
        }
        if self.sfu.leave(session.call_id, session.member).is_ok() {
            self.announce(session.call_id, session.member, false);
        }
    }

    /// Moves one sealed frame from a publisher to every subscriber of its stream.
    ///
    /// Returns how many deliveries were offered to a session, which is not how many arrived: a
    /// session whose queue was full had its frame dropped and counted. The publisher is never a
    /// recipient — the core's own rule, and the one place it is enforced.
    ///
    /// # Errors
    ///
    /// Whatever the core refuses: the call, the seat or the stream is not there, or the frame
    /// claims a layer its stream does not offer.
    pub fn forward(&self, call_id: Id, from: Member, frame: InboundFrame) -> Result<usize, Error> {
        let deliveries = self.sfu.forward(call_id, from, frame)?;
        let sessions = self.sessions.lock();
        let mut offered = 0;
        for delivery in deliveries {
            let bytes = delivery.frame.into_bytes();
            let datagram = encode_delivery_datagram(
                delivery.publisher,
                delivery.stream_id,
                delivery.sequence,
                delivery.layer,
                &bytes,
            );
            let len = datagram.len() as u64;
            offered += 1;
            if let Some(session) = sessions.get(&(call_id, delivery.to.device_id)) {
                if session.offer(Outbound::Media(datagram)) {
                    self.meters.dropped_media.inc();
                    self.meters.media_bytes_dropped.add(len);
                } else {
                    self.meters.media_bytes.add(len);
                }
            } else {
                // The core has a seat the map does not. That is a race between a teardown and an
                // in-flight frame, not a protocol error: the frame has nowhere to go and is
                // counted rather than invented a recipient for.
                self.meters.undeliverable.inc();
            }
        }
        self.meters.media_frames.add(offered as u64);
        Ok(offered)
    }

    /// Answers one control request from a seated device.
    ///
    /// `Join` is answered here as a refusal: the connection was admitted before it had a session,
    /// so a second join is a client that has lost track of its own state, and seating it again
    /// would hand it a session whose predecessor is still writing to the same socket half.
    #[must_use]
    pub fn handle(&self, session: &Session, request: &Request, now: Timestamp) -> Reply {
        let call_id = session.call_id;
        let member = session.member;
        match request {
            Request::Join { .. } => refuse(&fault::validation(
                "ticket",
                "this connection is already seated; open a new connection to join again",
            )),
            Request::Publish {
                stream_id,
                kind,
                layers,
            } => self.state_change(
                self.sfu.publish(
                    call_id,
                    member,
                    PublishRequest {
                        stream_id: *stream_id,
                        kind: *kind,
                        layers: layers.clone(),
                    },
                ),
                Reply::Ok,
            ),
            Request::Unpublish { stream_id } => {
                self.state_change(self.sfu.unpublish(call_id, member, *stream_id), Reply::Ok)
            }
            Request::Subscribe {
                account_id,
                device_id,
                stream_id,
                layer,
            } => self.state_change(
                self.sfu.subscribe(
                    call_id,
                    member,
                    Member {
                        account_id: *account_id,
                        device_id: *device_id,
                    },
                    *stream_id,
                    *layer,
                    now,
                ),
                Reply::Ok,
            ),
            Request::Unsubscribe {
                account_id,
                device_id,
                stream_id,
            } => self.state_change(
                self.sfu.unsubscribe(
                    call_id,
                    member,
                    Member {
                        account_id: *account_id,
                        device_id: *device_id,
                    },
                    *stream_id,
                ),
                Reply::Ok,
            ),
            Request::Stats {
                account_id,
                device_id,
                stream_id,
                stats,
            } => match self.sfu.report_stats(
                call_id,
                member,
                Member {
                    account_id: *account_id,
                    device_id: *device_id,
                },
                *stream_id,
                &stats.into_link_stats(),
                now,
            ) {
                Ok(adaptation) => adaptation_reply(&adaptation),
                Err(error) => refuse(&error),
            },
            Request::Mode { mode } => self.state_change(
                self.sfu.set_bandwidth_mode(call_id, member, *mode),
                Reply::Ok,
            ),
            // The seat is retired by the caller that owns the socket, which is the only place
            // that can also close it. Answering here rather than leaving it unsaid keeps the
            // request/reply pairing every client's control loop depends on.
            Request::Leave => Reply::Ok,
            Request::Roster => Reply::Roster {
                seats: self.seats(call_id),
            },
        }
    }

    /// Counts a connection this plane refused at the door.
    ///
    /// The socket layer decides who is admitted, because it holds the ticket key; the meters
    /// live here, because this is the process's one description of what it did. A refusal an
    /// operator cannot count is a refusal they cannot tell from a client that never arrived.
    pub fn admission_refused(&self) {
        self.meters.admission_refused.inc();
    }

    /// Counts a media datagram whose routing header could not be read.
    pub fn datagram_refused(&self) {
        self.meters.datagram_refused.inc();
    }

    /// Counts a media frame the decision core would not forward.
    pub fn frame_refused(&self) {
        self.meters.frame_refused.inc();
    }

    /// The call's seats, as the wire carries them.
    #[must_use]
    pub fn seats(&self, call_id: Id) -> Vec<SeatWire> {
        self.sfu
            .roster(call_id)
            .iter()
            .map(SeatWire::from_view)
            .collect()
    }

    /// Queues a reply on a session, counting a drop the same way a media frame is counted.
    ///
    /// A control reply is small and rare, so a full queue here means the session is not reading
    /// at all; the reply is dropped rather than awaited, because awaiting would block the task
    /// that is supposed to be forwarding everybody else's frames.
    pub fn reply(&self, session: &Session, reply: &Reply) {
        if session.offer(Outbound::Control(encode_reply(reply))) {
            self.meters.dropped_control.inc();
        }
    }

    /// Tells the other seats of a call that one arrived or departed.
    fn announce(&self, call_id: Id, member: Member, joined: bool) {
        let reply = Reply::Peer { joined, member };
        let body = encode_reply(&reply);
        let sessions = self.sessions.lock();
        for ((call, device), session) in sessions.iter() {
            if *call != call_id || *device == member.device_id {
                continue;
            }
            if session.offer(Outbound::Control(body.clone())) {
                self.meters.dropped_control.inc();
            }
        }
    }

    /// Maps a core outcome to a reply, or a core refusal to an error reply.
    fn state_change<T>(&self, outcome: Result<T, Error>, ok: Reply) -> Reply {
        match outcome {
            Ok(_) => ok,
            Err(error) => refuse(&error),
        }
    }
}

/// Renders a core refusal as the wire's error reply.
fn refuse(error: &Error) -> Reply {
    Reply::Error {
        code: error.code(),
        retry_after_ms: error.retry_after().unwrap_or(0),
        message: error.public_message().to_string(),
    }
}

/// Renders what the core decided for one subscription.
fn adaptation_reply(adaptation: &Adaptation) -> Reply {
    Reply::Adaptation {
        quality: adaptation.quality,
        layer: adaptation.layer,
        frame_stride: adaptation.frame_stride,
        bitrate_cap_pct: adaptation.bitrate_cap_pct,
        keyframe_interval_ms: adaptation.keyframe_interval_ms,
        changed: adaptation.changed,
    }
}

impl LinkNumbers {
    /// Reads these numbers as the core's own view of a link.
    #[must_use]
    pub fn into_link_stats(self) -> LinkStats {
        LinkStats {
            packet_loss_pct: self.packet_loss_pct,
            rtt_ms: self.rtt_ms,
            jitter_ms: self.jitter_ms,
            available_kbps: self.available_kbps,
            sent_kbps: self.sent_kbps,
            dropped_frame_pct: self.dropped_frame_pct,
        }
    }
}

/// The node's own meters: what this process did, as opposed to what the calls did.
///
/// Separate names from the core's (`migo_sfu_*`) so an operator can tell a plane that is
/// refusing joins from one that is dropping frames, and both from the call-level counters the
/// core keeps.
struct Meters {
    sessions: Arc<Gauge>,
    admitted: Arc<Counter>,
    dropped_media: Arc<Counter>,
    dropped_control: Arc<Counter>,
    media_bytes: Arc<Counter>,
    media_bytes_dropped: Arc<Counter>,
    media_frames: Arc<Counter>,
    undeliverable: Arc<Counter>,
    admission_refused: Arc<Counter>,
    datagram_refused: Arc<Counter>,
    frame_refused: Arc<Counter>,
}

impl Meters {
    fn new(registry: &Registry) -> Self {
        Self {
            sessions: registry.gauge(
                "migo_sfu_node_sessions",
                "Media-plane sessions seated on this process.",
                &[],
            ),
            admitted: registry.counter(
                "migo_sfu_node_admissions_total",
                "Connections admitted to the media plane with a valid ticket.",
                &[],
            ),
            dropped_media: registry.counter(
                "migo_sfu_node_media_dropped_total",
                "Media frames dropped because a session's outbound queue was full.",
                &[],
            ),
            dropped_control: registry.counter(
                "migo_sfu_node_control_dropped_total",
                "Control frames dropped because a session was not reading its control stream.",
                &[],
            ),
            media_bytes: registry.counter(
                "migo_sfu_node_media_bytes_total",
                "Sealed media bytes queued for delivery.",
                &[],
            ),
            media_bytes_dropped: registry.counter(
                "migo_sfu_node_media_bytes_dropped_total",
                "Sealed media bytes dropped with the frames that carried them.",
                &[],
            ),
            media_frames: registry.counter(
                "migo_sfu_node_media_frames_total",
                "Sealed media frames offered for delivery.",
                &[],
            ),
            undeliverable: registry.counter(
                "migo_sfu_node_undeliverable_total",
                "Frames the call's seats asked for but no live session could receive.",
                &[],
            ),
            admission_refused: registry.counter(
                "migo_sfu_node_admissions_refused_total",
                "Connections refused before a seat: an unverifiable ticket, or a call at its ceiling.",
                &[],
            ),
            datagram_refused: registry.counter(
                "migo_sfu_node_datagrams_refused_total",
                "Media datagrams dropped because their routing header could not be read.",
                &[],
            ),
            frame_refused: registry.counter(
                "migo_sfu_node_frames_refused_total",
                "Media frames the decision core would not forward, such as an unknown stream.",
                &[],
            ),
        }
    }
}
