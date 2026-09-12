//! One session's outbound queue: the bounded mailbox every frame bound for the client passes
//! through, and the place brief sections 150 and 151 are enforced.
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

use std::collections::VecDeque;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::Notify;

use migo_core::Timestamp;
use migo_protocol::DeliveryClass;

use crate::metrics::Dropped;

/// A frame waiting to be written to the client.
struct Queued {
    bytes: Bytes,
    class: DeliveryClass,
    /// The coalescing key, for a [`DeliveryClass::Coalescable`] frame; `None` otherwise.
    coalesce_key: Option<u64>,
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
    /// The frame was dropped under pressure; the argument names its class for the metric.
    Dropped(Dropped),
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
}

/// A session's outbound mailbox. Cloned handles (an [`std::sync::Arc`]) are held by the
/// session's own writer and by every other session that fans out to it, so `push` is called
/// from many tasks and `take_ready` from exactly one.
pub(crate) struct Outbound {
    inner: Mutex<Inner>,
    notify: Notify,
}

impl Outbound {
    /// A fresh, empty mailbox.
    pub(crate) fn new(capacity: usize, ring_cap: usize, resume_window_ms: i64) -> Self {
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
            }),
            notify: Notify::new(),
        }
    }

    /// Pushes a frame, applying the delivery-class policy, and wakes the writer if anything
    /// became sendable.
    pub(crate) fn push(
        &self,
        bytes: Bytes,
        class: DeliveryClass,
        coalesce_key: Option<u64>,
        now: Timestamp,
    ) -> PushOutcome {
        let mut inner = self.inner.lock();
        if inner.closed {
            return PushOutcome::Closed;
        }
        let outcome = match class {
            DeliveryClass::Critical => {
                let seq = inner.next_seq;
                inner.next_seq += 1;
                inner.queue.push_back(Queued {
                    bytes: bytes.clone(),
                    class,
                    coalesce_key: None,
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
                let existing = coalesce_key.and_then(|key| {
                    inner.queue.iter_mut().find(|q| {
                        q.class == DeliveryClass::Coalescable && q.coalesce_key == Some(key)
                    })
                });
                if let Some(slot) = existing {
                    slot.bytes = bytes;
                    PushOutcome::Coalesced
                } else if inner.queue.len() >= inner.capacity {
                    PushOutcome::Dropped(Dropped::Coalescable)
                } else {
                    inner.queue.push_back(Queued {
                        bytes,
                        class,
                        coalesce_key,
                    });
                    PushOutcome::Enqueued
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
                    });
                    PushOutcome::Enqueued
                }
            }
        };
        drop(inner);
        if matches!(outcome, PushOutcome::Enqueued | PushOutcome::Coalesced) {
            self.notify.notify_one();
        }
        outcome
    }

    /// Takes every currently-queued frame's bytes, in order, for the writer to send.
    ///
    /// The writer measures the drain itself against the lagging deadline (section 160), because
    /// fullness here is invisible once taken: this is the handoff point, and a client that
    /// cannot keep up is one whose frames cannot cross it in time.
    pub(crate) fn take_ready(&self) -> Vec<Bytes> {
        let mut inner = self.inner.lock();
        let ready: Vec<Bytes> = inner.queue.drain(..).map(|q| q.bytes).collect();
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

    fn now() -> Timestamp {
        Timestamp::from_millis(0)
    }

    #[test]
    fn a_coalescable_frame_replaces_its_queued_older_value_at_capacity() {
        // A one-slot queue, already holding an older value for a key: the situation
        // every burst of presence or typing updates creates. The newer value must not
        // be dropped — it replaces the older one in place, so the burst costs one slot
        // no matter how long it runs (sections 151 and 160).
        let outbound = Outbound::new(1, 8, 60_000);
        let key = 7;
        let older = Bytes::from_static(b"presence-v1");
        assert_eq!(
            outbound.push(older, DeliveryClass::Coalescable, Some(key), now()),
            PushOutcome::Enqueued
        );
        let newer = Bytes::from_static(b"presence-v2");
        assert_eq!(
            outbound.push(newer.clone(), DeliveryClass::Coalescable, Some(key), now()),
            PushOutcome::Coalesced,
            "at capacity, a newer same-key value replaces the older one; it is never dropped"
        );
        let ready = outbound.take_ready();
        assert_eq!(ready.len(), 1, "coalescing must not grow the queue");
        assert_eq!(
            ready[0], newer,
            "the value the client sees is the newest one"
        );
    }
}
