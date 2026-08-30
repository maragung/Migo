//! What the calls service offers the layer above, and what it asks of the
//! layers beside it.
//!
//! # Why the events are return values and not publications
//!
//! No method here sends a frame. Each one returns the event it produced, and
//! the dispatcher — the one place a connection context exists — decides where
//! it goes. That is the same seam every other domain uses, and for calls it
//! carries extra weight: the service knows *what changed* ("this call is
//! connecting"), but only the dispatcher can know *who is connected right
//! now* and which topic reaches them.
//!
//! # The gate
//!
//! Brief section 180 makes a refused call indistinguishable from a missing
//! one, and the facts that decide a refusal — membership, a block — belong to
//! other domains. [`CallGate`] is the two questions this crate needs answered,
//! asked as questions, so that the answers arrive pre-checked: the composition
//! root wires them to the store and the social graph, and a test wires them to
//! a closure. This crate deliberately cannot read either table itself, which
//! is what keeps the layering rule (no layer-3 crate depends on another)
//! intact around the one domain whose every request is about *somebody else*.
//!
//! # What [`Callkeeper::call`] is for
//!
//! The dispatcher needs a call row to route with — which of the two accounts
//! should hear about this state change, which account owns the device a relay
//! targets. It is a read on the path of an operation that has already been
//! charged, so like `migo-rooms`' `authorize` it deliberately costs nothing:
//! billing a routing lookup would make a call's price depend on how many
//! publishes the dispatcher does for it.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};

use crate::model::{Call, CallIceWire, CallInviteWire, CallSdpWire, Caller, TurnServerWire};

/// The questions the call service must ask another domain, answered by the
/// composition root.
///
/// Both methods answer with a `bool` rather than a `Result` on purpose: an
/// authority that cannot answer is an authority whose failure is a policy
/// decision, and the *caller of the gate* is the one who has to live with
/// that decision. Fail closed — "not a member", "blocked" — is the only
/// posture either question supports.
#[async_trait]
pub trait CallGate: Send + Sync {
    /// Whether `caller_id` may place a call inside `conversation_id`.
    ///
    /// Membership, pre-checked by whoever owns conversations. The service
    /// treats `false` as indistinguishable from a conversation that does not
    /// exist.
    async fn may_invite(&self, conversation_id: Id, caller_id: Id) -> bool;

    /// Whether either account has blocked the other.
    ///
    /// The block is symmetric and either direction stops the call — the same
    /// rule `migo-social` enforces for messages, applied to the ring.
    async fn blocked_either_way(&self, a: Id, b: Id) -> bool;
}

/// A shared, fully erased gate.
pub type SharedCallGate = Arc<dyn CallGate>;

/// The gate that says yes to everything.
///
/// The development default, and the one a simulation wants: every
/// conversation is callable, nobody is blocked. It exists so a test (or a
/// laptop) can run the whole ring state machine without standing up the
/// membership and graph tables behind it — never so a production node can
/// skip the checks, which is why nothing in this crate constructs it for
/// you.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenGate;

#[async_trait]
impl CallGate for OpenGate {
    async fn may_invite(&self, _conversation_id: Id, _caller_id: Id) -> bool {
        true
    }

    async fn blocked_either_way(&self, _a: Id, _b: Id) -> bool {
        false
    }
}

/// The call signalling service, erased.
///
/// Every method takes the [`Caller`] the gateway proved. Nothing here trusts
/// an account or device from a frame; the one frame field that names a device
/// the server must believe is `to_device`, and the service checks that
/// against the call row rather than the connection.
#[async_trait]
pub trait Callkeeper: Send + Sync {
    /// Caller invites callee.
    ///
    /// Validates shape, charges the caller, retires expired invites, and —
    /// when the gate allows it — records the call in
    /// [`CallState::Ringing`](crate::model::CallState::Ringing) and returns
    /// the invite event for the callee's user topic alongside the outcome for
    /// the caller. A retried `call_id` gets the same answer it got the first
    /// time and no second ring: the callee already has the event, and a
    /// second one is a second notification for one call.
    ///
    /// The sealed offer is carried into the event untouched. Nothing between
    /// the request and the callee's topic reads it, copies it into a log, or
    /// measures its length beyond the codec's own bound.
    async fn invite(
        &self,
        caller: &Caller,
        invite: CallInviteWire,
    ) -> Result<(
        crate::model::InviteOutcome,
        Option<migo_protocol::CallInviteEvent>,
    )>;

