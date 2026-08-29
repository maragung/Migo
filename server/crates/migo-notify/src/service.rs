//! The service.
//!
//! # Five rules, in the order they are applied
//!
//! 1. **Nobody is told what they just did.** An event whose actor is its recipient is
//!    dropped before anything else happens.
//! 2. **A kind is stored only if it has nowhere else to live.** Messages, mentions,
//!    replies, voice notes, pending friend requests, and ringing calls all have durable
//!    state elsewhere; a row here would be a second copy of a count, and a second copy
//!    of a count is a count that disagrees.
//! 3. **A push is a wake-up.** The payload is a [`Wakeup`], which cannot hold a
//!    sentence, and the crate has no method that would let a caller add one.
//! 4. **A device that is connected is not woken.** It already has the event on its
//!    socket. This is the most common reason a push is not sent, and it is a success.
//! 5. **A wake-up withheld is never an error.** Coalesced, budgeted, stale, connected —
//!    all four are counted, returned in [`Delivery`], and reported as `Ok`. The event is
//!    stored and the badge is right either way, and a caller that got `RATE_LIMITED`
//!    from a gift would have no correct way to respond to it.
//!
//! # What is not here
//!
//! Deciding *whether* somebody should be told. A room announcement goes to members
//! because `migo-rooms` said so; a gift notification goes to the recipient because
//! `migo-economy` posted the transaction. This crate does not re-authorise events, and
//! it deliberately has no read access to membership or the social graph: two layer-3
//! crates that depend on each other are how a dependency graph becomes a cycle.
//!
//! The one consequence worth stating plainly: a caller that gets its recipient list
//! wrong will have this crate faithfully tell the wrong people. The recipient list is
//! part of the authorisation decision and it belongs with the crate that made it.

use std::sync::Arc;

use migo_cache::{Cache, CacheKey, SharedCache, Ttl};
use migo_core::metrics::Registry;
use migo_core::{Id, Random, Result, Timestamp};
use migo_protocol::{fault, NotificationKind, Platform};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter, TrustTier};
use migo_store::model::{notification_kind, Notification, PushTarget};
use migo_store::{SharedStore, Store};
use parking_lot::Mutex;

use crate::metrics::{Meters, RegistrationOutcome};
use crate::model::{
    Caller, Delivery, Event, Failure, Inbox, Item, NotifyConfig, RawToken, Wakeup, Withheld,
    MAX_INBOX_PAGE,
};
use crate::token::TokenKeeper;
use crate::traits::{Notifier, PushSender, Sent, SharedNotifier, SharedPushSender, Target};

/// Cache scope for the coalescing marks.
///
/// One key per device per kind. Not per account: coalescing at the account level would
/// mean a wake-up to a phone suppressing the one to a tablet, and the two are not the
/// same person's attention.
const COALESCE_SCOPE: &str = "notify_coalesce";

/// What reading a page of the inbox costs.
const INBOX_COST: u32 = 3;
/// What reading the badge costs.
///
/// Cheaper than the page because it is called on every app foreground, which is to say
/// several times an hour per device for a client that is behaving.
const BADGE_COST: u32 = 1;
/// What acknowledging costs.
const ACK_COST: u32 = 2;
/// What registering a push token costs.
///
/// The most expensive thing here, and the only one that writes a credential. A client
/// re-registering in a loop is either broken or probing, and either way five is enough
/// for the once-per-cold-start this is meant to serve.
const REGISTER_COST: u32 = 5;
/// What one wake-up costs a device's budget.
///
/// Charged against the *recipient's* device, and spending it does not refuse anything:
/// it downgrades the wake-up to [`Withheld::Budget`], leaving the inbox row and the
/// badge intact. The bucket is there to protect a phone's battery and the deployment's
/// provider quota from an event storm, not to punish somebody for being popular.
const WAKEUP_COST: u32 = 1;

/// Notifications, and the wake-ups behind them.
pub struct Notifications<
    S: ?Sized = dyn Store,
    C: ?Sized = dyn Cache,
    L: ?Sized = dyn RateLimiter,
    P: ?Sized = dyn PushSender,
> {
    store: Arc<S>,
    cache: Arc<C>,
    limiter: Arc<L>,
    sender: Arc<P>,
    /// Seals and hashes push tokens. See [`crate::token`].
    keeper: TokenKeeper,
    config: NotifyConfig,
    /// Mints notification ids.
    ///
    /// A `Mutex` around a boxed generator, matching every other service in the tree. The
    /// lock is taken, an id is produced, and the guard is dropped inside one statement —
    /// never held across an `await`, because a lock held across a yield point in an async
    /// runtime is a deadlock waiting for the right interleaving.
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
}

