//! How big a bucket is.
//!
//! Kept apart from [`crate::scope`] so that the answer to "what is being limited" and
//! the answer to "how much" can be changed independently. An operator retuning a limit
//! must not have to touch key construction, because a change to key construction
//! silently resets every bucket in the system.
//!
//! # The shape of the calculation
//!
//! One base policy per class of caller, scaled twice:
//!
//! ```text
//! base(tier)  ×  tier factor  ×  scope factor  =  bucket
//! ```
//!
//! Both factors apply to capacity *and* to refill rate. Scaling capacity alone — which
//! is how ADR-0006 originally put it — changes how large a burst is tolerated but
//! leaves the sustained rate identical for everybody, so a brand new account would be
//! limited to a smaller burst and then allowed to send at a trusted account's rate
//! forever. The sustained rate is the part that matters for abuse; the burst is the part
//! that matters for feeling responsive.
//!
//! # Why there is no per-opcode policy
//!
//! Because the opcode's cost is in the protocol IDL (ADR-0006), and cost is the right
//! place for it: a limiter with a table of opcodes in it has to be edited every time the
//! protocol grows, and the edit is in a different repository area than the opcode. With
//! costs in the IDL a new opcode arrives already priced, and `make protocol-check`
//! refuses to let it arrive unpriced.

use migo_cache::model::BucketSpec;
use migo_core::config::RateLimitConfig;
use migo_core::Result;
use migo_protocol::{fault, AuthLevel, Opcode};

use crate::scope::Scope;

/// How much of an operator's configured budget survives a cache outage.
///
/// A quarter. The local fallback cannot be shared between nodes, so N nodes each
/// enforcing the configured limit would together enforce N times it — the limiter would
/// get *looser* during the outage, which is the opposite of what a degraded mode is
/// for. A quarter is not derived from the node count on purpose: a node that has lost
/// Redis has also lost the thing it would learn the node count from.
pub const FALLBACK_DIVISOR: u32 = 4;

/// Standing of the caller, as the limiter sees it.
///
/// Not a wire type. Nothing here crosses a socket — a client that could tell the server
/// which tier it belongs to would tell it `Trusted` — so it is deliberately absent from
/// the protocol IDL. It is computed server-side from account age and history by the
/// auth crate, and passed in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrustTier {
    /// No session yet. The default, because an unauthenticated caller is what every
    /// connection starts as and the tightest budget is the right thing to get wrong.
    #[default]
    Anonymous,
    /// Authenticated, but young. Where throwaway accounts live.
    New,
    /// Authenticated and unremarkable. The ordinary case.
    Established,
    /// Authenticated with a long clean history.
    Trusted,
    /// A bot identity.
    Bot,
}

impl TrustTier {
    /// Every tier, for validation and tests.
    pub const ALL: &'static [Self] = &[
        Self::Anonymous,
        Self::New,
        Self::Established,
        Self::Trusted,
        Self::Bot,
    ];

    /// Human-readable, for error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::New => "new",
            Self::Established => "established",
            Self::Trusted => "trusted",
            Self::Bot => "bot",
        }
    }

    /// The highest session state a caller at this tier can have reached.
    ///
    /// Used to work out which opcodes the tier is able to send at all, which is what
    /// makes startup validation precise rather than pessimistic: an anonymous caller
    /// cannot send `KEY_PUBLISH`, so an anonymous budget does not have to be large
    /// enough to pay for one.
    #[must_use]
    pub const fn auth_level(self) -> AuthLevel {
        match self {
            Self::Anonymous => AuthLevel::None,
            Self::New | Self::Established | Self::Trusted => AuthLevel::User,
            Self::Bot => AuthLevel::Bot,
        }
    }

    /// Capacity and rate multiplier, as a fraction.
    const fn factor(self) -> (u32, u32) {
        match self {
            // The anonymous and bot bases are already tier-specific, so scaling them
            // again would be applying the same judgement twice.
            Self::Anonymous | Self::Bot => (1, 1),
            Self::New => (1, 4),
            Self::Established => (1, 1),
            Self::Trusted => (2, 1),
        }
    }
}

/// Ordering of [`AuthLevel`], as a number.
///
/// Written out rather than derived from the enum's discriminant so that a variant added
/// to the IDL fails to compile here instead of silently ranking last.
const fn rank(level: AuthLevel) -> u8 {
    match level {
        AuthLevel::None => 0,
        AuthLevel::User => 1,
        AuthLevel::Bot => 2,
        AuthLevel::Server => 3,
    }
}

/// The most expensive opcode a caller at `level` is allowed to send.
///
/// Derived from the IDL, so a new opcode with a higher cost tightens validation on the
/// next `make protocol` without anybody remembering to come here.
#[must_use]
pub fn max_cost_at(level: AuthLevel) -> u32 {
    Opcode::ALL
        .iter()
        .filter(|opcode| rank(opcode.auth()) <= rank(level))
        .map(|opcode| opcode.cost())
        .max()
        .unwrap_or(0)
}

/// Capacity and rate multiplier for a surface, as a fraction.
///
/// The shared surfaces are wider because they are shared: a /24 may hold a university,
/// and a room may hold a thousand people, so a budget sized for one caller would refuse
/// a crowd doing nothing wrong. The endpoint surface is narrower because it is the only
/// one meant to be hit by a fraction of a caller's traffic — it exists to stop a whole
/// budget going on one operation, which requires it to run out before the budget does.
const fn scope_factor(scope: Scope) -> (u32, u32) {
    match scope {
        Scope::Ip => (4, 1),
        Scope::Room => (8, 1),
        Scope::Endpoint => (1, 2),
        Scope::Account | Scope::Device | Scope::Token | Scope::Bot => (1, 1),
    }
}

