//! One session's outbound queue: the bounded mailbox every frame bound for the client passes
//! through, and the place brief sections 150, 151, and 159 are enforced.
//!
//! # Three classes, three fates under pressure
//!
//! A frame carries a [`DeliveryClass`] (section 151), and that is the whole of what the queue
//! needs to decide its fate when the client cannot keep up:
//!
//! - **Critical** is never dropped. If the queue is full the frame still goes in — the queue may
//!   exceed capacity, because the alternative is losing a frame the client cannot recover — and
//!   the writer's drain deadline (section 160: the session closes as lagging when frames cannot
//!   be handed to the socket within `lagging_deadline_ms`) is what turns a client that cannot
//!   keep up into a resume, because dropping a Critical frame silently is far worse than forcing
//!   a reconnect.
//! - **Coalescable** collapses: a newer value for the same coalescing key overwrites the older
//!   one already queued, in place, so a burst of typing or presence updates costs one slot,
//!   not a hundred. If none is queued to overwrite and the queue is full, the newest is
//!   dropped — the next update will carry the current value anyway.
//! - **Droppable** is dropped silently under pressure, but counted, because a frame that
//!   vanishes without a metric is a bug nobody finds for months.
//!
//! # Pacing, and why it is a hold rather than a drop
//!
//! A `Coalescable` frame whose opcode is `paced` in the schema (presence, section 159) is
//! spaced by the session's presence minimum interval, which the session's bandwidth mode
//! decides. A frame that arrives inside the window is *held*, not dropped: the state it carries
//! may be the last one for a while — a user who goes Online and then Away within five seconds
//! must end up showing Away — so the queue keeps it, with its due time attached, and a newer
//! value for the same subject replaces it inside the hold. The trailing edge is the invariant:
//! whatever the window releases is the newest state, never a stale one and never nothing. A
//! held frame does not block the frames behind it; [`Outbound::take_ready`] walks past it and
//! the writer's ticker comes back for it within one tick of its due time.
//!
//! # Suppression
//!
//! An opcode whose schema entry names the session's bandwidth mode in `suppress_on` (typing on
//! `UltraLowData`, section 159) is refused at the mailbox: the frame never occupies a slot and
//! never spends the bytes, because the client that negotiated that mode does not render it.
//!
//! # `frame_seq`, the ring, and cumulative ACK
//!
//! Only Critical frames carry a `frame_seq` (section 141 says a droppable frame need not be
//! tracked at all) and only Critical frames are retained in a ring buffer for resume
//! (section 150): capacity [`resume_buffer_frames`], window [`resume_window_ms`]. A cumulative
//! ACK from the client advances a watermark that trims the ring — one ACK settles hundreds of
//! frames (section 151). The ring doubles as the redelivery buffer: an unacked Critical frame
//! is exactly one still in the ring, and a resume resends the ring's tail.
//!
//! [`resume_buffer_frames`]: crate::config::Settings::resume_buffer_frames
//! [`resume_window_ms`]: crate::config::Settings::resume_window_ms

use std::collections::{HashMap, VecDeque};

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::Notify;

use migo_core::Timestamp;
use migo_protocol::{cadence_for, BandwidthMode, Cadence, DeliveryClass, Opcode};

use crate::metrics::Dropped;

/// A frame waiting to be written to the client.
struct Queued {
    bytes: Bytes,
    class: DeliveryClass,
    /// The coalescing key, for a [`DeliveryClass::Coalescable`] frame; `None` otherwise.
    coalesce_key: Option<u64>,
    /// Whether the frame carries the presence minimum interval (section 159). Only a paced
    /// frame can be held, and only a paced frame moves the delivery clock for its key.
    paced: bool,
    /// When a held frame may leave the queue; `None` for a frame that is ready now.
    hold_until: Option<Timestamp>,
}

/// A Critical frame kept in the resume ring, tagged with its sequence and when it was sent.
#[derive(Clone)]
struct Retained {
    seq: u64,
    sent_at: Timestamp,
    bytes: Bytes,
}

