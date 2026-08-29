//! The presence service.
//!
//! # The four invariants
//!
//! **A device's lifetime is longer than the heartbeat it was told to send.** The
//! gateway advertises `heartbeat_ms` in `Welcome`; this crate stores an entry that
//! survives [`MISSED_HEARTBEATS`](crate::model::MISSED_HEARTBEATS) of them. A
//! session on `UltraLowData` was told to heartbeat four times more slowly, so its
//! entries live four times longer — otherwise a client obeying the server's own
//! instruction would blink offline between two punctual heartbeats.
//!
//! **Invisible is enforced by not telling anyone, not by asking nicely.** Brief
//! section 14 says a client must not be trusted to hide its own presence. Every
//! frame this crate produces carries the *projected* state from
//! [`crate::state::visible_state`], and Invisible projects to Offline before a
//! frame exists. There is no code path that publishes Invisible.
//!
//! **Nothing is sent when nothing changed.** Brief section 156. Every mutating
//! method computes the account's visible state before and after its own write and
//! returns `None` when the two agree, which is the normal outcome of a heartbeat
//! and of every disconnect that is not the last one.
//!
//! **Presence is the only thing losing the cache loses.** Brief section 173. All
//! state here lives in the cache with a TTL; the store is touched for two things
//! only, both of them reads about somebody else's privacy settings, plus one write
//! that records when a device was last seen.
//!
//! # Why the minimum interval is advertised and not enforced here
//!
//! Brief section 159 asks for a server-side floor on how often one user's presence
//! may be republished. [`Cadence::min_interval_ms`](crate::model::Cadence) computes
//! it, and this crate deliberately does not apply it.
//!
//! A floor can be applied two ways. Dropping an update that arrives too soon is
//! cheap and wrong: the dropped update is frequently the *last* one, so a user who
//! goes Online and then Away within the window is left showing Online until
//! something unrelated moves. Holding it until the window closes is correct, and it
//! needs a timer, a pending slot per subject, and somewhere to flush from — which
//! is a coalescing queue, which is what the gateway already has (brief section 154,
//! `Coalescable` keyed by user id). Publishing the number and letting the component
//! with the queue apply it keeps one mechanism instead of two that can disagree.
//!
//! # What is deliberately absent
//!
//! No custom status. `PresenceUpdate` carries the field and this server refuses it
//! with `FEATURE_DISABLED` rather than accepting it into a presence entry, because a
//! custom status is expected to outlive a disconnect and everything in this crate
//! evaporates with the cache — storing it here would make section 173's "losing
//! Redis loses nothing but ephemeral state" quietly false. Its home is a profile
//! column, and `UserProfile.custom_status` in the IDL is already where it will be
//! read from.
//!
//! No away-detection. Nothing here decides that a user has gone idle: the client
//! knows whether its window has focus and this crate would be guessing from a
//! heartbeat interval. A server that promotes Online to Away on its own would also
//! have to demote it, which is a timer per session for a fact the client already
//! has.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::traits::PresenceCache;
use migo_cache::{Cache, PresenceEntry, SharedCache};
use migo_core::metrics::Registry;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{
    fault, BandwidthMode, Opcode, PresenceEvent, PresenceState, PresenceUpdate, RelationshipKind,
};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::Visibility;
use migo_store::traits::{AccountStore, DeviceStore, SocialStore};
use migo_store::{SharedStore, Store};

use crate::fanout::Fanout;
use crate::metrics::{LastSeenOutcome, Meters, SessionEvent, UpdateOutcome};
use crate::model::{
    cadence_for, Cadence, Caller, Detail, PresenceConfig, MAX_LAST_SEEN_LOOKUPS,
    MAX_SNAPSHOT_SUBJECTS,
};
use crate::state::{
    any_invisible, declared_state, entry_of, state_with, state_without, visible_state,
};
use crate::traits::Presence;

/// The presence service, behind its trait.
pub type SharedPresence = Arc<dyn Presence>;

/// Presence over a store, a cache, and a rate limiter.
///
/// Generic with `dyn` defaults, like every other service here: `migod` builds
/// `Presences<dyn Store, dyn Cache, dyn RateLimiter>` and pays one virtual call per
/// operation, while tests instantiate it over the in-memory backends and pay
/// nothing. The parameters are `?Sized` so the erased form is the default rather
/// than a special case.
pub struct Presences<S: ?Sized = dyn Store, C: ?Sized = dyn Cache, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    cache: Arc<C>,
    limiter: Arc<L>,
    config: PresenceConfig,
    meters: Meters,
}

