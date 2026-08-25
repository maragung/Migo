//! Inputs and limits that the messaging service works from.
//!
//! Two conventions, both inherited from the rest of the server.
//!
//! Every operation takes a [`Caller`], and the caller carries `now`. Nothing in
//! this crate reads a clock, which is what makes a disappearing-message deadline
//! or a typing expiry testable rather than hopeful (ADR-0009).
//!
//! Every limit is a named constant with the reasoning attached, not a literal at
//! a call site. A number nobody can explain is a number nobody dares change.

use migo_core::{Id, Timestamp};
use migo_ratelimit::TrustTier;

/// Who is calling, reduced to what messaging actually needs.
///
/// Deliberately **not** `migo_auth::RequestContext`. Authentication and messaging
/// are both layer-3 domain crates, and two of those may not depend on each other
/// (see `docs/01-architecture.md`): the day messaging imported an authentication
/// type, the dependency graph would stop being a layering and start being a
/// suggestion. The gateway holds both and translates at the edge, which is one
/// short function in the one place that is allowed to know about both.
///
/// The address is absent on purpose. Rate limiting here is per account and per
/// device, because the caller is authenticated and an account is a stronger
/// subject than a network; and brief section 174 forbids a full address in a log
/// line, so a field that never arrives cannot be logged by accident.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection this request arrived on.
    ///
    /// Load-bearing rather than decorative: it is stamped on the message so
    /// other devices can tell which of their own sent it, and it is the device
    /// excluded from fanout, because the sender's own connection gets an
    /// acknowledgement instead of a copy.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

impl Caller {
    /// A caller with the four facts every operation needs.
    #[must_use]
    pub fn new(account_id: Id, device_id: Id, tier: TrustTier, now: Timestamp) -> Self {
        Self {
            account_id,
            device_id,
            tier,
            now,
            request_id: None,
        }
    }

    /// Sets the correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

/// How long a typing mark survives without being refreshed.
///
/// Brief section 15 has the client send `Start` once and then refresh it "at
/// most every few seconds" while the user keeps typing, so this has to outlive
/// that interval with enough margin for a slow network — otherwise the indicator
/// blinks off mid-sentence. Ten seconds is that margin. It is also the longest a
/// client that vanished mid-word can keep showing as typing, which is the other
/// half of the trade: a longer TTL is smoother and lies for longer.
pub const TYPING_TTL_MS: u32 = 10_000;

/// Members a group may have, including its creator.
///
/// A group is fanned out to synchronously and every member's cursor is written
/// on every send, so the number is a latency budget rather than a product
/// preference. Anything larger belongs in a room, which has a home region, a
/// sequencer, and a member count column for exactly this reason.
pub const MAX_GROUP_MEMBERS: usize = 256;

/// Conversations returned when the caller does not say how many.
///
/// Brief section 157 fixes the server's *maximum* page at 200 and requires a
/// larger request to be clamped rather than refused. It says nothing about an
/// unspecified one, and answering an unspecified request with the maximum would
/// hand a mobile client on a metered connection four screenfuls it did not ask
/// for. Fifty is a screenful plus prefetch.
pub const DEFAULT_CONVERSATION_PAGE: u16 = 50;

/// Members named in each row of a conversation list.
///
/// Enough to render the stacked avatars a group row shows and no more. The full
/// membership of a conversation is its own request: putting it in the list would
/// make the response size a function of the largest group the caller is in.
pub const MEMBER_PREVIEW: u16 = 8;

/// Longest disappearing-message lifetime a client may ask for.
///
/// Thirty days. The ceiling exists because `expires_in_ms` is a `u32` of
/// milliseconds, which reaches 49 days, and a request for "expires in 49 days"
/// is not a feature — it is an off-by-a-thousand in a client that meant seconds.
/// Refusing it is how that bug gets found in development rather than in a year.
pub const MAX_EXPIRY_MS: u32 = 30 * 24 * 60 * 60 * 1_000;