/// What a [`Outbound::push`] did, so the caller can move the right metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    /// The frame was queued for sending.
    Enqueued,
    /// A coalescable frame replaced an older one for the same key in place.
    Coalesced,
    /// A coalescable frame replaced an older one for the same key in place and is held until
    /// its pacing window closes; the value the window releases is this newest one.
    CoalescedHeld,
    /// The frame was held until its pacing window closes (section 159).
    Held,
    /// The frame was dropped under pressure; the argument names its class for the metric.
    Dropped(Dropped),
    /// The frame's opcode is suppressed for this session's bandwidth mode (section 159), so
    /// it was refused at the mailbox. Not a drop: policy, not pressure, and nothing to count.
    Suppressed,
    /// The queue is closed; the frame was discarded.
    Closed,
}

/// A detached snapshot of a session's resume state, kept after it disconnects so a
/// reconnecting client can bridge the gap (section 150).
#[derive(Clone)]
pub(crate) struct ResumeBuffer {
    frames: Vec<Retained>,
    next_seq: u64,
    expires_at: Timestamp,
}

impl ResumeBuffer {
    /// Whether this buffer can still bridge a client that last saw `last_seq`.
    ///
    /// It can when the buffer has not expired, the client is not claiming a frame past what
    /// was ever sent, and there is no gap below the oldest retained frame — a gap means a
    /// Critical frame was evicted before the client acknowledged it, which only a full resync
    /// can repair.
    pub(crate) fn covers(&self, last_seq: u64, now: Timestamp) -> bool {
        if now.as_unix_ms() > self.expires_at.as_unix_ms() {
            return false;
        }
        if last_seq >= self.next_seq {
            return false;
        }
        match self.frames.first() {
            Some(first) => last_seq + 1 >= first.seq,
            None => true,
        }
    }

    /// The retained frames the client has not seen, oldest first.
    fn frames_after(&self, last_seq: u64) -> Vec<Retained> {
        self.frames
            .iter()
            .filter(|frame| frame.seq > last_seq)
            .cloned()
            .collect()
    }

    /// Whether this buffer's resume window has passed, so it can serve no client and is only
    /// taking up room in the node's resume store.
    pub(crate) fn expired(&self, now: Timestamp) -> bool {
        now.as_unix_ms() > self.expires_at.as_unix_ms()
    }
}

/// The mutable state, behind one lock.
struct Inner {
    queue: VecDeque<Queued>,
    capacity: usize,
    /// The next `frame_seq` to assign to a Critical frame (section 152). Per session, per
    /// direction; this is the server-to-client direction.
    next_seq: u64,
    ring: VecDeque<Retained>,
    ring_cap: usize,
    resume_window_ms: i64,
    /// The highest `frame_seq` the client has acknowledged.
    ack_watermark: u64,
    closed: bool,
    /// The bandwidth mode this session negotiated in its `HELLO`, kept because opcode
    /// suppression (section 159) is a fact about the pair of frame and mode.
    mode: BandwidthMode,
    /// The intervals this session runs at (section 159): the presence floor the queue enforces,
    /// and the heartbeat the gateway advertised to earn it.
    cadence: Cadence,
    /// When each paced coalescing key was last handed to the writer, so the floor is measured
    /// between deliveries rather than between arrivals — a frame that sat behind a slow drain
    /// must still count as the session's one presence frame for its subject in the window.
    paced_sent: HashMap<u64, Timestamp>,
}

impl Inner {
    /// Drops ring entries the client has acknowledged or that have aged out of the window.
    fn trim_ring(&mut self, now: Timestamp) {
        while self.ring.len() > self.ring_cap {
            self.ring.pop_front();
        }
        let cutoff = now.as_unix_ms().saturating_sub(self.resume_window_ms);
        while let Some(front) = self.ring.front() {
            if front.sent_at.as_unix_ms() < cutoff {
                self.ring.pop_front();
            } else {
                break;
            }
        }
    }

    /// When a paced frame for `key`, arriving at `now`, may be delivered: one full window after
    /// the key's last delivery, or immediately if it has been at least that long (or the key
    /// has never been delivered on this session).
    fn pacing_hold(&self, key: u64, now: Timestamp) -> Option<Timestamp> {
        let last = self.paced_sent.get(&key)?;
        let due = last.saturating_add_millis(i64::from(self.cadence.min_interval_ms));
        (now.as_unix_ms() < due.as_unix_ms()).then_some(due)
    }
}

/// A session's outbound mailbox. Cloned handles (an [`std::sync::Arc`]) are held by the
/// session's own writer and by every other session that fans out to it, so `push` is called
/// from many tasks and `take_ready` from exactly one.
pub(crate) struct Outbound {
    inner: Mutex<Inner>,
    notify: Notify,
}

