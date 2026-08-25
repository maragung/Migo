//! The cache contract, as one suite that every backend has to pass.
//!
//! These are not tests of any one implementation. Each function pins something the
//! trait documentation promises: expiry is indistinguishable from absence, a counter
//! window is fixed rather than sliding, presence is per device, an unbind is refused
//! when another node owns the route. Two backends behind one set of traits are only
//! interchangeable if one set of statements is true of both, so the statements live
//! here, once, and both `memory_contract.rs` and `redis_contract.rs` run all of them.
//!
//! Nothing here may name a concrete backend. The moment a case needs to know which
//! one it is talking to, it has stopped being a contract and belongs in that
//! backend's own file.
//!
//! # Time
//!
//! The two backends read different clocks. The in-memory one believes the `now` it is
//! handed; Redis believes its own and is told a TTL. A case that only advanced `now`
//! would pass on memory and fail on Redis, and one that only slept would do the
//! reverse. So expiry cases call [`advance`], which does both, and use TTLs of a few
//! tens of milliseconds with a generous margin — long enough to survive a slow round
//! trip, short enough that the suite is not a coffee break.

#![allow(clippy::too_many_lines)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use migo_cache::key::{CacheKey, SCOPE_BUCKET, SCOPE_COUNTER, SCOPE_KV};
use migo_cache::model::{BucketSpec, PresenceEntry, SessionRoute, Ttl};
use migo_cache::traits::{MAX_PRESENCE_FANOUT, MAX_VALUE_BYTES};
use migo_cache::SharedCache;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{codes, PresenceState};

/// Handed out one per fixture, so that cases sharing one Redis cannot collide.
///
/// A counter rather than a hash of the case name: a counter cannot collide, where a
/// hash can and would present as a mysterious cross-case failure. The cost is that
/// the ids a case uses differ between runs, which nothing asserts on — cases assert
/// on relationships between ids, never on their values.
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);

/// A prefix no other run of this suite will use, mixed into every scope.
///
/// The counter alone does not isolate runs from each other, though a comment here once
/// claimed it did. Every process starts it at one, so the second run hands out exactly
/// the scope numbers the first run used. Most cases write with a sixty-second TTL and do
/// not delete afterwards — deliberately, because a contract case should be about the
/// contract and not about tidying up — so a suite re-run inside that minute can read back
/// a value the *previous* run left under what it believes is its own private key. That
/// failure is rare enough to be filed as flakiness and reproducible enough to cost an
/// afternoon. `migo-store`'s contract avoids it by dropping and recreating the database;
/// a shared Redis cannot be flushed (the URL might one day point somewhere real), so the
/// run is stamped instead.
///
/// The low twenty bits are left to the counter, which is room for a million cases, and
/// keeps the case number legible in a failure message.
fn run_stamp() -> u64 {
    static STAMP: OnceLock<u64> = OnceLock::new();
    *STAMP.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);
        // The process id separates two runs that started inside the same nanosecond,
        // which sounds impossible until one `cargo test` launches several test binaries
        // against the same server at once.
        (nanos ^ u64::from(std::process::id())) << 20
    })
}

/// One case's cache, plus a private corner of the keyspace to use it in.
pub struct Fixture {
    cache: SharedCache,
    scope: u64,
}