    /// Callee answers. `Ringing` → `Connecting`, and the callee's device is
    /// recorded.
    ///
    /// The device is the connection's own, handed in by the dispatcher — a
    /// frame-supplied device id would let one device answer for another, and
    /// the sealed answer then relays to a device that never picked up.
    /// Returns the `Connecting` state event for the *caller's* topic. A
    /// repeat answer from the same device changes nothing and returns no
    /// event; a second device of the same account that loses the race is told
    /// so with `CONFLICT`.
    async fn answer(
        &self,
        caller: &Caller,
        call_id: Id,
        callee_device: Id,
    ) -> Result<Option<migo_protocol::CallStateEvent>>;

    /// Callee declines. Any live call ends `Declined`.
    ///
    /// Returns the ended state event for the caller's topic. Declining a
    /// call that is already over is a retry, not an error: nothing changes,
    /// nothing is sent.
    async fn decline(
        &self,
        caller: &Caller,
        call_id: Id,
    ) -> Result<Option<migo_protocol::CallStateEvent>>;

    /// Caller cancels before an answer. Any live call ends `ByCaller`.
    ///
    /// Returns the ended state event for the callee's topic — the device that
    /// is ringing needs to stop. Cancelling an ended call is a retry and
    /// returns no event.
    async fn cancel(
        &self,
        caller: &Caller,
        call_id: Id,
    ) -> Result<Option<migo_protocol::CallStateEvent>>;

    /// Either party ends an established call, with the wire's own reason.
    ///
    /// The reason is the sender's claim and is recorded as claimed; see
    /// [`EndReason`](crate::model::EndReason) for why that is the whole of
    /// the server's role. Returns the ended state event for the *other*
    /// party's topic.
    async fn end(
        &self,
        caller: &Caller,
        call_id: Id,
        reason: u32,
    ) -> Result<Option<migo_protocol::CallStateEvent>>;

    /// Relays sealed SDP from one device in the call to the other.
    ///
    /// The server reads only the routing headers — call, from-device,
    /// to-device — and checks them against the call row: the sender must be a
    /// device of a party to the call, and the target must be the other
    /// device. The payload is returned exactly as it arrived; this method
    /// exists so the routing decision is made once, here, against state,
    /// rather than in every dispatcher that could publish a frame.
    ///
    /// Relaying the callee's first answer while the call is `Connecting`
    /// marks the call `Connected`: that relay is the moment both sides hold
    /// what they need for media.
    async fn relay_sdp(&self, caller: &Caller, sdp: CallSdpWire) -> Result<CallSdpWire>;

    /// Relays a batch of sealed ICE candidates, same rules as
    /// [`Callkeeper::relay_sdp`].
    ///
    /// Never marks a call connected: candidates flow while the call is still
    /// `Connecting`, and connectivity is an SDP-and-answer fact, not a
    /// candidate-arrival fact.
    async fn relay_ice(&self, caller: &Caller, ice: CallIceWire) -> Result<CallIceWire>;

    /// Marks a call `Connected`.
    ///
    /// Separately from [`Callkeeper::relay_sdp`] because the SFU build will
    /// reach this state from its own join path rather than from a relay.
    /// Idempotent, and a no-op on a call that has already ended — the mark
    /// raced the end, and the end is the more recent truth.
    async fn mark_connected(&self, call_id: Id, at: Timestamp) -> Result<()>;

    /// TURN relays for a call, from configuration.
    ///
    /// Empty until credentials are configured. `call_id` is validated for
    /// shape and otherwise unused — TURN credentials are per-deployment, not
    /// per-call, and a caller who can name a call id they are not in learns
    /// nothing from an empty list.
    async fn turn_servers(&self, call_id: Id) -> Result<Vec<TurnServerWire>>;

    /// Ends every invite whose deadline has passed, returning a `NoAnswer`
    /// state event for each.
    ///
    /// Whose topic those events belong to is the publisher's problem: the
    /// wire event names the call, and the dispatcher (or the background task,
    /// when one exists) maps call to caller. In this build the sweep also
    /// runs inside [`Callkeeper::invite`], so a node without a background
    /// task still retires its dead rings — the caller's client times the ring
    /// itself from `expires_at`, so it needs no event to stop ringing.
    async fn sweep(&self, now: Timestamp) -> Result<Vec<migo_protocol::CallStateEvent>>;

    /// One call, for a participant.
    ///
    /// The routing read: the dispatcher asks this to learn which account
    /// should hear about a state change. A stranger gets `NOT_FOUND` — the
    /// same answer as a call id that was never minted, so the endpoint is not
    /// a probe for which calls exist. Not rate limited, because it rides
    /// paths that already paid.
    async fn call(&self, caller: &Caller, call_id: Id) -> Result<Call>;
}

/// The call service, shared.
pub type SharedCallkeeper = Arc<dyn Callkeeper>;
