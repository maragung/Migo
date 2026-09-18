//! Where call state lives, and the in-memory backend that stands in for it.
//!
//! # Why calls get their own store and not a `migo-store` table
//!
//! A call row's useful life is one ring plus one conversation — seconds, not
//! days — and the interesting events around it (an invite retried, a second
//! device answering) are races between two requests that a SQL round trip
//! apiece turns into a real window. The trait here is the shape a production
//! backend has to hold: a keyed upsert, a keyed read, a "what is ringing for
//! this account" scan, and a sweep that retires expired invites atomically.
//! The in-memory backend is the honest v1: it is single-process, it loses
//! nothing the brief mourns (an in-flight ring does not survive a node
//! restart in any design, because the client times the ring itself), and it
//! keeps the interface honest until the real backend arrives.
//!
//! # What the store never sees
//!
//! Sealed SDP and ICE. Not because the store would read them, but because
//! there is nothing for a store to do with them: the relay is a routing
//! decision made against the call row, and the bytes pass straight through.
//! Storing them would be keeping a copy of ciphertext whose only key holder
//! is a device, for no consumer, on the server's disk.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use parking_lot::Mutex;

use crate::model::{Call, CallState, EndReason};

/// Call persistence, as the service needs it.
///
/// Five operations, deliberately few. A production backend gets to decide
/// what "expired" means in the face of clock skew (the sweep is handed `now`,
/// never `Timestamp::now()`), and gets to make [`CallStore::put`] an upsert
/// whose read-modify-write races are its own to resolve — the in-memory
/// backend holds a lock across the whole critical section, which is the
/// behaviour to match.
#[async_trait]
pub trait CallStore: Send + Sync {
    /// Writes the call, replacing whatever the id held.
    ///
    /// The service always does its read-modify-write through this method; a
    /// backend that can make that atomic should, because two racing answers
    /// or a cancel against a decline is the traffic a call server actually
    /// gets.
    async fn put(&self, call: &Call) -> Result<()>;

    /// Reads the call, if it exists.
    async fn get(&self, call_id: Id) -> Result<Option<Call>>;

    /// The calls still live for `callee_id` at `now`.
    ///
    /// A ringing-but-expired invite is not active — the sweep's job is to
    /// retire it, and this read must not report a ring the deadline has
    /// already killed. Ordered by deadline, so the caller (a "line busy"
    /// check, a call-waiting screen) sees the most urgent ring first.
    async fn active_for_callee(&self, callee_id: Id, now: Timestamp) -> Result<Vec<Call>>;

    /// The rings `caller_id` placed that are still live at `now`.
    ///
    /// The caller's own side of [`CallStore::active_for_callee`], and the fifth
    /// read for one reason: a call list answers "which calls is this account
    /// in", and a ring the account placed from its phone is exactly such a
    /// call on its tablet. Ordered by deadline for the same reason, so a
    /// caller ringing two people sees the ring that expires first at the top.
    async fn active_for_caller(&self, caller_id: Id, now: Timestamp) -> Result<Vec<Call>>;

    /// The answered calls `account_id` is a party to, either side.
    ///
    /// `Connecting` and `Connected` only: a ring the account placed is the
    /// ring sweep's business (its own deadline retires it), and a ring the
    /// account is running from dies of `NoAnswer` without anybody's help.
    /// This is the read the disconnect path asks — which established calls
    /// just lost their party — and the answer is ordered by deadline for the
    /// same reason `active_for_callee` is, so a caller with several rows
    /// renders the most urgent first.
    async fn live_for(&self, account_id: Id) -> Result<Vec<Call>>;

    /// Ends every expired invite at `now`, returning the calls it retired.
    ///
    /// Idempotent by construction: a second sweep finds the calls it already
    /// ended in [`CallState::Ended`] and leaves them there. The caller decides
    /// what to do with the returned rows — the service turns them into
    /// `NoAnswer` state events, and a background task (when one exists)
    /// would publish them.
    async fn sweep_expired(&self, now: Timestamp) -> Result<Vec<Call>>;

