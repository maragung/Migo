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
//! read, a leave that retires an empty call, and a seat retirement the store
//! decides on its own. The in-memory backend is the honest v1, the same choice
//! the 1:1 store makes: a call that outlives a node restart is nobody's
//! expectation, because the participants' own clients time the call and
//! re-join against a new node.
//!
//! # Why the seat retirement is the store's to take
//!
//! The sweep runs on a timer and every departure it returns is a membership
//! fact the roster hears exactly once, so the retirement of a seat must be
//! one decision, not the outcome of a race between two readers. A service
//! that read the roster, removed the grace-expired seats itself, and wrote
//! the roster back would let two overlapping sweeps retire the same seat
//! twice — two departure events for one death — and would let any other
//! writer's stale clone put the retired seat back, resurrection by last
//! write. [`GroupCallStore::retire_gone`] therefore decides *and* removes
//! under the store's own lock, and [`GroupCallStore::put`] refuses the exact
//! seat a retirement took: a stale clone still carrying it is corrected on
//! write, while a fresh join of the same device — a seat whose `joined_at`
//! the retirement never saw — is seated as the new seat it is.
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
use migo_core::{Id, Result, Timestamp};
use parking_lot::Mutex;

use crate::model::{GroupCall, GroupParticipant};

/// Group-call persistence, as the service needs it.
///
/// Four operations. [`GroupCallStore::put`] is the whole-roster upsert the
/// service's read-modify-write goes through — a backend that can make it
/// atomic should, because two racing joins are the traffic a group call
/// actually gets and the in-memory backend's one lock across the critical
/// section is the behaviour to match. [`GroupCallStore::retire_if_empty`]
/// retires a call whose last seat emptied, so an abandoned call does not sit
/// in the map forever holding the id a re-join would collide with.
/// [`GroupCallStore::retire_gone`] is the seat retirement the sweep asks
/// for, taken under the store's own lock so it is a decision rather than a
/// race.
#[async_trait]
pub trait GroupCallStore: Send + Sync {
    /// Writes the call, replacing whatever the id held.
    ///
    /// One seat this upsert must never write back: a seat
    /// [`GroupCallStore::retire_gone`] retired. A stale roster clone — the
    /// read half of a read-modify-write that began before the retirement —
    /// still carries the retired seat, and putting it would resurrect a
    /// death the roster already heard announced. The backend refuses the
    /// exact retired seat (the same device, at the same `joined_at`) while
    /// admitting the fresh join of the same device, which is a new seat the
    /// retirement never saw.
    async fn put(&self, call: &GroupCall) -> Result<()>;

    /// Reads the call, if it exists.
    async fn get(&self, call_id: Id) -> Result<Option<GroupCall>>;

    /// Removes the call if its roster holds nobody, returning it when it did.
    ///
    /// A call with no participants is over; this is the sweep that retires it,
    /// run at the moment of the last leave rather than on a timer, because the
    /// last leave is the one event that always knows.
    async fn retire_if_empty(&self, call_id: Id) -> Result<Option<GroupCall>>;

    /// Retires the seats whose grace window passed, atomically, returning
    /// what the sweep is owed for them.
    ///
    /// The grace window is the caller's knob, but the removal is the store's
    /// fact: a seat goes exactly when `now` is at or past its `gone_since`
    /// plus `grace_ms`, the decision and the removal happen under one lock,
    /// and the seats are returned to the one caller that took them — a
    /// second retirement for the same seat, from a racing sweep or a stale
    /// roster clone, finds nothing to take and announces nothing. The
    /// returned seats are in roster order, and a roster the retirement
    /// empties is stamped and retired in the same critical section, exactly
    /// as [`GroupCallStore::retire_if_empty`] would.
    async fn retire_gone(
        &self,
        call_id: Id,
        grace_ms: i64,
        now: Timestamp,
    ) -> Result<Vec<GroupParticipant>>;

    /// Every roster the store holds, for the sweep.
    ///
    /// The seat sweep cannot name the calls it must look at — a dead session's
    /// account may hold a seat in any of them — so the store hands over the
    /// whole working set and the caller asks [`GroupCallStore::retire_gone`]
    /// per call. The read is a candidate list, never the authority: whatever
    /// changed between the read and the retirement is the retirement's own
    /// lock to arbitrate. A backend that can answer "which rosters hold this
    /// account" in one indexed query should grow that question instead; the
    /// in-memory backend's clone of a handful of live calls is the behaviour
    /// to beat.
    async fn all(&self) -> Result<Vec<GroupCall>>;