impl<S, C, L, P> core::fmt::Debug for Notifications<S, C, L, P>
where
    S: ?Sized,
    C: ?Sized,
    L: ?Sized,
    P: ?Sized,
{
    /// Prints the configuration and nothing else.
    ///
    /// Not derived, for the same reason [`TokenKeeper`] is not: this struct holds one,
    /// and a derived `Debug` would carry the deployment's sealing key into whatever
    /// formatted it. The config is the part somebody debugging delivery wants anyway —
    /// whether push is enabled, and how wide the coalescing window is.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Notifications")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl<S, C, L, P> Notifications<S, C, L, P>
where
    S: Store + ?Sized,
    C: Cache + ?Sized,
    L: RateLimiter + ?Sized,
    P: PushSender + ?Sized,
{
    /// Builds a service.
    ///
    /// `root_secret` is the deployment signing secret, from which the token sealing and
    /// lookup keys are derived. `migod` refuses to start in production with an empty or
    /// default secret, so this constructor does not have to check.
    pub fn new(
        store: Arc<S>,
        cache: Arc<C>,
        limiter: Arc<L>,
        sender: Arc<P>,
        random: Box<dyn Random>,
        root_secret: &[u8],
        config: NotifyConfig,
        registry: &Registry,
    ) -> Self {
        Self {
            store,
            cache,
            limiter,
            sender,
            keeper: TokenKeeper::derive(root_secret),
            config,
            random: Mutex::new(random),
            meters: Meters::new(registry),
        }
    }

    /// A time-ordered id for a row created at `at`.
    fn new_id(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, &mut **random)
    }

    /// Charges the caller's own budget for a read or a write it asked for.
    async fn charge(&self, caller: &Caller, cost: u32) -> Result<()> {
        self.limiter
            .charge(
                &[BucketKey::account_write(caller.account_id)],
                cost,
                caller.tier,
                caller.now,
            )
            .await?
            .into_result()
    }

    /// Refuses a caller that is not fully identified.
    ///
    /// Checked before the rate-limit charge, so an unauthenticated caller cannot spend a
    /// bucket it has no business reaching. A nil account or a nil device means the request
    /// did not arrive through an authenticated session, and reading or writing a
    /// notification for "nobody" is a bug upstream that must not reach the store.
    fn require_identity(caller: &Caller) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "notifications need an identified account and device",
            ));
        }
        Ok(())
    }

    /// Writes the inbox row for an event, if its kind belongs in the inbox.
    ///
    /// `Ok(false)` means the kind is delivered but not stored, which is the normal case
    /// for a message. A storage failure is propagated: a gift that was announced and not
    /// recorded is a gift the recipient cannot find later, and it is better for the crate
    /// that posted the transaction to know its announcement failed.
    async fn store_event(&self, event: &Event) -> Result<bool> {
        let kind = wire_kind(event.kind);
        if !notification_kind::is_storable(kind) {
            return Ok(false);
        }
        self.store
            .create_notification(Notification {
                notification_id: self.new_id(event.at),
                account_id: event.account_id,
                kind,
                room_id: event.room_id,
                actor_id: event.actor_id,
                subject_id: event.subject_id,
                created_at: event.at,
                read_at: None,
            })
            .await?;
        self.meters.stored(event.kind);
        Ok(true)
    }

    /// Which of an account's devices currently hold a live socket.
    ///
    /// Asked of the cache and not of `migo-presence`: the routing table is layer 2, and a
    /// layer-3 crate that depended on another layer-3 crate to answer this would be
    /// buying a dependency edge for information that is already one call away.
    ///
    /// A cache failure is not propagated. Brief section 173 requires that losing the
    /// cache lose nothing that matters, and the honest failure direction here is to
    /// assume nobody is connected: an extra wake-up to somebody already reading their
    /// messages is a wasted buzz, where a suppressed one is a message that never arrived.
    async fn connected_devices(&self, account_id: Id, now: Timestamp) -> Vec<Id> {
        if !self.config.skip_connected {
            return Vec::new();
        }
        match self.cache.routes_of_account(account_id, now).await {
            Ok(routes) => routes.into_iter().map(|route| route.device_id).collect(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "routing lookup failed; assuming no device is connected"
                );
                Vec::new()
            }
        }
    }

    /// Whether a wake-up of this kind to this device is inside the coalescing window.
    ///
    /// Implemented with `set_if_absent` rather than a read followed by a write, because
    /// two events arriving on two nodes at the same instant would both read "no mark" and
    /// both push. The cache's own atomicity is the only thing that makes this correct
    /// across a cluster, and a check-then-set here would be a bug that only appears under
    /// the load it exists to handle.
    ///
    /// On a cache failure the wake-up goes out. Section 173 again: a coalescing window
    /// that fails closed turns a Redis blip into silence.
    async fn claim_window(&self, device_id: Id, kind: NotificationKind, now: Timestamp) -> bool {
        if self.config.coalesce_window_ms == 0 {
            return true;
        }
        let key = CacheKey::new(
            COALESCE_SCOPE,
            &format!("{}:{}", device_id.to_text(), kind.to_wire()),
        );
        let ttl = Ttl::from_millis(self.config.coalesce_window_ms);
        match self.cache.set_if_absent(&key, &[], ttl, now).await {
            Ok(claimed) => claimed,
            Err(error) => {
                tracing::warn!(error = %error, "coalescing mark failed; waking anyway");
                true
            }
        }
    }

    /// Whether this device has budget left for a wake-up.
    ///
    /// Charged at [`TrustTier::Established`] because the tier that matters is the
    /// recipient's device's capacity to be buzzed, and that has nothing to do with how
    /// old the *sender's* account is. A cache failure allows the wake-up, for the same
    /// reason as [`Notifications::claim_window`].
    async fn claim_budget(&self, device_id: Id, now: Timestamp) -> bool {
        match self
            .limiter
            .charge(
                &[BucketKey::device(device_id)],
                WAKEUP_COST,
                TrustTier::Established,
                now,
            )
            .await
        {
            Ok(verdict) => verdict.is_allowed(),
            Err(error) => {
                tracing::warn!(error = %error, "wake-up budget check failed; waking anyway");
                true
            }
        }
    }

    /// Wakes one device, or records why it was not woken.
    ///
    /// Returns `None` when nothing was attempted, so the caller can tell "withheld" from
    /// "attempted and failed" without inspecting a status code. The order of the four
    /// checks is deliberate and is cheapest-first: staleness is a field already in hand,
    /// connectedness is a list already fetched, the coalescing mark is one cache write,
    /// and the budget is a token bucket. A device that is connected should not cost a
    /// bucket charge to discover.
    async fn wake(
        &self,
        target: &PushTarget,
        wakeup: &Wakeup,
        connected: &[Id],
        now: Timestamp,
    ) -> Option<Result<Sent>> {
        if self.stale(target, now) {
            self.meters.withheld(Withheld::Stale);
            return None;
        }
        if connected.contains(&target.device_id) {
            self.meters.withheld(Withheld::Connected);
            return None;
        }
        // Urgent kinds skip the window but not the budget. A ringing call has seconds of
        // usefulness and coalescing one is coalescing the only notification in the
        // product with a deadline; a device being flooded with call attempts is still a
        // device being flooded.
        if !wakeup.is_urgent() && !self.claim_window(target.device_id, wakeup.kind, now).await {
            self.meters.withheld(Withheld::Coalesced);
            return None;
        }
        if !self.claim_budget(target.device_id, now).await {
            self.meters.withheld(Withheld::Budget);
            return None;
        }
        if !self.sender.handles(target.registration.provider) {
            // No sender for this provider is a deployment that dropped a provider it
            // still has registrations for. Counted as stale rather than as an error,
            // because retiring those registrations is exactly the right cleanup.
            self.meters.withheld(Withheld::Stale);
            return None;
        }
        let token = match self.keeper.open(target.device_id, &target.registration) {
            Ok(token) => token,
            Err(error) => return Some(Err(error)),
        };
        let outcome = self
            .sender
            .send(
                Target {
                    device_id: target.device_id,
                    platform: target.platform,
                    provider: target.registration.provider,
                    token: &token,
                },
                wakeup,
            )
            .await;
        Some(outcome)
    }

    /// Whether a registration is older than the deployment believes tokens live.
    fn stale(&self, target: &PushTarget, now: Timestamp) -> bool {
        let age = now
            .as_millis()
            .saturating_sub(target.updated_at.as_millis());
        age > self.config.registration_ttl_ms
    }

    /// Wakes every sleeping device of one account.
    ///
    /// Sequential rather than concurrent. A fan-out here would be a burst of provider
    /// requests per event, and the provider is the rate-limited resource: five devices
    /// woken in parallel is five times the chance of a `Throttled` that then has to be
    /// retried anyway. Sequential also keeps the badge read to one query per account
    /// rather than one per device.
    async fn wake_devices(&self, event: &Event, badge: u32) -> Delivery {
        let mut delivery = Delivery::default();
        if !self.config.push_enabled {
            return delivery;
        }
        let targets = match self.store.push_targets(event.account_id).await {
            Ok(targets) => targets,
            Err(error) => {
                tracing::warn!(error = %error, "push targets unavailable; nobody woken");
                return delivery;
            }
        };
        if targets.is_empty() {
            return delivery;
        }
        let wakeup = Wakeup {
            kind: event.kind,
            room_id: event.room_id,
            subject_id: event.subject_id,
            badge,
            at: event.at,
        };
        let connected = self.connected_devices(event.account_id, event.at).await;
        for target in &targets {
            match self.wake(target, &wakeup, &connected, event.at).await {
                None => delivery.withheld += 1,
                Some(Ok(Sent::Delivered)) => {
                    delivery.woken += 1;
                    self.meters.woken(event.kind);
                }
                Some(Ok(Sent::Unregistered)) => {
                    delivery.failed += 1;
                    self.meters.failed(Failure::Unregistered);
                    self.retire(&target.registration.hash).await;
                }
                Some(Ok(Sent::Throttled)) => {
                    delivery.failed += 1;
                    self.meters.failed(Failure::Throttled);
                }
                Some(Err(error)) => {
                    delivery.failed += 1;
                    self.meters.failed(Failure::Error);
                    // The hash and not the token, and no account id: brief section 174.
                    tracing::warn!(
                        error = %error,
                        registration = %target.registration.hash,
                        "wake-up failed"
                    );
                }
            }
        }
        delivery
    }

    /// Forgets a registration a provider has declared dead.
    ///
    /// A failure here is logged and swallowed. The wake-up already failed, the caller is
    /// mid-fan-out, and the consequence of not retiring is one more wasted send next
    /// time — which the next failure will try to clean up again.
    async fn retire(&self, hash: &str) {
        match self.store.retire_push_hash(hash).await {
            Ok(true) => self.meters.registration(RegistrationOutcome::Retired),
            Ok(false) => {}
            Err(error) => tracing::warn!(
                error = %error,
                registration = %hash,
                "dead push registration could not be retired"
            ),
        }
    }

    /// The recipient's unread count, or zero if it cannot be read.
    ///
    /// Zero rather than an error, because the badge is decoration on a wake-up and the
    /// wake-up is the point. A push that arrives with a stale badge is a working push; a
    /// push that did not arrive because a count query timed out is not.
    async fn badge_for(&self, account_id: Id) -> u32 {
        match self.store.unread_notifications(account_id).await {
            Ok(count) => count,
            Err(error) => {
                tracing::warn!(error = %error, "badge count unavailable; sending zero");
                0
            }
        }
    }

    /// Stores and delivers one event, having already decided it is worth delivering.
    async fn deliver(&self, event: Event) -> Result<Delivery> {
        let stored = self.store_event(&event).await?;
        let badge = self.badge_for(event.account_id).await;
        let mut delivery = self.wake_devices(&event, badge).await;
        delivery.stored = stored;
        Ok(delivery)
    }
}

