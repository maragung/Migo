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
/// Four operations, deliberately few. A production backend gets to decide
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

    /// Ends every expired invite at `now`, returning the calls it retired.
    ///
    /// Idempotent by construction: a second sweep finds the calls it already
    /// ended in [`CallState::Ended`] and leaves them there. The caller decides
    /// what to do with the returned rows — the service turns them into
    /// `NoAnswer` state events, and a background task (when one exists)
    /// would publish them.
    async fn sweep_expired(&self, now: Timestamp) -> Result<Vec<Call>>;
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
}