    /// The ended group calls `account_id` held a seat in, newest first.
    ///
    /// The group half of the call history, and the reason it lives here rather
    /// than beside the 1:1 store's: a group call leaves [`GroupCallStore::all`]
    /// at the exact moment it becomes worth remembering, because the last
    /// leave retires the row. What is kept is therefore a second map, written
    /// by the retirements and read by this method, and what it keeps is not
    /// just the row — an emptied roster names nobody, so the accounts it
    /// seated travel with it, or the visibility question this method exists to
    /// answer would have no answer at all.
    ///
    /// `before` is the paging cursor, exclusive, and `conversation_id` scopes
    /// the read in the store for the reason the 1:1 store's twin gives: a
    /// filter applied after a limit hands a conversation-scoped screen a short
    /// page and a cursor that skips what it never saw.
    async fn history_for(
        &self,
        account_id: Id,
        conversation_id: Option<Id>,
        before: Option<Timestamp>,
        limit: usize,
    ) -> Result<Vec<GroupCallHistory>>;

    /// Drops ended rosters past their retention, returning how many it
    /// dropped.
    ///
    /// The same two limits the 1:1 store is pruned by, for the same reason:
    /// `retention_ms` bounds a quiet account by age, `keep_per_account` bounds
    /// a busy one by count, and a row over either party's budget goes for both
    /// parties at once.
    async fn prune_history(
        &self,
        now: Timestamp,
        retention_ms: i64,
        keep_per_account: usize,
    ) -> Result<usize>;
}

/// A group call whose roster emptied, kept because it is history.
///
/// The row alone is not enough and that is the whole shape of this type: by
/// the time a group call can be remembered, its roster holds nobody, so the
/// row cannot say who was in it — neither to an account asking for its own
/// history nor to the count a history line renders. The accounts are therefore
/// kept beside the row, in the order they first joined.
#[derive(Debug, Clone)]
pub struct GroupCallHistory {
    /// The row as the last leave left it: `ended_at` stamped, roster empty.
    pub call: GroupCall,
    /// The distinct accounts the roster seated over its life, in first-join
    /// order.
    pub seated: Vec<Id>,
}

/// A shared, fully erased group-call store.
pub type SharedGroupCallStore = Arc<dyn GroupCallStore>;

/// The in-memory group-call store: the rosters and the sweep's tombstones
/// behind one lock.
///
/// One lock for the whole store, the same reasoning as the 1:1 backend: the
/// working set is one roster per live group call and the critical sections are
/// a clone and an insert. A node whose contention shows up here is a node
/// relaying more simultaneous group calls than the brief's ceiling allows
/// conversations to hold, and the rate limiter should notice that first.
#[derive(Debug, Default)]
pub struct MemoryGroupCallStore {
    rows: Mutex<GroupCallRows>,
}

/// What the lock guards: the live rosters, the seats the sweep retired, and
/// the rosters that have ended.
#[derive(Debug, Default)]
struct GroupCallRows {
    calls: HashMap<Id, GroupCall>,
    /// The seats [`GroupCallStore::retire_gone`] has taken, keyed by the
    /// `joined_at` that made each seat the seat it was.
    ///
    /// The pair is the whole test: a stale roster clone re-putting a retired
    /// seat carries the seat with its original `joined_at` — the exact pair
    /// the filter refuses — while a fresh join of the same device carries a
    /// `joined_at` the retirement never saw and goes through untouched. The
    /// tombstones outlive the row they came from, because the stale clone
    /// that could resurrect the seat is just as happy to re-create a removed
    /// row as to write into a live one; they grow once per retired seat for
    /// the life of the process, the same bounded growth the presence
    /// relay's per-subject table accepts.
    retired: HashMap<Id, HashMap<Id, Timestamp>>,
    /// The accounts each live roster has seated, accumulated across writes.
    ///
    /// Accumulated rather than read off the row, because the roster is exactly
    /// the thing that is gone by the time anyone asks: the service's last
    /// leave writes the roster with the leaver already removed, so the final
    /// seat is one no write ever shows as seated. This is the map that
    /// remembers it, and it is dropped into the history entry at retirement
    /// rather than kept for the life of the process.
    seated: HashMap<Id, Vec<Id>>,
    /// The calls whose roster emptied, which is every group call the history
    /// can answer about.
    history: HashMap<Id, GroupCallHistory>,
}