impl Fixture {
    /// Claims the next free scope inside this run's stamp.
    pub fn new(cache: SharedCache) -> Self {
        Self {
            cache,
            scope: run_stamp() | NEXT_SCOPE.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The cache under test.
    pub fn cache(&self) -> &SharedCache {
        &self.cache
    }

    /// Id number `n` within this case. Distinct scopes give disjoint id ranges, so
    /// `f.id(1)` in one case is never `f.id(1)` in another.
    pub fn id(&self, n: u64) -> Id {
        Id::from((u128::from(self.scope) << 64) | u128::from(n))
    }

    /// A key-value key inside this case's corner.
    pub fn key(&self, tail: &str) -> CacheKey {
        CacheKey::new(SCOPE_KV, &format!("{}/{tail}", self.scope))
    }

    /// A counter key inside this case's corner.
    pub fn counter(&self, tail: &str) -> CacheKey {
        CacheKey::new(SCOPE_COUNTER, &format!("{}/{tail}", self.scope))
    }

    /// A token bucket key inside this case's corner.
    pub fn bucket(&self, tail: &str) -> CacheKey {
        CacheKey::new(SCOPE_BUCKET, &format!("{}/{tail}", self.scope))
    }
}

/// The instant every case starts from. A round number, so a failure message reads.
fn start() -> Timestamp {
    Timestamp::from_millis(1_000_000)
}

/// Moves the caller's clock and lets the same span of real time pass.
///
/// See the module note on time for why it has to be both.
async fn advance(now: &mut Timestamp, millis: u64) {
    *now = now.saturating_add_millis(millis as i64);
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

/// A TTL short enough to wait out.
fn brief() -> Ttl {
    Ttl::from_millis(80)
}

/// Long enough that nothing expires during a case that is not about expiry.
fn ample() -> Ttl {
    Ttl::from_seconds(60)
}

/// Asserts that a call failed with one specific protocol code.
///
/// Comparing the code rather than the message means a reworded internal string does
/// not break a test, while a change of failure *class* does.
#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    match result {
        Ok(_) => panic!("expected error {code}, got success"),
        Err(error) => assert_eq!(
            error.code(),
            code,
            "wrong code, internal message was: {}",
            error.internal_message()
        ),
    }
}

/// Builds a presence entry for `device`, alive until `ttl` after `now`.
fn presence_of(
    f: &Fixture,
    account: u64,
    device: u64,
    state: PresenceState,
    now: Timestamp,
    ttl: Ttl,
) -> PresenceEntry {
    PresenceEntry {
        account_id: f.id(account),
        device_id: f.id(device),
        state,
        since: now,
        expires_at: ttl.deadline(now),
    }
}

/// Builds a route for `device` on `node`, alive until `ttl` after `now`.
fn route_of(
    f: &Fixture,
    account: u64,
    device: u64,
    node: &str,
    now: Timestamp,
    ttl: Ttl,
) -> SessionRoute {
    SessionRoute {
        account_id: f.id(account),
        device_id: f.id(device),
        node_id: node.to_string(),
        connected_at: now,
        expires_at: ttl.deadline(now),
    }
}

// --- key/value ------------------------------------------------------------

pub async fn a_value_reads_back_exactly_as_written(f: &Fixture) {
    let now = start();
    let key = f.key("greeting");
    // Bytes, not text: values are opaque to the cache and must survive anything,
    // including the ones a string type would mangle.
    let value = &[0x00, 0xff, b'\n', 0x80, b':'];
    f.cache().set(&key, value, ample(), now).await.unwrap();
    assert_eq!(
        f.cache().get(&key, now).await.unwrap().as_deref(),
        Some(&value[..])
    );
}

pub async fn a_missing_key_is_absent_rather_than_an_error(f: &Fixture) {
    let now = start();
    assert_eq!(
        f.cache().get(&f.key("never-written"), now).await.unwrap(),
        None
    );
}

pub async fn writing_again_replaces_the_value(f: &Fixture) {
    let now = start();
    let key = f.key("overwritten");
    f.cache().set(&key, b"first", ample(), now).await.unwrap();
    f.cache().set(&key, b"second", ample(), now).await.unwrap();
    assert_eq!(
        f.cache().get(&key, now).await.unwrap().as_deref(),
        Some(&b"second"[..])
    );
}

pub async fn an_expired_value_is_indistinguishable_from_an_absent_one(f: &Fixture) {
    let mut now = start();
    let key = f.key("short-lived");
    f.cache().set(&key, b"here", brief(), now).await.unwrap();
    assert!(f.cache().get(&key, now).await.unwrap().is_some());
    advance(&mut now, 300).await;
    assert_eq!(f.cache().get(&key, now).await.unwrap(), None);
}

pub async fn a_rewrite_restarts_the_lifetime(f: &Fixture) {
    let mut now = start();
    let key = f.key("kept-alive");
    f.cache().set(&key, b"here", brief(), now).await.unwrap();
    advance(&mut now, 50).await;
    // Rewritten before it expired: the entry must live a fresh `brief()` from here,
    // not the remainder of the first one.
    f.cache().set(&key, b"here", brief(), now).await.unwrap();
    advance(&mut now, 50).await;
    assert!(
        f.cache().get(&key, now).await.unwrap().is_some(),
        "a rewrite must reset the lifetime, not top up the old one"
    );
}

pub async fn set_if_absent_is_won_by_exactly_one_caller(f: &Fixture) {
    let now = start();
    let key = f.key("lock");
    assert!(f
        .cache()
        .set_if_absent(&key, b"mine", ample(), now)
        .await
        .unwrap());
    assert!(
        !f.cache()
            .set_if_absent(&key, b"yours", ample(), now)
            .await
            .unwrap(),
        "the second caller must lose"
    );
    assert_eq!(
        f.cache().get(&key, now).await.unwrap().as_deref(),
        Some(&b"mine"[..]),
        "and must not have overwritten the winner"
    );
}

pub async fn a_lock_is_free_again_once_it_expires(f: &Fixture) {
    let mut now = start();
    let key = f.key("expiring-lock");
    assert!(f
        .cache()
        .set_if_absent(&key, b"mine", brief(), now)
        .await
        .unwrap());
    advance(&mut now, 300).await;
    assert!(
        f.cache()
            .set_if_absent(&key, b"yours", brief(), now)
            .await
            .unwrap(),
        "a lock that outlives its holder's crash is a deadlock"
    );
}

pub async fn compare_and_set_refuses_when_the_value_moved(f: &Fixture) {
    let now = start();
    let key = f.key("cas");
    f.cache().set(&key, b"one", ample(), now).await.unwrap();
    assert!(!f
        .cache()
        .compare_and_set(&key, Some(b"stale"), b"two", ample(), now)
        .await
        .unwrap());
    assert!(f
        .cache()
        .compare_and_set(&key, Some(b"one"), b"two", ample(), now)
        .await
        .unwrap());
    assert_eq!(
        f.cache().get(&key, now).await.unwrap().as_deref(),
        Some(&b"two"[..])
    );
}

pub async fn compare_and_set_against_nothing_is_a_create(f: &Fixture) {
    let now = start();
    let key = f.key("cas-create");
    assert!(f
        .cache()
        .compare_and_set(&key, None, b"first", ample(), now)
        .await
        .unwrap());
    assert!(
        !f.cache()
            .compare_and_set(&key, None, b"again", ample(), now)
            .await
            .unwrap(),
        "expecting absence must fail once something is there"
    );
    assert!(
        !f.cache()
            .compare_and_set(&f.key("cas-absent"), Some(b"anything"), b"x", ample(), now)
            .await
            .unwrap(),
        "expecting a value must fail when the key is absent"
    );
}

pub async fn delete_reports_whether_there_was_anything_to_delete(f: &Fixture) {
    let now = start();
    let key = f.key("deleted");
    f.cache().set(&key, b"here", ample(), now).await.unwrap();
    assert!(f.cache().delete(&key).await.unwrap());
    assert!(!f.cache().delete(&key).await.unwrap());
    assert_eq!(f.cache().get(&key, now).await.unwrap(), None);
}

pub async fn an_oversized_value_is_reported_not_stored(f: &Fixture) {
    let now = start();
    let key = f.key("too-big");
    let value = vec![0u8; MAX_VALUE_BYTES + 1];
    // A validation failure rather than a cache failure: this one is the caller's bug,
    // and a caller that degrades past it would silently stop caching.
    expect_code(
        f.cache().set(&key, &value, ample(), now).await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(f.cache().get(&key, now).await.unwrap(), None);
    let at_the_limit = vec![0u8; MAX_VALUE_BYTES];
    f.cache()
        .set(&key, &at_the_limit, ample(), now)
        .await
        .expect("the limit itself must be allowed, or it is not the limit");
}

pub async fn the_namespaces_do_not_share_a_keyspace(f: &Fixture) {
    let now = start();
    // Same tail, different namespace. If the scopes leaked into each other the
    // counter would read the value the key-value write left behind.
    f.cache()
        .set(&f.key("same-tail"), b"kv", ample(), now)
        .await
        .unwrap();
    assert_eq!(
        f.cache().count(&f.counter("same-tail"), now).await.unwrap(),
        0
    );
    f.cache()
        .increment(&f.counter("same-tail"), 3, ample(), now)
        .await
        .unwrap();
    assert_eq!(
        f.cache()
            .get(&f.key("same-tail"), now)
            .await
            .unwrap()
            .as_deref(),
        Some(&b"kv"[..])
    );
}

// --- counters -------------------------------------------------------------

pub async fn a_counter_starts_at_zero_and_adds_up(f: &Fixture) {
    let now = start();
    let key = f.counter("adds-up");
    assert_eq!(f.cache().count(&key, now).await.unwrap(), 0);
    assert_eq!(
        f.cache()
            .increment(&key, 5, ample(), now)
            .await
            .unwrap()
            .value,
        5
    );
    assert_eq!(
        f.cache()
            .increment(&key, 3, ample(), now)
            .await
            .unwrap()
            .value,
        8
    );
    assert_eq!(f.cache().count(&key, now).await.unwrap(), 8);
}

pub async fn a_counter_window_is_fixed_not_extended_by_traffic(f: &Fixture) {
    let mut now = start();
    let key = f.counter("fixed-window");
    f.cache().increment(&key, 1, brief(), now).await.unwrap();
    // Traffic inside the window must not push the window out; if it did, a user who
    // keeps trying would never see their limit reset and `retry_after` would be a lie.
    advance(&mut now, 40).await;
    f.cache().increment(&key, 1, brief(), now).await.unwrap();
    advance(&mut now, 300).await;
    assert_eq!(
        f.cache().count(&key, now).await.unwrap(),
        0,
        "the window must have ended on schedule"
    );
    assert_eq!(
        f.cache()
            .increment(&key, 1, brief(), now)
            .await
            .unwrap()
            .value,
        1,
        "and the next increment starts a fresh one"
    );
}

pub async fn an_increment_says_when_the_window_ends(f: &Fixture) {
    let now = start();
    let key = f.counter("deadline");
    let counted = f.cache().increment(&key, 1, ample(), now).await.unwrap();
    let remaining = counted.expires_at - now;
    // Not an exact equality: Redis answers with the TTL it actually has left, which
    // is a round trip short of the full window. The claim being tested is that the
    // deadline is inside the window and in the future, which is what a caller needs
    // to compute `retry_after`.
    assert!(
        remaining > 0 && remaining <= i64::from(ample().as_millis()),
        "window deadline {remaining}ms away, expected 0..={}",
        ample().as_millis()
    );
}

pub async fn resetting_a_counter_clears_it(f: &Fixture) {
    let now = start();
    let key = f.counter("reset");
    f.cache().increment(&key, 9, ample(), now).await.unwrap();
    f.cache().reset(&key).await.unwrap();
    assert_eq!(f.cache().count(&key, now).await.unwrap(), 0);
    assert_eq!(
        f.cache()
            .increment(&key, 1, ample(), now)
            .await
            .unwrap()
            .value,
        1
    );
}

// --- token buckets --------------------------------------------------------

/// A bucket nobody has touched is full, so a subject that has been quiet costs the
/// cache nothing while it waits.
pub async fn an_untouched_bucket_is_full(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(10, 5);
    assert_eq!(
        f.cache()
            .peek_bucket(&f.bucket("fresh"), spec, now)
            .await
            .unwrap(),
        10
    );
}

pub async fn taking_tokens_leaves_the_rest(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(10, 1);
    let key = f.bucket("spend");
    let first = f.cache().take_tokens(&key, spec, 4, now).await.unwrap();
    assert!(first.taken);
    assert_eq!(first.remaining, 6);
    let second = f.cache().take_tokens(&key, spec, 6, now).await.unwrap();
    assert!(second.taken);
    assert_eq!(second.remaining, 0);
}

/// The refusal, and the two things it has to say: nothing was charged, and how long
/// the caller must wait for the same request to become affordable.
pub async fn an_empty_bucket_refuses_and_says_how_long(f: &Fixture) {
    let now = start();
    // 1 token per second, so one token is 1000 ms of waiting and the arithmetic in
    // the assertion is readable rather than derived.
    let spec = BucketSpec::new(2, 1);
    let key = f.bucket("refuse");
    assert!(
        f.cache()
            .take_tokens(&key, spec, 2, now)
            .await
            .unwrap()
            .taken
    );

    let refused = f.cache().take_tokens(&key, spec, 1, now).await.unwrap();
    assert!(!refused.taken);
    assert_eq!(refused.remaining, 0);
    assert_eq!(refused.retry_after_ms, 1000);
}

/// A refusal must not charge. Otherwise a client being refused digs its own hole
/// deeper with every retry and the advertised `retry_after` becomes a lie.
pub async fn a_refusal_charges_nothing(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(10, 1);
    let key = f.bucket("free-refusal");
    f.cache().take_tokens(&key, spec, 7, now).await.unwrap();

    for _ in 0..5 {
        let refused = f.cache().take_tokens(&key, spec, 9, now).await.unwrap();
        assert!(!refused.taken);
        assert_eq!(refused.remaining, 3, "a refusal moved the level");
        assert_eq!(refused.retry_after_ms, 6000);
    }
    // And the three that were there are still spendable.
    assert!(
        f.cache()
            .take_tokens(&key, spec, 3, now)
            .await
            .unwrap()
            .taken
    );
}

/// Waiting really does buy tokens back, on both a clock that is told and a clock that
/// ticks.
pub async fn a_bucket_refills_over_time(f: &Fixture) {
    let mut now = start();
    // 50 tokens per second: 100 ms buys 5, which is above the granularity of a sleep
    // and below the capacity, so the case tests refill rather than the cap.
    let spec = BucketSpec::new(20, 50);
    let key = f.bucket("refill");
    f.cache().take_tokens(&key, spec, 20, now).await.unwrap();
    assert!(
        !f.cache()
            .take_tokens(&key, spec, 5, now)
            .await
            .unwrap()
            .taken
    );

    advance(&mut now, 120).await;
    let after = f.cache().take_tokens(&key, spec, 5, now).await.unwrap();
    assert!(after.taken, "120 ms at 50/s must buy at least 5 tokens");
}

/// Refill stops at the brim. A bucket idle for an hour holds one bucketful, not an
/// hour's worth, or the limit would only apply to callers who never pause.
pub async fn refill_stops_at_capacity(f: &Fixture) {
    let mut now = start();
    let spec = BucketSpec::new(4, 1000);
    let key = f.bucket("cap");
    f.cache().take_tokens(&key, spec, 4, now).await.unwrap();

    // Far longer than the 4 ms this bucket needs to refill completely.
    advance(&mut now, 200).await;
    assert_eq!(
        f.cache().peek_bucket(&key, spec, now).await.unwrap(),
        4,
        "an idle bucket held more than its capacity"
    );
    assert!(
        f.cache()
            .take_tokens(&key, spec, 4, now)
            .await
            .unwrap()
            .taken
    );
    assert!(
        !f.cache()
            .take_tokens(&key, spec, 1, now)
            .await
            .unwrap()
            .taken
    );
}

/// Cost zero is a question, not a charge: it reports the level and changes nothing.
pub async fn taking_nothing_is_always_allowed(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(3, 1);
    let key = f.bucket("free");
    f.cache().take_tokens(&key, spec, 3, now).await.unwrap();
    let verdict = f.cache().take_tokens(&key, spec, 0, now).await.unwrap();
    assert!(verdict.taken, "a free operation must never be refused");
    assert_eq!(verdict.remaining, 0);
}

/// A price the bucket can never hold is a configuration fault, not a rate limit:
/// there is no honest `retry_after` for a wait that will not help.
pub async fn a_cost_above_capacity_is_reported_not_refused(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(5, 1);
    expect_code(
        f.cache()
            .take_tokens(&f.bucket("too-dear"), spec, 6, now)
            .await,
        codes::VALIDATION_FAILED,
    );
}

/// Peeking is read-only. It exists for an operator asking why a subject is being
/// refused, and an inspection that spends is worse than no inspection.
pub async fn peeking_does_not_charge(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(8, 1);
    let key = f.bucket("peek");
    f.cache().take_tokens(&key, spec, 3, now).await.unwrap();
    for _ in 0..3 {
        assert_eq!(f.cache().peek_bucket(&key, spec, now).await.unwrap(), 5);
    }
}

pub async fn clearing_a_bucket_refills_it(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(6, 1);
    let key = f.bucket("clear");
    f.cache().take_tokens(&key, spec, 6, now).await.unwrap();
    f.cache().clear_bucket(&key).await.unwrap();
    assert_eq!(f.cache().peek_bucket(&key, spec, now).await.unwrap(), 6);
}

/// Two subjects are two buckets. Obvious, and the failure it guards against —
/// everyone sharing one key because the tail was dropped — takes down a whole
/// deployment at once.
pub async fn buckets_are_separate_per_key(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(2, 1);
    let mine = f.bucket("mine");
    let yours = f.bucket("yours");
    f.cache().take_tokens(&mine, spec, 2, now).await.unwrap();
    assert!(
        !f.cache()
            .take_tokens(&mine, spec, 1, now)
            .await
            .unwrap()
            .taken
    );
    assert!(
        f.cache()
            .take_tokens(&yours, spec, 2, now)
            .await
            .unwrap()
            .taken
    );
}

/// The shape of a bucket is an argument, not stored state, so retuning a limit takes
/// effect on the next call with no migration and no stale copy anywhere.
///
/// Narrowing is the direction worth pinning. Widening is visible a moment later
/// anyway, as refill runs; narrowing has to bite immediately, or whatever the previous
/// wider policy had banked stays spendable until the clock happens to move.
pub async fn the_shape_is_the_callers_and_can_change_between_calls(f: &Fixture) {
    let now = start();
    let key = f.bucket("retune");
    let generous = BucketSpec::new(40, 1);
    assert_eq!(
        f.cache()
            .take_tokens(&key, generous, 1, now)
            .await
            .unwrap()
            .remaining,
        39
    );

    let narrow = BucketSpec::new(4, 1);
    assert_eq!(
        f.cache().peek_bucket(&key, narrow, now).await.unwrap(),
        4,
        "the level was not clamped to the narrowed capacity"
    );
    let verdict = f.cache().take_tokens(&key, narrow, 4, now).await.unwrap();
    assert!(verdict.taken);
    assert_eq!(verdict.remaining, 0);
}

/// Time running backwards must not mint tokens. It happens for real — an NTP step, or
/// two gateways whose clocks differ by a second — and a bucket that refilled on a
/// negative interval would hand an attacker a full bucket for the price of a clock
/// skew.
pub async fn a_clock_going_backwards_grants_nothing(f: &Fixture) {
    let now = start();
    let spec = BucketSpec::new(5, 1000);
    let key = f.bucket("backwards");
    f.cache().take_tokens(&key, spec, 5, now).await.unwrap();

    let earlier = Timestamp::from_millis(now.as_millis() - 60_000);
    let verdict = f.cache().take_tokens(&key, spec, 5, earlier).await.unwrap();
    assert!(
        !verdict.taken,
        "a timestamp from the past refilled the bucket"
    );
}

// --- presence -------------------------------------------------------------

pub async fn presence_is_per_device_not_per_account(f: &Fixture) {
    let now = start();
    let phone = presence_of(f, 1, 10, PresenceState::Online, now, ample());
    let laptop = presence_of(f, 1, 11, PresenceState::Away, now, ample());
    f.cache().set_presence(phone, ample(), now).await.unwrap();
    f.cache().set_presence(laptop, ample(), now).await.unwrap();

    let mut found = f.cache().presence(f.id(1), now).await.unwrap();
    found.sort_by_key(|entry| entry.device_id);
    assert_eq!(found.len(), 2, "two devices are two facts");
    assert_eq!(found[0], phone);
    assert_eq!(found[1], laptop);
}

pub async fn the_latest_report_for_a_device_wins(f: &Fixture) {
    let now = start();
    let first = presence_of(f, 1, 10, PresenceState::Online, now, ample());
    let second = PresenceEntry {
        state: PresenceState::Busy,
        ..first
    };
    f.cache().set_presence(first, ample(), now).await.unwrap();
    f.cache().set_presence(second, ample(), now).await.unwrap();
    let found = f.cache().presence(f.id(1), now).await.unwrap();
    assert_eq!(found, vec![second]);
}

pub async fn presence_for_an_unknown_account_is_empty(f: &Fixture) {
    let now = start();
    assert!(f.cache().presence(f.id(999), now).await.unwrap().is_empty());
}

pub async fn presence_expires_without_anybody_clearing_it(f: &Fixture) {
    let mut now = start();
    let entry = presence_of(f, 1, 10, PresenceState::Online, now, brief());
    f.cache().set_presence(entry, brief(), now).await.unwrap();
    assert_eq!(f.cache().presence(f.id(1), now).await.unwrap().len(), 1);
    advance(&mut now, 300).await;
    assert!(
        f.cache().presence(f.id(1), now).await.unwrap().is_empty(),
        "a device that stops sending heartbeats must fall out on its own"
    );
}

pub async fn clearing_presence_takes_effect_at_once(f: &Fixture) {
    let now = start();
    let phone = presence_of(f, 1, 10, PresenceState::Online, now, ample());
    let laptop = presence_of(f, 1, 11, PresenceState::Online, now, ample());
    f.cache().set_presence(phone, ample(), now).await.unwrap();
    f.cache().set_presence(laptop, ample(), now).await.unwrap();
    f.cache().clear_presence(f.id(1), f.id(10)).await.unwrap();
    let found = f.cache().presence(f.id(1), now).await.unwrap();
    assert_eq!(found, vec![laptop], "only the named device goes");
    // Clearing what is not there is a no-op, not an error: two gateways can both
    // notice the same disconnect.
    f.cache().clear_presence(f.id(1), f.id(10)).await.unwrap();
}

pub async fn presence_many_answers_for_several_accounts_at_once(f: &Fixture) {
    let now = start();
    for (account, device) in [(1u64, 10u64), (2, 20), (3, 30)] {
        let entry = presence_of(f, account, device, PresenceState::Online, now, ample());
        f.cache().set_presence(entry, ample(), now).await.unwrap();
    }
    let asked = [f.id(1), f.id(3), f.id(4)];
    let mut found = f.cache().presence_many(&asked, now).await.unwrap();
    found.sort_by_key(|entry| entry.account_id);
    assert_eq!(
        found.len(),
        2,
        "the account with no presence contributes none"
    );
    assert_eq!(found[0].account_id, f.id(1));
    assert_eq!(found[1].account_id, f.id(3));

    assert!(f.cache().presence_many(&[], now).await.unwrap().is_empty());
}

pub async fn presence_many_ignores_ids_past_the_fanout_limit(f: &Fixture) {
    let now = start();
    let entry = presence_of(f, 1, 10, PresenceState::Online, now, ample());
    f.cache().set_presence(entry, ample(), now).await.unwrap();

    // The one account with presence sits past the cut. Ignoring rather than
    // rejecting: a truncated contact list renders, a failed one does not.
    let mut asked: Vec<Id> = (0..MAX_PRESENCE_FANOUT as u64)
        .map(|n| f.id(100 + n))
        .collect();
    asked.push(f.id(1));
    assert_eq!(asked.len(), MAX_PRESENCE_FANOUT + 1);
    assert!(f
        .cache()
        .presence_many(&asked, now)
        .await
        .unwrap()
        .is_empty());

    // Inside the cut it is found, which is what makes the case above about the limit
    // rather than about the id.
    let inside = [f.id(1)];
    assert_eq!(
        f.cache().presence_many(&inside, now).await.unwrap().len(),
        1
    );
}

// --- typing ---------------------------------------------------------------

pub async fn typing_lists_everyone_currently_typing(f: &Fixture) {
    let now = start();
    let conversation = f.id(50);
    f.cache()
        .set_typing(conversation, f.id(1), ample(), now)
        .await
        .unwrap();
    f.cache()
        .set_typing(conversation, f.id(2), ample(), now)
        .await
        .unwrap();
    let mut typing = f.cache().typing(conversation, now).await.unwrap();
    typing.sort();
    assert_eq!(typing, vec![f.id(1), f.id(2)]);
    // A different conversation is a different list.
    assert!(f.cache().typing(f.id(51), now).await.unwrap().is_empty());
}

pub async fn typing_expires_on_its_own(f: &Fixture) {
    let mut now = start();
    let conversation = f.id(50);
    f.cache()
        .set_typing(conversation, f.id(1), brief(), now)
        .await
        .unwrap();
    assert_eq!(f.cache().typing(conversation, now).await.unwrap().len(), 1);
    advance(&mut now, 300).await;
    assert!(
        f.cache()
            .typing(conversation, now)
            .await
            .unwrap()
            .is_empty(),
        "a client that closes mid-word must stop showing as typing"
    );
}

pub async fn clearing_typing_takes_effect_at_once(f: &Fixture) {
    let now = start();
    let conversation = f.id(50);
    f.cache()
        .set_typing(conversation, f.id(1), ample(), now)
        .await
        .unwrap();
    f.cache()
        .set_typing(conversation, f.id(2), ample(), now)
        .await
        .unwrap();
    f.cache().clear_typing(conversation, f.id(1)).await.unwrap();
    assert_eq!(
        f.cache().typing(conversation, now).await.unwrap(),
        vec![f.id(2)]
    );
    f.cache().clear_typing(conversation, f.id(1)).await.unwrap();
}

// --- routing --------------------------------------------------------------

pub async fn a_route_points_at_the_node_that_bound_it(f: &Fixture) {
    let now = start();
    let route = route_of(f, 1, 10, "gateway-sg-1", now, ample());
    f.cache()
        .bind_session(route.clone(), ample(), now)
        .await
        .unwrap();
    assert_eq!(
        f.cache().route(f.id(10), now).await.unwrap(),
        Some(route.clone())
    );
    assert_eq!(
        f.cache().routes_of_account(f.id(1), now).await.unwrap(),
        vec![route]
    );
}

pub async fn an_unknown_device_has_no_route(f: &Fixture) {
    let now = start();
    assert_eq!(f.cache().route(f.id(999), now).await.unwrap(), None);
    assert!(f
        .cache()
        .routes_of_account(f.id(999), now)
        .await
        .unwrap()
        .is_empty());
}

pub async fn rebinding_moves_a_device_to_the_new_node(f: &Fixture) {
    let now = start();
    let old = route_of(f, 1, 10, "gateway-sg-1", now, ample());
    let new = route_of(f, 1, 10, "gateway-sg-2", now, ample());
    f.cache().bind_session(old, ample(), now).await.unwrap();
    f.cache()
        .bind_session(new.clone(), ample(), now)
        .await
        .unwrap();
    assert_eq!(
        f.cache().route(f.id(10), now).await.unwrap(),
        Some(new.clone()),
        "the old socket is already gone; last writer has to win"
    );
    assert_eq!(
        f.cache().routes_of_account(f.id(1), now).await.unwrap(),
        vec![new],
        "and the account index must not list the device twice"
    );
}

pub async fn every_device_of_an_account_is_routed_separately(f: &Fixture) {
    let now = start();
    let phone = route_of(f, 1, 10, "gateway-sg-1", now, ample());
    let laptop = route_of(f, 1, 11, "gateway-sg-2", now, ample());
    let stranger = route_of(f, 2, 20, "gateway-sg-1", now, ample());
    for route in [&phone, &laptop, &stranger] {
        f.cache()
            .bind_session(route.clone(), ample(), now)
            .await
            .unwrap();
    }
    let mut found = f.cache().routes_of_account(f.id(1), now).await.unwrap();
    found.sort_by_key(|route| route.device_id);
    assert_eq!(found, vec![phone, laptop]);
    assert_eq!(
        f.cache().routes_of_account(f.id(2), now).await.unwrap(),
        vec![stranger],
        "the fan-out set for a push must not include another account's sockets"
    );
}

pub async fn unbinding_is_refused_when_another_node_owns_the_route(f: &Fixture) {
    let now = start();
    let route = route_of(f, 1, 10, "gateway-sg-2", now, ample());
    f.cache()
        .bind_session(route.clone(), ample(), now)
        .await
        .unwrap();
    // The device reconnected to sg-2 while sg-1 was still cleaning up. An
    // unconditional unbind here strands a connected device.
    assert!(!f
        .cache()
        .unbind_session(f.id(10), f.id(1), "gateway-sg-1")
        .await
        .unwrap());
    assert_eq!(f.cache().route(f.id(10), now).await.unwrap(), Some(route));
}

pub async fn unbinding_removes_the_route_and_its_index_entry(f: &Fixture) {
    let now = start();
    let phone = route_of(f, 1, 10, "gateway-sg-1", now, ample());
    let laptop = route_of(f, 1, 11, "gateway-sg-1", now, ample());
    f.cache().bind_session(phone, ample(), now).await.unwrap();
    f.cache()
        .bind_session(laptop.clone(), ample(), now)
        .await
        .unwrap();
    assert!(f
        .cache()
        .unbind_session(f.id(10), f.id(1), "gateway-sg-1")
        .await
        .unwrap());
    assert_eq!(f.cache().route(f.id(10), now).await.unwrap(), None);
    assert_eq!(
        f.cache().routes_of_account(f.id(1), now).await.unwrap(),
        vec![laptop],
        "the index must lose the device too, or a push keeps aiming at a dead socket"
    );
    assert!(
        !f.cache()
            .unbind_session(f.id(10), f.id(1), "gateway-sg-1")
            .await
            .unwrap(),
        "unbinding twice is a no-op"
    );
}

pub async fn a_route_expires_without_being_unbound(f: &Fixture) {
    let mut now = start();
    let route = route_of(f, 1, 10, "gateway-sg-1", now, brief());
    f.cache().bind_session(route, brief(), now).await.unwrap();
    assert!(f.cache().route(f.id(10), now).await.unwrap().is_some());
    advance(&mut now, 300).await;
    assert_eq!(
        f.cache().route(f.id(10), now).await.unwrap(),
        None,
        "a gateway that dies without cleaning up must not hold the route forever"
    );
    assert!(f
        .cache()
        .routes_of_account(f.id(1), now)
        .await
        .unwrap()
        .is_empty());
}

// --- whole-cache ----------------------------------------------------------

pub async fn a_reachable_cache_reports_healthy(f: &Fixture) {
    f.cache().health().await.unwrap();
}

pub async fn sweeping_never_removes_a_live_entry(f: &Fixture) {
    let now = start();
    let key = f.key("survives-a-sweep");
    f.cache().set(&key, b"here", ample(), now).await.unwrap();
    let entry = presence_of(f, 1, 10, PresenceState::Online, now, ample());
    f.cache().set_presence(entry, ample(), now).await.unwrap();
    let route = route_of(f, 1, 10, "gateway-sg-1", now, ample());
    f.cache()
        .bind_session(route.clone(), ample(), now)
        .await
        .unwrap();

    // How many it drops is a backend's business — Redis expires keys itself and
    // answers zero. What both must promise is that nothing live goes.
    f.cache().sweep(now).await.unwrap();

    assert!(f.cache().get(&key, now).await.unwrap().is_some());
    assert_eq!(f.cache().presence(f.id(1), now).await.unwrap(), vec![entry]);
    assert_eq!(f.cache().route(f.id(10), now).await.unwrap(), Some(route));
}

/// Every case in the suite, named once.
///
/// Both runners expand this macro, so a case cannot be wired into one backend and
/// forgotten in the other — which is the failure this whole arrangement exists to
/// make impossible.
#[macro_export]
macro_rules! for_each_contract_case {
    ($case:ident) => {
        $case!(a_value_reads_back_exactly_as_written);
        $case!(a_missing_key_is_absent_rather_than_an_error);
        $case!(writing_again_replaces_the_value);
        $case!(an_expired_value_is_indistinguishable_from_an_absent_one);
        $case!(a_rewrite_restarts_the_lifetime);
        $case!(set_if_absent_is_won_by_exactly_one_caller);
        $case!(a_lock_is_free_again_once_it_expires);
        $case!(compare_and_set_refuses_when_the_value_moved);
        $case!(compare_and_set_against_nothing_is_a_create);
        $case!(delete_reports_whether_there_was_anything_to_delete);
        $case!(an_oversized_value_is_reported_not_stored);
        $case!(the_namespaces_do_not_share_a_keyspace);
        $case!(a_counter_starts_at_zero_and_adds_up);
        $case!(a_counter_window_is_fixed_not_extended_by_traffic);
        $case!(an_increment_says_when_the_window_ends);
        $case!(resetting_a_counter_clears_it);
        $case!(an_untouched_bucket_is_full);
        $case!(taking_tokens_leaves_the_rest);
        $case!(an_empty_bucket_refuses_and_says_how_long);
        $case!(a_refusal_charges_nothing);
        $case!(a_bucket_refills_over_time);
        $case!(refill_stops_at_capacity);
        $case!(taking_nothing_is_always_allowed);
        $case!(a_cost_above_capacity_is_reported_not_refused);
        $case!(peeking_does_not_charge);
        $case!(clearing_a_bucket_refills_it);
        $case!(buckets_are_separate_per_key);
        $case!(the_shape_is_the_callers_and_can_change_between_calls);
        $case!(a_clock_going_backwards_grants_nothing);
        $case!(presence_is_per_device_not_per_account);
        $case!(the_latest_report_for_a_device_wins);
        $case!(presence_for_an_unknown_account_is_empty);
        $case!(presence_expires_without_anybody_clearing_it);
        $case!(clearing_presence_takes_effect_at_once);
        $case!(presence_many_answers_for_several_accounts_at_once);
        $case!(presence_many_ignores_ids_past_the_fanout_limit);
        $case!(typing_lists_everyone_currently_typing);
        $case!(typing_expires_on_its_own);
        $case!(clearing_typing_takes_effect_at_once);
        $case!(a_route_points_at_the_node_that_bound_it);
        $case!(an_unknown_device_has_no_route);
        $case!(rebinding_moves_a_device_to_the_new_node);
        $case!(every_device_of_an_account_is_routed_separately);
        $case!(unbinding_is_refused_when_another_node_owns_the_route);
        $case!(unbinding_removes_the_route_and_its_index_entry);
        $case!(a_route_expires_without_being_unbound);
        $case!(a_reachable_cache_reports_healthy);
        $case!(sweeping_never_removes_a_live_entry);
    };
}