impl Outbound {
    /// A fresh, empty mailbox for a session that negotiated `mode` against a node advertising
    /// `heartbeat_ms` to a `Normal` session. The cadence derived here is the one the `WELCOME`
    /// advertises and the one this queue paces by, computed once so the two can never disagree.
    pub(crate) fn new(
        capacity: usize,
        ring_cap: usize,
        resume_window_ms: i64,
        mode: BandwidthMode,
        heartbeat_ms: u32,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                capacity,
                next_seq: 1,
                ring: VecDeque::new(),
                ring_cap,
                resume_window_ms,
                ack_watermark: 0,
                closed: false,
                mode,
                cadence: cadence_for(mode, heartbeat_ms),
                paced_sent: HashMap::new(),
            }),
            notify: Notify::new(),
        }
    }

    /// The intervals this session runs at (section 159). The connection driver reads the
    /// heartbeat from here for the `WELCOME` and the liveness deadline, so both follow the
    /// mode the client actually declared.
    pub(crate) fn cadence(&self) -> Cadence {
        self.inner.lock().cadence
    }

    /// Pushes a frame, applying the delivery-class policy and the section 159 rules, and wakes
    /// the writer if anything became sendable.
    pub(crate) fn push(
        &self,
        bytes: Bytes,
        class: DeliveryClass,
        opcode: Opcode,
        coalesce_key: Option<u64>,
        now: Timestamp,
    ) -> PushOutcome {
        let mut inner = self.inner.lock();
        if inner.closed {
            return PushOutcome::Closed;
        }
        if opcode.suppressed_on(inner.mode) {
            return PushOutcome::Suppressed;
        }
        let outcome = match class {
            DeliveryClass::Critical => {
                let seq = inner.next_seq;
                inner.next_seq += 1;
                inner.queue.push_back(Queued {
                    bytes: bytes.clone(),
                    class,
                    coalesce_key: None,
                    paced: false,
                    hold_until: None,
                });
                inner.ring.push_back(Retained {
                    seq,
                    sent_at: now,
                    bytes,
                });
                inner.trim_ring(now);
                PushOutcome::Enqueued
            }
            DeliveryClass::Coalescable => {
                // Only a paced opcode carries the floor, and only a keyed frame can be paced:
                // the window is per subject, and a frame with no key names no subject.
                let paced = opcode.paced() && coalesce_key.is_some();
                // Computed before the queue is searched, because the search holds the queue
                // mutably and the clock it reads lives beside it.
                let hold = if paced {
                    coalesce_key.and_then(|key| inner.pacing_hold(key, now))
                } else {
                    None
                };
                let existing = coalesce_key.and_then(|key| {
                    inner.queue.iter_mut().find(|q| {
                        q.class == DeliveryClass::Coalescable && q.coalesce_key == Some(key)
                    })
                });
                if let Some(slot) = existing {
                    slot.bytes = bytes;
                    // The replacement keeps whatever hold the older value was under: the
                    // window belongs to the subject, not to the value, and releasing the
                    // newest state at the window's close is the whole trailing edge. A ready
                    // slot takes the hold computed above, so a value replacing a delivered
                    // one inside a new window waits out that window.
                    if paced && slot.hold_until.is_none() {
                        slot.hold_until = hold;
                    }
                    if paced && slot.hold_until.is_some() {
                        PushOutcome::CoalescedHeld
                    } else {
                        PushOutcome::Coalesced
                    }
                } else if inner.queue.len() >= inner.capacity {
                    PushOutcome::Dropped(Dropped::Coalescable)
                } else {
                    inner.queue.push_back(Queued {
                        bytes,
                        class,
                        coalesce_key,
                        paced,
                        hold_until: hold,
                    });
                    match hold {
                        Some(_) => PushOutcome::Held,
                        None => PushOutcome::Enqueued,
                    }
                }
            }
            DeliveryClass::Droppable => {
                if inner.queue.len() >= inner.capacity {
                    PushOutcome::Dropped(Dropped::Droppable)
                } else {
                    inner.queue.push_back(Queued {
                        bytes,
                        class,
                        coalesce_key: None,
                        paced: false,
                        hold_until: None,
                    });
                    PushOutcome::Enqueued
                }
            }
        };
        drop(inner);
        if matches!(
            outcome,
            PushOutcome::Enqueued
                | PushOutcome::Coalesced
                | PushOutcome::CoalescedHeld
                | PushOutcome::Held
        ) {
            self.notify.notify_one();
        }
        outcome
    }

    /// Takes every frame whose pacing window has closed, in order, for the writer to send.
    ///
    /// Held frames stay queued, in their position, and the frames behind them are not made to
    /// wait: a presence frame holding its window is never allowed to delay a message. Each
    /// paced frame taken here stamps its key's delivery clock, which is what the next arrival
    /// is measured against — the floor is between deliveries, and the writer sends what it
    /// takes immediately. Stale stamps are swept as part of the walk, so the map is bounded by
    /// the subjects a session is actively pacing rather than by every subject it ever saw.
    ///
    /// The writer measures the drain itself against the lagging deadline (section 160), because
    /// fullness here is invisible once taken: this is the handoff point, and a client that
    /// cannot keep up is one whose frames cannot cross it in time.
    pub(crate) fn take_ready(&self, now: Timestamp) -> Vec<Bytes> {
        let mut inner = self.inner.lock();
        let mut ready = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(queued) = inner.queue.pop_front() {
            match queued.hold_until {
                Some(due) if !now.is_at_or_after(due) => retained.push_back(queued),
                _ => {
                    if queued.paced {
                        if let Some(key) = queued.coalesce_key {
                            inner.paced_sent.insert(key, now);
                        }
                    }
                    ready.push(queued.bytes);
                }
            }
        }
        inner.queue = retained;
        let cutoff = now
            .as_unix_ms()
            .saturating_sub(i64::from(inner.cadence.min_interval_ms));
        inner
            .paced_sent
            .retain(|_, last| last.as_unix_ms() >= cutoff);
        ready
    }

    /// Waits until there may be something to send, or the queue is closed.
    pub(crate) async fn wait(&self) {
        self.notify.notified().await;
    }

    /// Advances the cumulative ACK watermark and trims the ring of everything at or below it.
    pub(crate) fn acknowledge(&self, watermark: u64) {
        let mut inner = self.inner.lock();
        if watermark <= inner.ack_watermark {
            return;
        }
        inner.ack_watermark = watermark;
        while let Some(front) = inner.ring.front() {
            if front.seq <= watermark {
                inner.ring.pop_front();
            } else {
                break;
            }
        }
    }

    /// Detaches the resume state for retention after the session disconnects.
    pub(crate) fn resume_buffer(&self, now: Timestamp) -> ResumeBuffer {
        let inner = self.inner.lock();
        ResumeBuffer {
            frames: inner.ring.iter().cloned().collect(),
            next_seq: inner.next_seq,
            expires_at: now.saturating_add_millis(inner.resume_window_ms),
        }
    }

    /// Seeds a freshly-built mailbox from a retained buffer on resume, re-queuing every frame
    /// the client has not seen (keeping its original bytes, and so its original id, per
    /// section 150) and returning how many were re-queued.
    pub(crate) fn seed_resume(&self, buffer: &ResumeBuffer, last_seq: u64) -> usize {
        let pending = buffer.frames_after(last_seq);
        let mut inner = self.inner.lock();
        inner.next_seq = buffer.next_seq;
        inner.ring = buffer.frames.iter().cloned().collect();
        inner.ack_watermark = last_seq;
        for frame in &pending {
            inner.queue.push_back(Queued {
                bytes: frame.bytes.clone(),
                class: DeliveryClass::Critical,
                coalesce_key: None,
                paced: false,
                hold_until: None,
            });
        }
        let count = pending.len();
        drop(inner);
        if count > 0 {
            self.notify.notify_one();
        }
        count
    }

    /// Marks the queue closed and wakes the writer so it can exit.
    pub(crate) fn close(&self) {
        self.inner.lock().closed = true;
        self.notify.notify_one();
    }

    /// Whether the queue has been closed.
    pub(crate) fn is_closed(&self) -> bool {
        self.inner.lock().closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Normal session on a 30-second heartbeat: presence floor of five seconds (section 159).
    fn mailbox(mode: BandwidthMode) -> Outbound {
        Outbound::new(8, 8, 60_000, mode, 30_000)
    }

    #[test]
    fn a_coalescable_frame_replaces_its_queued_older_value_at_capacity() {
        // A one-slot queue, already holding an older value for a key: the situation
        // every burst of presence or typing updates creates. The newer value must not
        // be dropped — it replaces the older one in place, so the burst costs one slot
        // no matter how long it runs (sections 151 and 160).
        let outbound = Outbound::new(1, 8, 60_000, BandwidthMode::Normal, 30_000);
        let key = 7;
        let older = Bytes::from_static(b"presence-v1");
        assert_eq!(
            outbound.push(
                older,
                DeliveryClass::Coalescable,
                Opcode::Typing,
                Some(key),
                Timestamp::from_millis(0)
            ),
            PushOutcome::Enqueued
        );
        let newer = Bytes::from_static(b"presence-v2");
        assert_eq!(
            outbound.push(
                newer.clone(),
                DeliveryClass::Coalescable,
                Opcode::Typing,
                Some(key),
                Timestamp::from_millis(0)
            ),
            PushOutcome::Coalesced,
            "at capacity, a newer same-key value replaces the older one; it is never dropped"
        );
        let ready = outbound.take_ready(Timestamp::from_millis(0));
        assert_eq!(ready.len(), 1, "coalescing must not grow the queue");
        assert_eq!(
            ready[0], newer,
            "the value the client sees is the newest one"
        );
    }

    #[test]
    fn a_paced_frame_inside_the_window_is_held_and_released_with_the_newest_value() {
        // The trailing edge of section 159: Online at t=0 is delivered, Away at t=1s is
        // held (the floor on a Normal 30-second heartbeat is five seconds), Offline at
        // t=2s replaces it inside the hold, and the window that closes at t=5s releases
        // Offline — never Online, never nothing.
        let outbound = mailbox(BandwidthMode::Normal);
        let key = 1;
        let online = Bytes::from_static(b"presence-online");
        assert_eq!(
            outbound.push(
                online,
                DeliveryClass::Coalescable,
                Opcode::PresenceEvent,
                Some(key),
                Timestamp::from_millis(0)
            ),
            PushOutcome::Enqueued
        );
        assert_eq!(
            outbound.take_ready(Timestamp::from_millis(0))[0],
            Bytes::from_static(b"presence-online")
        );

        let away = Bytes::from_static(b"presence-away");
        assert_eq!(
            outbound.push(
                away,
                DeliveryClass::Coalescable,
                Opcode::PresenceEvent,
                Some(key),
                Timestamp::from_millis(1_000)
            ),
            PushOutcome::Held,
            "one second after the last delivery, the five-second floor holds the frame"
        );
        let offline = Bytes::from_static(b"presence-offline");
        assert_eq!(
            outbound.push(
                offline.clone(),
                DeliveryClass::Coalescable,
                Opcode::PresenceEvent,
                Some(key),
                Timestamp::from_millis(2_000)
            ),
            PushOutcome::CoalescedHeld,
            "a newer value for a held key replaces it inside the hold"
        );
        assert!(
            outbound
                .take_ready(Timestamp::from_millis(4_999))
                .is_empty(),
            "the window has not closed yet"
        );
        let ready = outbound.take_ready(Timestamp::from_millis(5_000));
        assert_eq!(ready.len(), 1);
        assert_eq!(
            ready[0], offline,
            "the trailing edge delivers the newest state, not the first one in the window"
        );
    }

    #[test]
    fn a_held_paced_frame_does_not_delay_the_frames_behind_it() {
        // Holding presence is a statement about presence. A frame queued behind a
        // held one leaves with the next drain, or a slow presence subject would
        // become a slow conversation.
        let outbound = mailbox(BandwidthMode::Normal);
        outbound.push(
            Bytes::from_static(b"presence-v1"),
            DeliveryClass::Coalescable,
            Opcode::PresenceEvent,
            Some(1),
            Timestamp::from_millis(0),
        );
        outbound.take_ready(Timestamp::from_millis(0));
        outbound.push(
            Bytes::from_static(b"presence-v2"),
            DeliveryClass::Coalescable,
            Opcode::PresenceEvent,
            Some(1),
            Timestamp::from_millis(1_000),
        );
        outbound.push(
            Bytes::from_static(b"message"),
            DeliveryClass::Coalescable,
            Opcode::Typing,
            Some(2),
            Timestamp::from_millis(1_000),
        );
        let ready = outbound.take_ready(Timestamp::from_millis(1_000));
        assert_eq!(ready, vec![Bytes::from_static(b"message")]);
    }

    #[test]
    fn a_typing_frame_is_not_paced_by_the_presence_floor() {
        // Typing is Coalescable but not paced: a start mark that arrives one second
        // after a stop mark must still go out, or a conversation would show nobody
        // typing for the whole floor.
        let outbound = mailbox(BandwidthMode::Normal);
        let key = 3;
        outbound.push(
            Bytes::from_static(b"typing-stop"),
            DeliveryClass::Coalescable,
            Opcode::Typing,
            Some(key),
            Timestamp::from_millis(0),
        );
        outbound.take_ready(Timestamp::from_millis(0));
        assert_eq!(
            outbound.push(
                Bytes::from_static(b"typing-start"),
                DeliveryClass::Coalescable,
                Opcode::Typing,
                Some(key),
                Timestamp::from_millis(1_000)
            ),
            PushOutcome::Enqueued,
            "the floor of section 159 belongs to presence, not to every coalescable key"
        );
        assert_eq!(
            outbound.take_ready(Timestamp::from_millis(1_000)),
            vec![Bytes::from_static(b"typing-start")]
        );
    }

    #[test]
    fn typing_is_suppressed_on_a_session_that_negotiated_ultra_low_data() {
        // Section 159: typing is off entirely on UltraLowData, and the byte is saved
        // at the server — so the mailbox refuses the frame rather than queueing a
        // drop the client will never render.
        let outbound = mailbox(BandwidthMode::UltraLowData);
        assert_eq!(
            outbound.push(
                Bytes::from_static(b"typing"),
                DeliveryClass::Coalescable,
                Opcode::Typing,
                Some(4),
                Timestamp::from_millis(0)
            ),
            PushOutcome::Suppressed
        );
        assert!(
            outbound.take_ready(Timestamp::from_millis(0)).is_empty(),
            "a suppressed frame never occupied a slot"
        );
        // The same opcode reaches a Normal session unchanged.
        let outbound = mailbox(BandwidthMode::Normal);
        assert_eq!(
            outbound.push(
                Bytes::from_static(b"typing"),
                DeliveryClass::Coalescable,
                Opcode::Typing,
                Some(4),
                Timestamp::from_millis(0)
            ),
            PushOutcome::Enqueued
        );
    }

    #[test]
    fn the_delivery_clock_is_measured_from_delivery_not_arrival() {
        // A frame that arrives at t=0 but is only drained at t=3s (the writer was
        // busy) opens the next window at t=3s plus the floor, not at t=5s — otherwise
        // the client would see two presence frames three seconds apart and the
        // server would call it five.
        let outbound = mailbox(BandwidthMode::Normal);
        let key = 9;
        outbound.push(
            Bytes::from_static(b"v1"),
            DeliveryClass::Coalescable,
            Opcode::PresenceEvent,
            Some(key),
            Timestamp::from_millis(0),
        );
        outbound.take_ready(Timestamp::from_millis(3_000));
        assert_eq!(
            outbound.push(
                Bytes::from_static(b"v2"),
                DeliveryClass::Coalescable,
                Opcode::PresenceEvent,
                Some(key),
                Timestamp::from_millis(3_500)
            ),
            PushOutcome::Held,
            "the window runs from the delivery at t=3s, so t=3.5s is inside it"
        );
    }

    #[test]
    fn the_floor_follows_the_session_mode() {
        // UltraLowData raises the floor to a whole heartbeat (section 159), so the
        // same arrival pattern that a Normal session releases at five seconds is
        // held for thirty on a metered connection.
        let outbound = mailbox(BandwidthMode::UltraLowData);
        let key = 5;
        outbound.push(
            Bytes::from_static(b"v1"),
            DeliveryClass::Coalescable,
            Opcode::PresenceEvent,
            Some(key),
            Timestamp::from_millis(0),
        );
        outbound.take_ready(Timestamp::from_millis(0));
        assert_eq!(
            outbound.push(
                Bytes::from_static(b"v2"),
                DeliveryClass::Coalescable,
                Opcode::PresenceEvent,
                Some(key),
                Timestamp::from_millis(20_000)
            ),
            PushOutcome::Held,
            "twenty seconds is inside the thirty-second floor of an UltraLowData session"
        );
        assert!(outbound
            .take_ready(Timestamp::from_millis(29_999))
            .is_empty());
        assert_eq!(
            outbound.take_ready(Timestamp::from_millis(30_000)),
            vec![Bytes::from_static(b"v2")]
        );
    }
}
