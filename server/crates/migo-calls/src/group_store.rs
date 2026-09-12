//! Where the group-call roster lives.
//!
//! # Why the roster is a second store and not a column
//!
//! A group call's state is one list of seats, and the traffic it sees — a
//! join, a leave, a relay — is a read-modify-write of that list. The 1:1
//! [`CallStore`](crate::CallStore) holds rows whose whole shape is "two named
//! parties"; a roster is a set, and forcing it into that shape would give the
//! service an upsert it cannot express. The trait here is the shape a
//! production backend has to hold: a keyed upsert of the whole roster, a keyed
//! read, and a leave that retires an empty call. The in-memory backend is the
//! honest v1, the same choice the 1:1 store makes: a call that outlives a node
//! restart is nobody's expectation, because the participants' own clients time
//! the call and re-join against a new node.
//!
//! # What the store never sees
//!
//! Sealed offers it does see — stored, re-served, never opened. What it never
//! sees is media: an SFU that forwarded media would need a media plane, and
//! this node's group call is a *signalling* SFU (section 166) — the roster and
//! the sealed descriptions are everything it holds.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use migo_core::{Id, Result};
use parking_lot::Mutex;

use crate::model::{GroupCall, GroupParticipant};

/// Group-call persistence, as the service needs it.
///
/// Three operations. [`GroupCallStore::put`] is the whole-roster upsert the
/// service's read-modify-write goes through — a backend that can make it
/// atomic should, because two racing joins are the traffic a group call
/// actually gets and the in-memory backend's one lock across the critical
/// section is the behaviour to match. [`GroupCallStore::retire_if_empty`]
/// retires a call whose last seat emptied, so an abandoned call does not sit
/// in the map forever holding the id a re-join would collide with.
#[async_trait]
pub trait GroupCallStore: Send + Sync {
    /// Writes the call, replacing whatever the id held.
    async fn put(&self, call: &GroupCall) -> Result<()>;

    /// Reads the call, if it exists.
    async fn get(&self, call_id: Id) -> Result<Option<GroupCall>>;

    /// Removes the call if its roster holds nobody, returning it when it did.
    ///
    /// A call with no participants is over; this is the sweep that retires it,
    /// run at the moment of the last leave rather than on a timer, because the
    /// last leave is the one event that always knows.
    async fn retire_if_empty(&self, call_id: Id) -> Result<Option<GroupCall>>;
}

/// A shared, fully erased group-call store.
pub type SharedGroupCallStore = Arc<dyn GroupCallStore>;

/// The in-memory group-call store: a map behind a lock.
///
/// One lock for the whole store, the same reasoning as the 1:1 backend: the
/// working set is one roster per live group call and the critical sections are
/// a clone and an insert. A node whose contention shows up here is a node
/// relaying more simultaneous group calls than the brief's ceiling allows
/// conversations to hold, and the rate limiter should notice that first.
#[derive(Debug, Default)]
pub struct MemoryGroupCallStore {
    calls: Mutex<HashMap<Id, GroupCall>>,
}

impl MemoryGroupCallStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl GroupCallStore for MemoryGroupCallStore {
    async fn put(&self, call: &GroupCall) -> Result<()> {
        self.calls.lock().insert(call.call_id, call.clone());
        Ok(())
    }

    async fn get(&self, call_id: Id) -> Result<Option<GroupCall>> {
        Ok(self.calls.lock().get(&call_id).cloned())
    }

    async fn retire_if_empty(&self, call_id: Id) -> Result<Option<GroupCall>> {
        let mut calls = self.calls.lock();
        match calls.get(&call_id) {
            Some(call) if call.participants.is_empty() => Ok(calls.remove(&call_id)),
            _ => Ok(None),
        }
    }
}

/// The wire's roster line, under the service's own name.
pub type GroupParticipantWire = migo_protocol::CallSfuParticipant;

/// Projects a roster seat onto the wire.
///
/// The one place a [`GroupParticipant`] becomes a frame: the sealed offer moves
/// by reference and is never read, the same mail-slot promise every other
/// projection in this crate makes.
#[must_use]
pub fn participant_wire(participant: &GroupParticipant) -> GroupParticipantWire {
    GroupParticipantWire {
        user_id: participant.account_id,
        device_id: participant.device_id,
        joined_at: participant.joined_at,
        sealed_offer: participant.sealed_offer.clone(),
    }
}

/// The whole roster, in join order, as a joiner's reply carries it.
#[must_use]
pub fn roster_wire(call: &GroupCall) -> Vec<GroupParticipantWire> {
    call.participants.iter().map(participant_wire).collect()
}

/// The wire's `Connected` state, for the seat a join announcement names.
pub const GROUP_STATE_CONNECTED: u32 = 2;
/// The wire's `Ended` state, for the seat a departure announcement empties.
pub const GROUP_STATE_ENDED: u32 = 4;

/// Builds the join announcement the roster hears: `Connected`, naming the
/// joiner and the size after the change.
#[must_use]
pub fn group_join_event(
    call: &GroupCall,
    participant: &GroupParticipant,
) -> migo_protocol::CallStateEvent {
    migo_protocol::CallStateEvent {
        call_id: call.call_id,
        state: GROUP_STATE_CONNECTED,
        reason: None,
        conversation_id: Some(call.conversation_id),
        user_id: Some(participant.account_id),
        device_id: Some(participant.device_id),
        participant_count: Some(call.participants.len() as u32),
        sealed_offer: Some(participant.sealed_offer.clone()),
        participants: None,
    }
}

/// Builds the departure event the roster hears: `Ended` naming the leaver and
/// the size after the change — `Ended` because the wire's vocabulary has no
/// "left" state, and the reason slot carries
/// [`EndReason::ByCaller`](crate::model::EndReason::ByCaller)'s number
/// so a client rendering the optional reason still renders something honest
/// (the participant withdrew themselves).
#[must_use]
pub fn group_leave_event(
    call: &GroupCall,
    leaver: Id,
    device: Id,
    count: u32,
) -> migo_protocol::CallStateEvent {
    migo_protocol::CallStateEvent {
        call_id: call.call_id,
        state: GROUP_STATE_ENDED,
        reason: Some(crate::model::EndReason::ByCaller.to_wire()),
        conversation_id: Some(call.conversation_id),
        user_id: Some(leaver),
        device_id: Some(device),
        participant_count: Some(count),
        sealed_offer: None,
        participants: None,
    }
}