impl GroupCallRows {
    /// Moves a call out of the live map and into the history.
    ///
    /// The one place a group call stops being current and starts being past,
    /// called by both retirements so neither can forget: the row is dropped
    /// from `calls` in every case — that is what the callers asked for — and
    /// kept only if it carries an `ended_at`, because an order and a cursor
    /// built on when a call ended have no place for a row that never recorded
    /// one.
    fn retire(&mut self, call_id: Id) -> Option<GroupCall> {
        let call = self.calls.remove(&call_id)?;
        let seated = self.seated.remove(&call_id).unwrap_or_default();
        if call.ended_at.is_some() {
            self.history.insert(
                call_id,
                GroupCallHistory {
                    call: call.clone(),
                    seated,
                },
            );
        }
        Some(call)
    }

    /// The ended calls `account_id` was seated in, newest first.
    fn history_of(
        &self,
        account_id: Id,
        conversation_id: Option<Id>,
        before: Option<Timestamp>,
        limit: usize,
    ) -> Vec<GroupCallHistory> {
        let mut ended: Vec<GroupCallHistory> = self
            .history
            .values()
            .filter(|entry| {
                let Some(ended_at) = entry.call.ended_at else {
                    return false;
                };
                entry.seated.contains(&account_id)
                    && conversation_id.is_none_or(|id| entry.call.conversation_id == id)
                    && before.is_none_or(|cursor| !ended_at.is_at_or_after(cursor))
            })
            .cloned()
            .collect();
        ended.sort_by(|a, b| {
            b.call
                .ended_at
                .cmp(&a.call.ended_at)
                .then_with(|| b.call.call_id.cmp(&a.call.call_id))
        });
        ended.truncate(limit);
        ended
    }
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
        let mut rows = self.rows.lock();
        // The resurrection guard: a seat the sweep retired is gone, and the
        // only writer that could still name it is one whose read predates the
        // retirement. Such a clone carries the seat at the very `joined_at`
        // the tombstone recorded, so the pair identifies the dead seat
        // exactly where a fresh join's new `joined_at` does not.
        let mut seated = call.clone();
        if let Some(tombstones) = rows.retired.get(&call.call_id) {
            seated.participants.retain(|seat| {
                !tombstones
                    .get(&seat.device_id)
                    .is_some_and(|joined_at| *joined_at == seat.joined_at)
            });
        }
        if seated.participants.is_empty() {
            // An empty roster is an over call, whether the leaver emptied it
            // or the guard just did: the id is released for a fresh join, the
            // same release `retire_if_empty` performs. The moment the call
            // ended arrives on the caller's own copy and not on the stored
            // one, which still reads as live, so it is carried across before
            // the retirement reads the row it is about to keep: a store that
            // let the stored copy win would drop every group call that ended
            // when its last seat left, and a history missing exactly the
            // calls that used the ordinary way out is worse than no history.
            if let Some(stored) = rows.calls.get_mut(&call.call_id) {
                stored.ended_at = stored.ended_at.or(call.ended_at);
            }
            rows.retire(call.call_id);
        } else {
            // Recorded before the insert, and deduplicated, because the roster
            // that ends holds nobody and this is the only copy of who it held:
            // a device that leaves and re-joins is one account here, and the
            // joining order is the order the rows first arrived in.
            let accounts = rows.seated.entry(call.call_id).or_default();
            for seat in &seated.participants {
                if !accounts.contains(&seat.account_id) {
                    accounts.push(seat.account_id);
                }
            }
            rows.calls.insert(call.call_id, seated);
        }
        Ok(())
    }

    async fn get(&self, call_id: Id) -> Result<Option<GroupCall>> {
        Ok(self.rows.lock().calls.get(&call_id).cloned())
    }

    async fn retire_if_empty(&self, call_id: Id) -> Result<Option<GroupCall>> {
        let mut rows = self.rows.lock();
        let empty = rows
            .calls
            .get(&call_id)
            .is_some_and(|call| call.participants.is_empty());
        Ok(if empty { rows.retire(call_id) } else { None })
    }

    async fn retire_gone(
        &self,
        call_id: Id,
        grace_ms: i64,
        now: Timestamp,
    ) -> Result<Vec<GroupParticipant>> {
        // Rebound so the roster and the tombstones borrow as the disjoint
        // fields they are: a guard's `DerefMut` is opaque, and the removal
        // below writes a tombstone while the roster borrow is still live.
        let mut guard = self.rows.lock();
        let rows = &mut *guard;
        let mut retired = Vec::new();
        let mut emptied = false;
        {
            let Some(call) = rows.calls.get_mut(&call_id) else {
                return Ok(Vec::new());
            };
            let expired: Vec<usize> = call
                .participants
                .iter()
                .enumerate()
                .filter_map(|(index, seat)| {
                    seat.gone_since
                        .filter(|gone| now.is_at_or_after(gone.saturating_add_millis(grace_ms)))
                        .map(|_| index)
                })
                .collect();
            if expired.is_empty() {
                return Ok(Vec::new());
            }
            // Removed from the back so the indices collected against the
            // roster as it stood stay valid as it shrinks, and tombstoned as
            // they go so the `put` of any clone that still carries one is
            // corrected on write.
            for index in expired.iter().rev() {
                let seat = call.participants.remove(*index);
                rows.retired
                    .entry(call_id)
                    .or_default()
                    .insert(seat.device_id, seat.joined_at);
                retired.push(seat);
            }
            // Roster order, the order the join announcements taught the
            // roster to read departures in.
            retired.reverse();
            if call.participants.is_empty() {
                call.ended_at = Some(now);
                emptied = true;
            }
        }
        if emptied {
            // The tombstones stay: a stale clone is just as happy to
            // re-create a removed row as to write into a live one, and the
            // guard owes the seat its finality either way.
            rows.retire(call_id);
        }
        Ok(retired)
    }

    async fn all(&self) -> Result<Vec<GroupCall>> {
        Ok(self.rows.lock().calls.values().cloned().collect())
    }

    async fn history_for(
        &self,
        account_id: Id,
        conversation_id: Option<Id>,
        before: Option<Timestamp>,
        limit: usize,
    ) -> Result<Vec<GroupCallHistory>> {
        Ok(self
            .rows
            .lock()
            .history_of(account_id, conversation_id, before, limit))
    }

    async fn prune_history(
        &self,
        now: Timestamp,
        retention_ms: i64,
        keep_per_account: usize,
    ) -> Result<usize> {
        let horizon = now.saturating_add_millis(-retention_ms);
        let mut rows = self.rows.lock();
        let before = rows.history.len();
        rows.history.retain(|_, entry| {
            entry
                .call
                .ended_at
                .is_some_and(|ended_at| ended_at.is_at_or_after(horizon))
        });
        let mut dropped = before - rows.history.len();

        if keep_per_account > 0 {
            let mut kept: HashMap<Id, usize> = HashMap::new();
            let mut ended: Vec<(Id, Timestamp)> = rows
                .history
                .iter()
                .filter_map(|(id, entry)| entry.call.ended_at.map(|at| (*id, at)))
                .collect();
            ended.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
            for (call_id, _) in ended {
                let Some(seated) = rows.history.get(&call_id).map(|e| e.seated.clone()) else {
                    continue;
                };
                let over = seated
                    .iter()
                    .any(|party| kept.get(party).copied().unwrap_or(0) >= keep_per_account);
                if over {
                    rows.history.remove(&call_id);
                    dropped += 1;
                } else {
                    for party in seated {
                        *kept.entry(party).or_insert(0) += 1;
                    }
                }
            }
        }

        Ok(dropped)
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
/// "left" state, and the reason slot carries the departure's own truth:
/// [`EndReason::ByCaller`](crate::model::EndReason::ByCaller) for a leave the
/// participant sent, so a client rendering the optional reason still renders
/// something honest (the participant withdrew themselves), and
/// [`EndReason::Network`](crate::model::EndReason::Network) for the sweep's
/// retirement of a seat whose session died, so the same slot does not claim a
/// withdrawal nobody made.
#[must_use]
pub fn group_leave_event(
    call: &GroupCall,
    leaver: Id,
    device: Id,
    count: u32,
    reason: crate::model::EndReason,
) -> migo_protocol::CallStateEvent {
    migo_protocol::CallStateEvent {
        call_id: call.call_id,
        state: GROUP_STATE_ENDED,
        reason: Some(reason.to_wire()),
        conversation_id: Some(call.conversation_id),
        user_id: Some(leaver),
        device_id: Some(device),
        participant_count: Some(count),
        sealed_offer: None,
        participants: None,
    }
}