/// Builds the presence service the composition root hands around.
///
/// Infallible, and it takes a [`PresenceConfig`] where messaging took none. The
/// difference is not taste: presence has one number it cannot invent, the heartbeat
/// the gateway advertises, and a default that silently disagreed with the gateway's
/// would produce presence that expires before it can be refreshed.
#[must_use]
pub fn open(
    store: SharedStore,
    cache: SharedCache,
    limiter: SharedRateLimiter,
    registry: &Registry,
    config: PresenceConfig,
) -> SharedPresence {
    Arc::new(Presences::new(store, cache, limiter, registry, config))
}

impl<S, C, L> Presences<S, C, L>
where
    S: AccountStore + DeviceStore + SocialStore + ?Sized,
    C: PresenceCache + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Assembles the service and registers every series at zero.
    pub fn new(
        store: Arc<S>,
        cache: Arc<C>,
        limiter: Arc<L>,
        registry: &Registry,
        config: PresenceConfig,
    ) -> Self {
        Self {
            store,
            cache,
            limiter,
            config,
            meters: Meters::new(registry),
        }
    }

    /// Charges one opcode against the caller's buckets, tightest first.
    ///
    /// Two buckets, not three. The device is not one of them: a per-device bucket
    /// would let a chatty laptop exhaust an allowance that the user's phone also
    /// needs, and a client that mints device ids would escape the account limit
    /// entirely.
    async fn charge(&self, caller: &Caller, opcode: Opcode) -> Result<()> {
        let keys = [
            BucketKey::endpoint_write_of_account(caller.account_id, opcode),
            BucketKey::account_write(caller.account_id),
        ];
        // `charge_opcode` and not `charge`: ADR-0006 puts the price on the opcode, so
        // naming the opcode makes it impossible to charge the wrong amount, and a
        // reprice in the IDL takes effect here without an edit.
        self.limiter
            .charge_opcode(&keys, opcode, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// Refuses a caller that is not fully identified.
    ///
    /// The gateway never produces one. It is checked anyway because the failure it
    /// prevents is silent and shared: a nil device id is the *same* cache field for
    /// every session of an account, so one such caller would overwrite the presence
    /// of every device the account really has.
    fn require_identity(caller: &Caller) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "presence needs an identified account and device",
            ));
        }
        Ok(())
    }

    /// Writes one device's state and says whether anyone needs to hear about it.
    ///
    /// `entries` is the account's presence as it was read a moment ago, and it is
    /// passed in rather than re-read so that "before" and "after" come from the same
    /// observation. Re-reading after the write would answer a question this call
    /// already knows the answer to, and would answer it from a state another device
    /// may have changed in between.
    async fn write(
        &self,
        caller: &Caller,
        state: PresenceState,
        entries: &[PresenceEntry],
    ) -> Result<Option<Fanout>> {
        let before = visible_state(entries, caller.now);
        let after = state_with(entries, caller.device_id, state, caller.now);
        let ttl = cadence_for(caller.mode, self.config.heartbeat_ms).presence_ttl();
        // `since` survives a refresh and survives a re-declaration of the same
        // state, so "online since 09:12" does not reset every heartbeat. It moves
        // only when the state itself moves, which is what the field means.
        let since = entry_of(entries, caller.device_id, caller.now)
            .filter(|entry| entry.state == state)
            .map_or(caller.now, |entry| entry.since);
        let entry = PresenceEntry {
            account_id: caller.account_id,
            device_id: caller.device_id,
            state,
            since,
            expires_at: ttl.deadline(caller.now),
        };
        self.cache.set_presence(entry, ttl, caller.now).await?;
        if before == after {
            return Ok(None);
        }
        self.meters.broadcast(after);
        Ok(Some(Fanout::about(
            caller.account_id,
            caller.device_id,
            event(caller.account_id, after, None),
        )))
    }

    /// The state a connecting or reviving device should take.
    fn arriving_state(&self, caller: &Caller, entries: &[PresenceEntry]) -> PresenceState {
        if any_invisible(entries, caller.device_id, caller.now) {
            PresenceState::Invisible
        } else {
            PresenceState::Online
        }
    }

    /// When a subject was last seen, if this viewer is allowed to know.
    ///
    /// Only ever called for a subject with no live presence entry at all — not
    /// merely one whose projected state is Offline. The distinction is invisibility:
    /// a hidden user is reported Offline, and answering "last seen four seconds ago"
    /// about them would undo the hiding with arithmetic.
    async fn last_seen_for(&self, caller: &Caller, subject: Id) -> Result<Option<Timestamp>> {
        let Some(profile) = self.store.profile(subject).await? else {
            self.meters.last_seen(LastSeenOutcome::Withheld);
            return Ok(None);
        };
        let allowed = match profile.show_last_seen {
            Visibility::Nobody => false,
            Visibility::Friends => self.are_friends(caller.account_id, subject).await?,
            Visibility::Everyone => true,
        };
        if !allowed {
            self.meters.last_seen(LastSeenOutcome::Withheld);
            return Ok(None);
        }
        // The newest of the subject's live devices. Revoked ones are excluded: a
        // device the user has thrown off their account should not keep answering
        // questions about them.
        let seen = self
            .store
            .devices_for_account(subject)
            .await?
            .into_iter()
            .filter(|device| device.revoked_at.is_none())
            .map(|device| device.last_seen_at)
            .max_by_key(|at| at.as_millis());
        match seen {
            Some(at) => {
                self.meters.last_seen(LastSeenOutcome::Disclosed);
                Ok(Some(at))
            }
            None => {
                self.meters.last_seen(LastSeenOutcome::Withheld);
                Ok(None)
            }
        }
    }

    /// Whether these two are accepted friends.
    ///
    /// A friend edge with no `accepted_at` is a pending request, and a pending
    /// request is not a relationship — treating it as one would let anybody read a
    /// `Friends`-only field by asking to be a friend.
    async fn are_friends(&self, viewer: Id, subject: Id) -> Result<bool> {
        let edge = self
            .store
            .relationship(viewer, subject, RelationshipKind::Friend)
            .await?;
        Ok(edge.is_some_and(|edge| edge.accepted_at.is_some()))
    }
}

