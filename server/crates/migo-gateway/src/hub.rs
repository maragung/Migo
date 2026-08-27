//! The subscription hub: who is listening to what, and the one place a server event fans out
//! to many sockets.
//!
//! # Encode once, send N
//!
//! An event bound for a topic is encoded to bytes exactly once by the caller, and the hub
//! hands each subscriber a cheap [`Bytes`] clone — a reference-count bump, not a copy or a
//! re-encode (brief section 136's hot-path rule). A conversation with a thousand members costs
//! one encode and a thousand pointer bumps, and the compression decision is made once for all
//! of them.
//!
//! # A bounded number of ears per session
//!
//! One session may hold at most [`MAX_SUBSCRIPTIONS`](crate::config::MAX_SUBSCRIPTIONS) topics
//! (section 149). The cap is per session and enforced here, so a client cannot pin unbounded
//! server memory by subscribing to everything; the surplus is rejected, not silently dropped.
//!
//! # Locks are never held across a push
//!
//! A fan-out reads a topic's subscriber set into a small vector of ids, releases the shard
//! lock, and only then looks up each mailbox. Pushing into a mailbox never happens while a map
//! shard is locked, so a slow or contended mailbox cannot stall the whole hub.

use std::collections::HashSet;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;

use migo_core::{Id, Timestamp};
use migo_protocol::{DeliveryClass, Topic};

use crate::metrics::{Meters, Refused};
use crate::outbound::PushOutcome;
use crate::session::SessionHandle;
use crate::topic::TopicKey;

/// The subscription registry and fan-out point for one node.
pub(crate) struct Hub {
    /// Topic to the set of sessions listening on it.
    subscribers: DashMap<TopicKey, HashSet<Id>>,
    /// Session id to its handle, for delivery.
    sessions: DashMap<Id, SessionHandle>,
    /// Session id to the topics it holds, for cap enforcement and clean teardown.
    session_topics: DashMap<Id, HashSet<TopicKey>>,
    max_subscriptions: usize,
    meters: Arc<Meters>,
}

impl Hub {
    /// A hub with the given per-session subscription ceiling.
    pub(crate) fn new(max_subscriptions: usize, meters: Arc<Meters>) -> Self {
        Self {
            subscribers: DashMap::new(),
            sessions: DashMap::new(),
            session_topics: DashMap::new(),
            max_subscriptions,
            meters,
        }
    }

    /// Registers a session so it can be delivered to.
    pub(crate) fn register(&self, handle: SessionHandle) {
        let id = handle.session_id();
        self.sessions.insert(id, handle);
        self.session_topics.entry(id).or_default();
    }

    /// Removes a session and all of its subscriptions.
    pub(crate) fn deregister(&self, session_id: Id) {
        self.sessions.remove(&session_id);
        if let Some((_, topics)) = self.session_topics.remove(&session_id) {
            for key in &topics {
                if let Some(mut set) = self.subscribers.get_mut(key) {
                    set.remove(&session_id);
                    if set.is_empty() {
                        drop(set);
                        self.subscribers.remove(key);
                    }
                }
            }
            self.meters.subscriptions_removed(topics.len() as u64);
        }
    }

    /// Adds subscriptions, enforcing the per-session cap, and reports which were accepted and
    /// which were refused. Already-held topics are accepted idempotently and do not count
    /// against the cap a second time.
    pub(crate) fn subscribe(&self, session_id: Id, topics: &[Topic]) -> Subscribed {
        let mut held = self.session_topics.entry(session_id).or_default();
        let mut accepted = Vec::with_capacity(topics.len());
        let mut rejected = Vec::new();
        let mut added = 0_u64;
        for topic in topics {
            let key = TopicKey::of(topic);
            if held.contains(&key) {
                accepted.push(topic.clone());
            } else if held.len() < self.max_subscriptions {
                held.insert(key);
                self.subscribers.entry(key).or_default().insert(session_id);
                accepted.push(topic.clone());
                added += 1;
            } else {
                rejected.push(topic.clone());
            }
        }
        drop(held);
        self.meters.subscriptions_added(added);
        self.meters
            .subscriptions_refused(Refused::Cap, rejected.len() as u64);
        Subscribed { accepted, rejected }
    }

    /// Removes subscriptions the session holds; unknown topics are ignored.
    pub(crate) fn unsubscribe(&self, session_id: Id, topics: &[Topic]) {
        let Some(mut held) = self.session_topics.get_mut(&session_id) else {
            return;
        };
        let mut removed = 0_u64;
        for topic in topics {
            let key = TopicKey::of(topic);
            if held.remove(&key) {
                removed += 1;
                if let Some(mut set) = self.subscribers.get_mut(&key) {
                    set.remove(&session_id);
                    if set.is_empty() {
                        drop(set);
                        self.subscribers.remove(&key);
                    }
                }
            }
        }
        drop(held);
        self.meters.subscriptions_removed(removed);
    }

    /// Fans one pre-encoded frame out to every subscriber of a topic.
    ///
    /// `encoded` is the whole frame, encoded once; each subscriber receives a clone. Frames
    /// dropped under a subscriber's backpressure are counted here, so a slow client shows up
    /// in the drop metric rather than stalling the sender. `exclude`, when set, is the one
    /// session id that is skipped — the originator of a mutation, so a caller does not receive
    /// the echo of its own change (a domain fanout carries the sender's device for exactly
    /// this) while every other device on the topic, including the sender's own other
    /// connections, still does.
    pub(crate) fn broadcast(
        &self,
        topic: &Topic,
        encoded: &Bytes,
        class: DeliveryClass,
        coalesce_key: Option<u64>,
        now: Timestamp,
        exclude: Option<Id>,
    ) {
        let key = TopicKey::of(topic);
        let targets: Vec<Id> = match self.subscribers.get(&key) {
            Some(set) => set.iter().copied().collect(),
            None => return,
        };
        for session_id in targets {
            if Some(session_id) == exclude {
                continue;
            }
            if let Some(handle) = self.sessions.get(&session_id) {
                let outcome = handle
                    .outbound()
                    .push(encoded.clone(), class, coalesce_key, now);
                if let PushOutcome::Dropped(class) = outcome {
                    self.meters.frame_dropped(class);
                }
            }
        }
    }
}

/// The outcome of a [`Hub::subscribe`]: the topics taken and the topics refused for the cap.
pub(crate) struct Subscribed {
    pub(crate) accepted: Vec<Topic>,
    pub(crate) rejected: Vec<Topic>,
}