    /// The ended calls `account_id` was a party to, newest first.
    ///
    /// The read behind the call history, and the one question the three scans
    /// above cannot answer: they are all filters on [`CallState`], and the row
    /// they exclude is exactly the row a history is made of. `before` is the
    /// paging cursor — a row must have ended *strictly* before it — and a row
    /// with no `ended_at` is not history yet, because a call whose ending was
    /// never stamped has no place in an order built on when things ended.
    ///
    /// `conversation_id` scopes the read in the store rather than in the caller
    /// for the reason `limit` makes necessary: filtering after a limit would
    /// hand a conversation-scoped screen a page shorter than it asked for, and
    /// the second page it then requested would start past rows it never saw. A
    /// backend that can answer "the ended calls of this account, newest first,
    /// ending before this instant" in one indexed query should; the in-memory
    /// backend's scan of a bounded map is the behaviour to beat.
    async fn history_for(
        &self,
        account_id: Id,
        conversation_id: Option<Id>,
        before: Option<Timestamp>,
        limit: usize,
    ) -> Result<Vec<Call>>;

    /// Drops ended rows past their retention, returning how many it dropped.
    ///
    /// The bound the map did not have. An ended row is worth keeping — it is
    /// the whole of the call history — but keeping every row forever is a store
    /// that grows with the traffic of a node's whole life, and the growth is
    /// invisible until it is the problem. Two limits, because either one alone
    /// leaves a hole: `retention_ms` bounds a quiet account's rows by age, and
    /// `keep_per_account` bounds a busy account's by count, so neither a long
    /// idle period nor a burst can grow the store without end.
    ///
    /// A row is dropped for both of its parties at once when either party is
    /// over the count: a call is one row, and half-deleting it would show one
    /// side a call the other side never had.
    async fn prune_history(
        &self,
        now: Timestamp,
        retention_ms: i64,
        keep_per_account: usize,
    ) -> Result<usize>;
}

/// A shared, fully erased call store.
pub type SharedCallStore = Arc<dyn CallStore>;

/// The in-memory call store: a map behind a lock.
///
/// One lock for the whole store rather than a shard map, because the working
/// set is one call row per in-flight ring and the critical sections are a
/// clone and an insert. Contention here would mean a node is relaying more
/// simultaneous rings than it has any business accepting, and the rate
/// limiter is the component that should notice that first.
#[derive(Debug, Default)]
pub struct MemoryCallStore {
    calls: Mutex<HashMap<Id, Call>>,
}

impl MemoryCallStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CallStore for MemoryCallStore {
    async fn put(&self, call: &Call) -> Result<()> {
        self.calls.lock().insert(call.call_id, call.clone());
        Ok(())
    }

    async fn get(&self, call_id: Id) -> Result<Option<Call>> {
        Ok(self.calls.lock().get(&call_id).cloned())
    }

    async fn active_for_callee(&self, callee_id: Id, now: Timestamp) -> Result<Vec<Call>> {
        let mut active: Vec<Call> = self
            .calls
            .lock()
            .values()
            .filter(|call| {
                call.callee_id == callee_id
                    && match call.state {
                        // A ring past its deadline is not a ring; the sweep
                        // will say so, and a busy check should not say it
                        // first.
                        CallState::Ringing => !now.is_at_or_after(call.expires_at),
                        CallState::Connecting | CallState::Connected => true,
                        CallState::Ended => false,
                    }
            })
            .cloned()
            .collect();
        active.sort_by_key(|call| (call.expires_at, call.call_id));
        Ok(active)
    }

    async fn active_for_caller(&self, caller_id: Id, now: Timestamp) -> Result<Vec<Call>> {
        let mut active: Vec<Call> = self
            .calls
            .lock()
            .values()
            .filter(|call| call.caller_id == caller_id && call.invite_is_live(now))
            .cloned()
            .collect();
        active.sort_by_key(|call| (call.expires_at, call.call_id));
        Ok(active)
    }