/// One presence frame, with the two per-viewer fields left empty.
///
/// `custom_status` is empty because this server does not implement it; `last_seen`
/// is empty on every broadcast because it differs per viewer and a broadcast is
/// encoded once. The read path passes a value here; the write paths pass `None`.
fn event(user_id: Id, state: PresenceState, last_seen: Option<Timestamp>) -> PresenceEvent {
    PresenceEvent {
        user_id,
        state,
        custom_status: None,
        last_seen,
    }
}

#[async_trait]
impl<S, C, L> Presence for Presences<S, C, L>
where
    S: AccountStore + DeviceStore + SocialStore + ?Sized + Send + Sync,
    C: PresenceCache + ?Sized + Send + Sync,
    L: RateLimiter + ?Sized + Send + Sync,
{
    async fn connected(&self, caller: &Caller) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        let entries = self.cache.presence(caller.account_id, caller.now).await?;
        let state = self.arriving_state(caller, &entries);
        let fanout = self.write(caller, state, &entries).await?;
        self.meters.session(SessionEvent::Connected);
        Ok(fanout)
    }

    async fn heartbeat(&self, caller: &Caller) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        let entries = self.cache.presence(caller.account_id, caller.now).await?;
        let existing = entry_of(&entries, caller.device_id, caller.now).map(|entry| entry.state);
        let revived = existing.is_none();
        // A heartbeat from a device with no entry means the entry expired while the
        // socket stayed up — a paused mobile app, a suspended laptop. Recreating it
        // is right; guessing a state for it is not, so it arrives the same way a new
        // connection does, invisibility included.
        let state = existing.unwrap_or_else(|| self.arriving_state(caller, &entries));
        let fanout = self.write(caller, state, &entries).await?;
        self.meters.heartbeat(revived);
        Ok(fanout)
    }

    async fn set(&self, caller: &Caller, request: PresenceUpdate) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        // Shape first, and before the rate limiter: a malformed frame is a client
        // bug, and charging for it would let one broken build exhaust its user's
        // allowance and take their working devices down with it.
        if request.custom_status.is_some() {
            self.meters.update(UpdateOutcome::Unsupported);
            return Err(fault::feature_disabled("presence_custom_status"));
        }
        if request.state == PresenceState::Unknown {
            self.meters.update(UpdateOutcome::Invalid);
            return Err(fault::validation(
                "state",
                "not a presence state this build knows",
            ));
        }
        if let Err(error) = self.charge(caller, Opcode::PresenceSet).await {
            self.meters.update(UpdateOutcome::RateLimited);
            return Err(error);
        }

        let entries = self.cache.presence(caller.account_id, caller.now).await?;
        let fanout = self.write(caller, request.state, &entries).await?;
        self.meters.update(if fanout.is_some() {
            UpdateOutcome::Accepted
        } else {
            UpdateOutcome::Unchanged
        });
        Ok(fanout)
    }

    async fn disconnected(&self, caller: &Caller) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        let entries = self.cache.presence(caller.account_id, caller.now).await?;
        let before = visible_state(&entries, caller.now);
        let after = state_without(&entries, caller.device_id, caller.now);
        self.cache
            .clear_presence(caller.account_id, caller.device_id)
            .await?;
        // The one durable write this crate makes, and the only moment anything
        // records when a device was last seen. Authentication stamps it on the way
        // in; a heartbeat deliberately does not, because a row write per device per
        // heartbeat is a large amount of write amplification for a field rendered as
        // "last seen 2 hours ago".
        self.store
            .touch_device(caller.device_id, caller.now)
            .await?;
        self.meters.session(SessionEvent::Disconnected);
        if before == after {
            return Ok(None);
        }
        self.meters.broadcast(after);
        Ok(Some(Fanout::about(
            caller.account_id,
            caller.device_id,
            event(caller.account_id, after, None),
        )))
    }

    async fn snapshot(
        &self,
        caller: &Caller,
        of: &[Id],
        detail: Detail,
    ) -> Result<Vec<PresenceEvent>> {
        Self::require_identity(caller)?;
        // Deduplicated in the caller's order, and clamped rather than refused. A
        // repeated id would otherwise produce two rows for one person, which a
        // client would render as two contacts.
        let mut subjects: Vec<Id> = Vec::with_capacity(of.len().min(MAX_SNAPSHOT_SUBJECTS));
        for id in of {
            if id.is_nil() || subjects.contains(id) {
                continue;
            }
            subjects.push(*id);
            if subjects.len() == MAX_SNAPSHOT_SUBJECTS {
                break;
            }
        }
        if subjects.is_empty() {
            self.meters.snapshot(0);
            return Ok(Vec::new());
        }

        // One cache call for every subject. A loop here would be one round trip per
        // contact, which is the cost this method exists to avoid.
        let mut live: HashMap<Id, Vec<PresenceEntry>> = HashMap::new();
        for entry in self.cache.presence_many(&subjects, caller.now).await? {
            live.entry(entry.account_id).or_default().push(entry);
        }

        let mut events = Vec::with_capacity(subjects.len());
        let mut lookups = 0usize;
        for subject in &subjects {
            let entries = live.get(subject).map_or(&[][..], Vec::as_slice);
            if *subject == caller.account_id {
                // Asking about yourself is the one read that is not projected.
                events.push(event(*subject, declared_state(entries, caller.now), None));
                continue;
            }
            let state = visible_state(entries, caller.now);
            let nowhere = !entries.iter().any(|entry| !entry.is_expired(caller.now));
            let wants_last_seen = detail == Detail::WithLastSeen && nowhere;
            let affordable = wants_last_seen && lookups < MAX_LAST_SEEN_LOOKUPS;

            // The block check is skipped only when the answer cannot depend on it:
            // an offline subject whose last-seen is not being resolved reports
            // Offline either way, and a query nobody can observe is a query not
            // worth making.
            if (state != PresenceState::Offline || affordable)
                && self
                    .store
                    .is_blocked_either_way(caller.account_id, *subject)
                    .await?
            {
                events.push(event(*subject, PresenceState::Offline, None));
                continue;
            }

            let last_seen = if affordable {
                lookups += 1;
                self.last_seen_for(caller, *subject).await?
            } else {
                if wants_last_seen {
                    self.meters.last_seen(LastSeenOutcome::Skipped);
                }
                None
            };
            events.push(event(*subject, state, last_seen));
        }
        self.meters.snapshot(subjects.len());
        Ok(events)
    }

    async fn devices(&self, caller: &Caller) -> Result<Vec<PresenceEntry>> {
        Self::require_identity(caller)?;
        let mut entries = self.cache.presence(caller.account_id, caller.now).await?;
        entries.retain(|entry| !entry.is_expired(caller.now));
        Ok(entries)
    }

    fn cadence(&self, mode: BandwidthMode) -> Cadence {
        cadence_for(mode, self.config.heartbeat_ms)
    }
}