/// The wire number for a kind.
fn wire_kind(kind: NotificationKind) -> i16 {
    i16::try_from(kind.to_wire()).unwrap_or(0)
}

/// A stored row as a client reads it.
fn item_of(row: &Notification) -> Item {
    Item {
        notification_id: row.notification_id,
        kind: NotificationKind::from_wire(u32::try_from(row.kind).unwrap_or(0)),
        room_id: row.room_id,
        actor_id: row.actor_id,
        subject_id: row.subject_id,
        at: row.created_at,
        read: row.read_at.is_some(),
    }
}

#[async_trait::async_trait]
impl<S, C, L, P> Notifier for Notifications<S, C, L, P>
where
    S: Store + ?Sized,
    C: Cache + ?Sized,
    L: RateLimiter + ?Sized,
    P: PushSender + ?Sized,
{
    async fn notify(&self, event: Event) -> Result<Delivery> {
        self.meters.event(event.kind);
        // Two drops before anything is written. Telling somebody what they just did is
        // the most common notification bug in any product that has notifications, and an
        // `Unknown` kind is a newer build's event arriving at an older node — which must
        // not become a row whose meaning nobody can recover.
        if event.is_self_inflicted() || event.kind == NotificationKind::Unknown {
            self.meters.dropped();
            return Ok(Delivery::default());
        }
        if event.account_id.is_nil() {
            return Err(fault::validation("account_id", "must not be nil"));
        }
        self.deliver(event).await
    }

    async fn notify_many(&self, recipients: &[Id], event: Event) -> Result<Delivery> {
        let mut total = Delivery::default();
        for recipient in recipients {
            let one = Event {
                account_id: *recipient,
                ..event
            };
            // One recipient's failure does not stop the others. A room announcement to
            // four thousand members that aborts on the one member whose account was
            // deleted mid-fan-out would notify a prefix of the room and no more, and the
            // caller has no way to tell which prefix.
            match self.notify(one).await {
                Ok(delivery) => {
                    total.stored |= delivery.stored;
                    total.woken += delivery.woken;
                    total.withheld += delivery.withheld;
                    total.failed += delivery.failed;
                }
                Err(error) => {
                    total.failed += 1;
                    tracing::warn!(error = %error, "one recipient of a fan-out was not notified");
                }
            }
        }
        Ok(total)
    }

    async fn inbox(&self, caller: &Caller, limit: u16) -> Result<Inbox> {
        Self::require_identity(caller)?;
        self.charge(caller, INBOX_COST).await?;
        let rows = self
            .store
            .notifications(caller.account_id, limit.min(MAX_INBOX_PAGE))
            .await?;
        let unread = self.store.unread_notifications(caller.account_id).await?;
        self.meters.inbox_read();
        Ok(Inbox {
            items: rows.iter().map(item_of).collect(),
            unread,
        })
    }

    async fn badge(&self, caller: &Caller) -> Result<u32> {
        Self::require_identity(caller)?;
        self.charge(caller, BADGE_COST).await?;
        let count = self.store.unread_notifications(caller.account_id).await?;
        self.meters.badge_read();
        Ok(count)
    }

    async fn acknowledge(&self, caller: &Caller, through: Timestamp) -> Result<u32> {
        Self::require_identity(caller)?;
        self.charge(caller, ACK_COST).await?;
        let changed = self
            .store
            .mark_notifications_read(caller.account_id, through, caller.now)
            .await?;
        self.meters.acknowledged(changed);
        Ok(changed)
    }

    async fn register(&self, caller: &Caller, token: RawToken) -> Result<()> {
        Self::require_identity(caller)?;
        self.charge(caller, REGISTER_COST).await?;
        // The platform the client claims is recorded on the device row at sign-in, and
        // this is not the place to change it: a client that could rewrite its platform
        // here could make an Android handset receive an APNs payload shape.
        if token.platform() == Platform::Unknown {
            self.meters.registration(RegistrationOutcome::Rejected);
            return Err(fault::validation("platform", "must be known"));
        }
        let registration = {
            let mut random = self.random.lock();
            self.keeper
                .seal(caller.device_id, &token, &mut **random)
                .inspect_err(|_| self.meters.registration(RegistrationOutcome::Rejected))?
        };
        // `token` is dropped here, and with it the only copy of the raw credential this
        // process held. Everything below the call sees ciphertext and a hash.
        drop(token);
        self.store
            .set_push_registration(caller.device_id, registration, caller.now)
            .await
            .inspect_err(|_| self.meters.registration(RegistrationOutcome::Rejected))?;
        self.meters.registration(RegistrationOutcome::Registered);
        Ok(())
    }

    async fn unregister(&self, caller: &Caller) -> Result<()> {
        Self::require_identity(caller)?;
        // Not charged. Sign-out must not be refusable by a rate limiter: a client that
        // cannot unregister is a phone that keeps buzzing for an account somebody
        // deliberately left, and "you are doing that too often" is not an acceptable
        // answer to "stop notifying me".
        self.store.clear_push_registration(caller.device_id).await?;
        self.meters.registration(RegistrationOutcome::Unregistered);
        Ok(())
    }

    async fn sweep(&self, before: Timestamp, limit: u16) -> Result<u64> {
        let removed = self.store.purge_notifications(before, limit).await?;
        self.meters.swept(removed);
        Ok(removed)
    }
}

/// Builds the service for the composition root.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn open(
    store: SharedStore,
    cache: SharedCache,
    limiter: SharedRateLimiter,
    sender: SharedPushSender,
    random: Box<dyn Random>,
    root_secret: &[u8],
    config: NotifyConfig,
    registry: &Registry,
) -> SharedNotifier {
    Arc::new(Notifications::new(
        store,
        cache,
        limiter,
        sender,
        random,
        root_secret,
        config,
        registry,
    ))
}