    async fn live_for(&self, account_id: Id) -> Result<Vec<Call>> {
        let mut live: Vec<Call> = self
            .calls
            .lock()
            .values()
            .filter(|call| {
                (call.caller_id == account_id || call.callee_id == account_id)
                    && matches!(call.state, CallState::Connecting | CallState::Connected)
            })
            .cloned()
            .collect();
        live.sort_by_key(|call| (call.expires_at, call.call_id));
        Ok(live)
    }

    async fn sweep_expired(&self, now: Timestamp) -> Result<Vec<Call>> {
        let mut calls = self.calls.lock();
        let expired: Vec<Call> = calls
            .values_mut()
            .filter(|call| {
                matches!(call.state, CallState::Ringing) && now.is_at_or_after(call.expires_at)
            })
            .map(|call| {
                call.state = CallState::Ended;
                call.end_reason = Some(EndReason::NoAnswer);
                call.ended_at = Some(now);
                call.clone()
            })
            .collect();
        Ok(expired)
    }

    async fn history_for(
        &self,
        account_id: Id,
        conversation_id: Option<Id>,
        before: Option<Timestamp>,
        limit: usize,
    ) -> Result<Vec<Call>> {
        let mut ended: Vec<Call> = self
            .calls
            .lock()
            .values()
            .filter(|call| {
                let Some(ended_at) = call.ended_at else {
                    // A row that has not been stamped is not history, however
                    // it is marked: an order built on the instant a call ended
                    // has no place to put a row whose instant is unknown.
                    return false;
                };
                call.state == CallState::Ended
                    && (call.caller_id == account_id || call.callee_id == account_id)
                    && conversation_id.is_none_or(|id| call.conversation_id == id)
                    // Strictly before: the cursor is the last row the caller
                    // already holds, so including it would repeat it on every
                    // page and a client paging to the end would never get there.
                    && before.is_none_or(|cursor| !ended_at.is_at_or_after(cursor))
            })
            .cloned()
            .collect();
        // Newest first, with the id breaking ties: two calls can end in the
        // same millisecond, and a page boundary falling between them must not
        // order them one way on this page and the other way on the next.
        ended.sort_by(|a, b| {
            b.ended_at
                .cmp(&a.ended_at)
                .then_with(|| b.call_id.cmp(&a.call_id))
        });
        ended.truncate(limit);
        Ok(ended)
    }

    async fn prune_history(
        &self,
        now: Timestamp,
        retention_ms: i64,
        keep_per_account: usize,
    ) -> Result<usize> {
        let horizon = now.saturating_add_millis(-retention_ms);
        let mut calls = self.calls.lock();

        // Age first, then count. The other order would let a burst of rows
        // that are about to age out push out rows that are not.
        let before = calls.len();
        calls.retain(|_, call| match call.ended_at {
            Some(ended_at) if call.state == CallState::Ended => ended_at.is_at_or_after(horizon),
            _ => true,
        });
        let mut dropped = before - calls.len();

        if keep_per_account > 0 {
            // One pass over the ended rows, newest first, counting each party
            // as it is seen: a row that overflows either party's budget is
            // struck, so a busy account cannot be trimmed out from under a
            // quiet one it happened to call.
            let mut kept: HashMap<Id, usize> = HashMap::new();
            let mut ended: Vec<(Id, Timestamp)> = calls
                .iter()
                .filter(|(_, call)| call.state == CallState::Ended && call.ended_at.is_some())
                .map(|(id, call)| (*id, call.ended_at.unwrap_or(call.expires_at)))
                .collect();
            ended.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
            for (call_id, _) in ended {
                let Some((caller_id, callee_id)) = calls
                    .get(&call_id)
                    .map(|call| (call.caller_id, call.callee_id))
                else {
                    continue;
                };
                let over = [caller_id, callee_id]
                    .iter()
                    .any(|party| kept.get(party).copied().unwrap_or(0) >= keep_per_account);
                if over {
                    calls.remove(&call_id);
                    dropped += 1;
                } else {
                    for party in [caller_id, callee_id] {
                        *kept.entry(party).or_insert(0) += 1;
                    }
                }
            }
        }

        Ok(dropped)
    }
}
