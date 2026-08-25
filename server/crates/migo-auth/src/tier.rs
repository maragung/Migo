//! Standing: how much rope an account gets.
//!
//! [`migo_ratelimit`] scales every bucket by a [`TrustTier`] and deliberately does not
//! compute one — a limiter that decided who deserved raised limits would need to know
//! about accounts, and then the abuse-control crate would depend on the identity crate
//! and the dependency graph would have a cycle in it the first time identity wanted to
//! be rate limited. So the tier is computed here, where the account row already is,
//! and handed to the limiter as a value.
//!
//! # Age is the only signal, on purpose
//!
//! Everything better than age — messages sent, reports received, payments settled,
//! whether a human has ever replied to them — lives in a crate that does not exist
//! yet. Reaching for those signals now would make this function a dependency magnet
//! and would put a slow query on the sign-in path.
//!
//! Age is a weak signal and it is honest about what it buys: it costs an attacker
//! *time*, and time is the one resource that cannot be parallelised. Registering a
//! thousand accounts is cheap; having a thousand ninety-day-old accounts requires
//! having started ninety days ago.
//!
//! # Why a bot tier is not reachable from here
//!
//! The bot tier carries the largest buckets in the shipped configuration. Nothing a
//! client says may reach it: not the platform in its device claim, not a flag in its
//! registration. A device that announces itself as [`migo_protocol::Platform::Bot`] is
//! a client claiming a five-fold rate limit increase, and the only correct response is
//! to ignore the claim. Bot sessions are minted by the bot path from a bot token, and
//! that path is `migo-bots`.

use migo_core::Timestamp;
use migo_ratelimit::TrustTier;
use migo_store::model::Account;

/// How long an account stays on probation.
///
/// Seven days. Long enough that a throwaway account registered for one spam run never
/// leaves probation, short enough that a real person who signs up on a Monday is a
/// full citizen by the following Monday and never has to wonder why the app felt slow
/// at first.
pub const PROBATION_MILLIS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// How long until an account earns raised limits.
///
/// Ninety days. This is the tier that doubles a bucket, so it is priced in a quarter
/// of a year rather than in a week.
pub const TRUSTED_MILLIS: i64 = 90 * 24 * 60 * 60 * 1_000;

/// The standing of an account at a moment in time.
///
/// `now` is a parameter rather than a clock read, like everywhere else in this
/// workspace: a tier that depended on the host clock could not be tested at a
/// boundary, and the boundaries are the whole behaviour.
///
/// A clock that has gone backwards relative to the account's creation yields
/// [`TrustTier::New`], which is the conservative reading. The alternative — treating a
/// negative age as very large — would hand a fresh account the trusted tier during a
/// clock skew incident.
#[must_use]
pub fn of_account(account: &Account, now: Timestamp) -> TrustTier {
    let age = now.saturating_since(account.created_at);
    if age >= u64::try_from(TRUSTED_MILLIS).unwrap_or(u64::MAX) {
        TrustTier::Trusted
    } else if age >= u64::try_from(PROBATION_MILLIS).unwrap_or(u64::MAX) {
        TrustTier::Established
    } else {
        TrustTier::New
    }
}
