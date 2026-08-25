//! The limiter contract.
//!
//! One trait, because there is one limiter. The gateway, the HTTP API, and the bot
//! runtime all take `Arc<dyn RateLimiter>` and none of them knows whether the buckets
//! are in Redis, in this process, or in a test double that always refuses.
//!
//! # Why the verdict is a value and not an error
//!
//! [`Verdict`] is returned as `Ok`, not as `Err`. A refusal is an outcome the caller has
//! to be able to act on before it turns into a response: the gateway counts it against
//! the session's misbehaviour budget, the HTTP layer needs it to set `Retry-After`, and a
//! background job may prefer to sleep and retry rather than fail. Making a refusal an
//! error would push all of that into error-handling code, where the only reasonable
//! thing to do with an `Err` is propagate it. [`Verdict::into_result`] is there for the
//! callers that genuinely do just want to propagate.

use async_trait::async_trait;
use migo_core::{Result, Timestamp};
use migo_protocol::{fault, Opcode};

use crate::policy::{Policies, TrustTier};
use crate::scope::{BucketKey, Scope};

/// What the limiter decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The operation costs nothing, so no bucket was touched.
    ///
    /// Distinct from `Allowed` on purpose. The zero-cost opcodes are the ones the server
    /// itself sends and the ones that only acknowledge — `ACK`, `ERROR`, every `*_EVENT`
    /// — and charging them would have the server rate limiting its own replies. A caller
    /// that wants to log a remaining balance can tell from the variant that there is no
    /// meaningful number to log.
    Free,
    /// Charged. `remaining` is the tightest surface's balance in whole tokens, which is
    /// the number a client should pace itself against.
    Allowed {
        /// Whole tokens left on the tightest surface charged.
        remaining: u32,
    },
    /// Refused by `scope`, which is the first surface that could not pay.
    Rejected {
        /// The surface that could not pay.
        scope: Scope,
        /// How long until it could, in milliseconds.
        retry_after_ms: u32,
    },
}

impl Verdict {
    /// Whether the operation may proceed.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Free | Self::Allowed { .. })
    }

    /// Balance on the tightest surface, when there was one.
    #[must_use]
    pub const fn remaining(&self) -> Option<u32> {
        match self {
            Self::Allowed { remaining } => Some(*remaining),
            Self::Free | Self::Rejected { .. } => None,
        }
    }

    /// How long to wait, when refused.
    #[must_use]
    pub const fn retry_after_ms(&self) -> Option<u32> {
        match self {
            Self::Rejected { retry_after_ms, .. } => Some(*retry_after_ms),
            Self::Free | Self::Allowed { .. } => None,
        }
    }

    /// Which surface refused, when one did.
    #[must_use]
    pub const fn rejected_by(&self) -> Option<Scope> {
        match self {
            Self::Rejected { scope, .. } => Some(*scope),
            Self::Free | Self::Allowed { .. } => None,
        }
    }

    /// The refusal as an error, for callers that only propagate.
    ///
    /// Produces `RATE_LIMITED` carrying the wait, which is what ADR-0006 requires the
    /// client to receive. The surface is deliberately *not* in the message: telling a
    /// caller which of seven buckets refused it tells an attacker which of seven buckets
    /// to work around. It is in the metric, where the operator can see it and the
    /// attacker cannot.
    pub fn into_result(self) -> Result<()> {
        match self {
            Self::Free | Self::Allowed { .. } => Ok(()),
            Self::Rejected { retry_after_ms, .. } => Err(fault::rate_limited(retry_after_ms)),
        }
    }
}

/// Charges buckets and says whether to proceed.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Charges `cost` to every key, stopping at the first that cannot pay.
    ///
    /// All the keys for one request go in one call, in the order they should be checked
    /// — cheapest surface to widest is a reasonable order, since the tightest is most
    /// likely to refuse and a refusal short-circuits the rest.
    ///
    /// A refusal does **not** refund the keys already charged. That looks unfair and is
    /// deliberate: a rejection that costs nothing makes flooding free, because an
    /// attacker whose every request is refused would pay for none of them and could
    /// keep the server busy indefinitely at no cost to their own budget. Paying for a
    /// refused request is what makes a flood self-limiting.
    ///
    /// Fails with `VALIDATION_FAILED` when `keys` is empty — a charge against no
    /// surface limits nothing, and returning "allowed" for it would turn a caller's
    /// missing key into an unmetered path nobody notices.
    async fn charge(
        &self,
        keys: &[BucketKey],
        cost: u32,
        tier: TrustTier,
        now: Timestamp,
    ) -> Result<Verdict>;

    /// The same, priced from the protocol IDL.
    ///
    /// The form nearly every caller wants: ADR-0006 puts the cost of an operation on the
    /// opcode, so a caller that names the opcode cannot get the price wrong, and a
    /// reprice in the IDL takes effect everywhere without an edit.
    async fn charge_opcode(
        &self,
        keys: &[BucketKey],
        opcode: Opcode,
        tier: TrustTier,
        now: Timestamp,
    ) -> Result<Verdict> {
        self.charge(keys, opcode.cost(), tier, now).await
    }

    /// A bucket's balance, without charging it.
    ///
    /// For the operator asking why a subject is being refused. Not for deciding whether
    /// to proceed — between the peek and the charge the answer can change.
    async fn peek(&self, key: &BucketKey, tier: TrustTier, now: Timestamp) -> Result<u32>;

    /// Refills a bucket. For an operator lifting a limit by hand, and for tests.
    async fn clear(&self, key: &BucketKey) -> Result<()>;

    /// The budgets in force, for the startup banner and the admin surface.
    fn policies(&self) -> &Policies;
}