/// The three configured budgets, validated.
///
/// Cheap to copy and to resolve — every field is two `u32`s and `resolve` is four
/// multiplications — so the limiter holds one and calls it per key per request rather
/// than caching resolved specs. A cache of resolved specs would be a second place where
/// a limit lives, and the whole reason the shape is a parameter rather than stored state
/// is to have exactly one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policies {
    anonymous: BucketSpec,
    user: BucketSpec,
    bot: BucketSpec,
}

impl Policies {
    /// Reads the operator's configuration and checks it cannot lock anybody out.
    ///
    /// Fails with `VALIDATION_FAILED` naming the surface and tier when some resolvable
    /// bucket is too small to ever hold the most expensive operation that reaches it.
    /// Such a bucket does not rate limit — it refuses forever, and it refuses with a
    /// `retry_after_ms` that is a lie, because no amount of waiting would help. The
    /// check runs at startup so `migod` declines to boot rather than serving a
    /// permanent 429 on one opcode.
    pub fn from_config(config: &RateLimitConfig) -> Result<Self> {
        let policies = Self {
            anonymous: BucketSpec::new(config.anonymous_burst, config.anonymous_refill_per_second),
            user: BucketSpec::new(config.user_burst, config.user_refill_per_second),
            bot: BucketSpec::new(config.bot_burst, config.bot_refill_per_second),
        };
        policies.validate()?;
        Ok(policies)
    }

    /// The bucket for one surface and one caller.
    #[must_use]
    pub fn resolve(&self, scope: Scope, tier: TrustTier) -> BucketSpec {
        let (base, tier_factor) = if scope.scales_with_tier() {
            (self.base(tier), tier.factor())
        } else {
            // A shared surface is sized from the ordinary user's budget whoever is
            // asking, so that the limit a crowd meets does not depend on which member
            // of it happened to arrive first.
            (self.user, (1, 1))
        };
        let (scope_num, scope_den) = scope_factor(scope);
        let scaled = scale(base, tier_factor.0, tier_factor.1);
        scale(scaled, scope_num, scope_den)
    }

    /// The unscaled budget behind a tier.
    #[must_use]
    pub const fn base(&self, tier: TrustTier) -> BucketSpec {
        match tier {
            TrustTier::Anonymous => self.anonymous,
            TrustTier::New | TrustTier::Established | TrustTier::Trusted => self.user,
            TrustTier::Bot => self.bot,
        }
    }

    /// The same bucket, tightened for a cache outage.
    ///
    /// The capacity floor at `cost` is not a nicety. Without it a tightened bucket can
    /// end up smaller than the operation it has to pay for — an anonymous endpoint
    /// bucket of ten tokens divided by four holds two, and `AUTHENTICATE` costs ten —
    /// and a bucket that cannot afford an operation refuses it every time, forever. The
    /// outage would lock every user out of logging in, which is a worse failure than the
    /// one the fallback exists to survive. Flooring the capacity keeps the operation
    /// payable and puts the tightening entirely in the refill rate, which is where it
    /// belongs anyway: what a degraded node needs to limit is throughput, not the size
    /// of one request.
    ///
    /// The floor can never raise the fallback above the primary, because
    /// `Policies::validate` has already established that the primary's capacity is at
    /// least the largest cost that can reach it.
    #[must_use]
    pub fn degraded(&self, scope: Scope, tier: TrustTier, cost: u32) -> BucketSpec {
        let spec = self.resolve(scope, tier);
        BucketSpec::new(
            (spec.capacity() / FALLBACK_DIVISOR).max(cost),
            spec.refill_per_second() / FALLBACK_DIVISOR,
        )
    }

    fn validate(&self) -> Result<()> {
        for &tier in TrustTier::ALL {
            let ceiling = max_cost_at(tier.auth_level());
            for &scope in Scope::ALL {
                // A shared surface carries traffic from every tier, so it is checked
                // against the most expensive opcode any caller can send rather than
                // against the tier being iterated.
                let needed = if scope.scales_with_tier() {
                    ceiling
                } else {
                    max_cost_at(AuthLevel::Bot)
                };
                let spec = self.resolve(scope, tier);
                if spec.capacity() < needed {
                    return Err(fault::validation(
                        "rate_limit",
                        &format!(
                            "the {} bucket for a {} caller holds {} tokens, but the most \
                             expensive operation reaching it costs {needed}: that bucket would \
                             refuse the operation forever rather than limit it, so the \
                             configured burst is too small",
                            scope.label(),
                            tier.name(),
                            spec.capacity(),
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Default for Policies {
    /// The shipped defaults, which `Policies::validate` accepts. Panic-free because
    /// `RateLimitConfig::default` is checked by a test in this crate.
    fn default() -> Self {
        Self::from_config(&RateLimitConfig::default()).unwrap_or(Self {
            anonymous: BucketSpec::new(20, 5),
            user: BucketSpec::new(200, 50),
            bot: BucketSpec::new(500, 200),
        })
    }
}

/// `spec` times `num/den`, on both dimensions, never reaching zero.
///
/// [`BucketSpec::new`] clamps each dimension up to one, so a factor that would round a
/// small budget to nothing yields the smallest working bucket instead. A zero-capacity
/// bucket refuses everything and a zero-refill bucket never recovers; neither is a rate
/// limit.
fn scale(spec: BucketSpec, num: u32, den: u32) -> BucketSpec {
    debug_assert!(
        den > 0,
        "a scope or tier factor must have a non-zero divisor"
    );
    BucketSpec::new(
        spec.capacity().saturating_mul(num) / den.max(1),
        spec.refill_per_second().saturating_mul(num) / den.max(1),
    )
}
