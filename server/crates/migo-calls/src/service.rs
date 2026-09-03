//! The call service.
//!
//! # The four invariants this file exists to hold
//!
//! **A call id is a promise.** The client mints it and retries against it,
//! so every method here treats a repeated id as the *same call*: a re-invite
//! returns the first invite's answer and rings nobody twice, a re-answer
//! from the same device is a no-op, and a decline of a call that is already
//! over is a success that changes nothing. An id reused for *different*
//! intent — another callee, another conversation — is refused with
//! `IDEMPOTENCY_MISMATCH`, the same code the messaging path uses for the same
//! client bug.
//!
//! **The state machine has no zombies.** Every path out of `Ringing` writes
//! an end: the sweep retires expired invites as `NoAnswer`, an answer that
//! arrives one millisecond late retires the ring and says so, and a decline
//! or a cancel ends the call from any live state rather than erroring and
//! leaving a ring nobody can stop.
//!
//! **The server reads headers, never payloads.** The relay methods validate
//! `call_id`, `from_device`, and `to_device` against the call row and return
//! the sealed bytes untouched. There is no code path in this crate that
//! inspects, logs, or re-seals an SDP blob or a candidate batch, and the
//! store has no column that could hold one.
//!
//! **The other side of every change hears about it.** Each mutating method
//! returns the event its change implies for the *other* participant — the
//! callee's ring, the caller's `Connecting`, the other party's `Ended` — and
//! returns `None` when nothing changed, because a frame for a change that
//! did not happen is a frame a client will mis-render.
//!
//! # Why the sweep runs inside `invite`
//!
//! An expired invite that nothing sweeps is a row that answers "ringing"
//! forever. This crate deliberately owns no timer, so every invite first
//! retires whatever expired — the traffic that creates dead rings is the
//! traffic that cleans them — and [`Callkeeper::sweep`] stays public for the
//! composition root, which runs it on a timer of its own. The two are not
//! rivals: the timer publishes [`Call::ended_event`] to both parties so a
//! callee nobody re-invites still hears the ring die, while this opportunistic
//! pass keeps a quiet node's rows honest between ticks.
//!
//! # What is deliberately not here
//!
//! *No TURN issuance.* The credentials come from configuration the operator
//! owns, and until there is a configured relay there is an honest empty list;
//! see `CallsConfig`.
//!
//! *No SFU.* Group calls are a separate deployment with their own join path;
//! the dispatcher answers those opcodes `FEATURE_DISABLED` and this crate
//! has nothing to say about them.
//!
//! *No push.* Whether a ring should wake a device that is offline is
//! `migo-notify`'s question, and the invite event the dispatcher publishes
//! is the realtime half of it.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::metrics::Registry;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{codes, fault, CallInviteEvent, CallStateEvent, Opcode};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};

use crate::metrics::{AnswerOutcome, InviteOutcome, Meters, RelayKind};
use crate::model::{
    Call, CallIceWire, CallInviteWire, CallSdpWire, CallState, Caller, CallsConfig, EndReason,
    InviteOutcome as WireOutcome, TurnServerWire, MAX_SEALED_LEN, MEDIA_VIDEO,
};
use crate::store::{CallStore, SharedCallStore};
use crate::traits::{CallGate, Callkeeper, SharedCallGate, SharedCallkeeper};

/// Builds the call service the composition root hands around.
///
/// The store, the limiter, and the gate arrive pre-constructed because each
/// is a deployment decision: which backend, which buckets, whose membership
/// and block tables answer the gate. [`CallsConfig`] arrives separately for
/// the same reason as every other domain's config — the ring timeout is
/// something an operator tunes, not something the code knows.
#[must_use]
pub fn open(
    store: SharedCallStore,
    limiter: SharedRateLimiter,
    gate: SharedCallGate,
    registry: &Registry,
    config: CallsConfig,
) -> SharedCallkeeper {
    Arc::new(Calls::new(store, limiter, gate, registry, config))
}

/// Calls over a store and a rate limiter.
///
/// Generic over both with `dyn` defaults, the same shape as the messaging
/// service: the composition root holds the erased form, and a test can hold
/// the monomorphised one over the in-memory store and the real limiter.
pub struct Calls<S: ?Sized = dyn CallStore, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    limiter: Arc<L>,
    gate: Arc<dyn CallGate>,
    config: CallsConfig,
    meters: Meters,
}

impl<S, L> Calls<S, L>
where
    S: CallStore + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Assembles the service and registers every series at zero.
    pub fn new(
        store: Arc<S>,
        limiter: Arc<L>,
        gate: Arc<dyn CallGate>,
        registry: &Registry,
        config: CallsConfig,
    ) -> Self {
        Self {
            store,
            limiter,
            gate,
            config,
            meters: Meters::new(registry),
        }
    }

    /// Charges the caller's own surfaces for one operation.
    ///
    /// Two buckets, tightest first, exactly as the messaging service charges
    /// them. The call's *other party* is deliberately not a third bucket: a
    /// callee-scoped limit would let one account's callers spend the
    /// callee's budget, and the account that places the call is the account
    /// whose allowance it costs.
    async fn charge(&self, caller: &Caller, opcode: Opcode) -> Result<()> {
        let keys = [
            BucketKey::endpoint_write_of_account(caller.account_id, opcode),
            BucketKey::account_write(caller.account_id),
        ];
        self.limiter
            .charge_opcode(&keys, opcode, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// The call, or `NOT_FOUND`.
    ///
    /// One answer for "no such call" and "not your call": the id is
    /// client-minted and unguessable, but an endpoint that distinguished the
    /// two would still be an oracle for which ids are live, and a ring is
    /// nobody's business but its two parties'.
    async fn load(&self, call_id: Id) -> Result<Call> {
        self.store
            .get(call_id)
            .await?
            .ok_or_else(|| fault::not_found("call"))
    }

    /// Ends a live call and returns its event.
    ///
    /// The one shape every terminal path shares: stamp the reason and the
    /// moment, persist, count, and hand back the frame the other party
    /// should see. `ended` is `None` only if the caller of this helper
    /// already checked liveness, which every caller does.
    async fn terminate(
        &self,
        call: &mut Call,
        reason: EndReason,
        now: Timestamp,
    ) -> Result<CallStateEvent> {
        call.state = CallState::Ended;
        call.end_reason = Some(reason);
        call.ended_at = Some(now);
        self.store.put(call).await?;
        self.meters.ended(reason);
        Ok(call.ended_event())
    }

    /// The routing and standing checks every relay shares.
    ///
    /// The call must exist and be past its answer (state first: a relay
    /// against a ring or a corpse is a state conflict, and telling the client
    /// *that* is more useful than a routing refusal that reads as a bug); the
    /// sending device must be a device of the caller's account in that call
    /// (which checks both that the account is a party and that the device it
    /// names is the one it is using); and the target must be the *other*
    /// device — a relay that targeted the sender's own device would be a
    /// loop, and one that targeted a device outside the call would be a
    /// stranger's inbox.
    async fn route(&self, caller: &Caller, frame: RelayFrame) -> Result<Call> {
        let call = self.load(frame.call_id).await?;
        match call.state {
            CallState::Connecting | CallState::Connected => {}
            CallState::Ringing => {
                return Err(fault::conflict(
                    "the call has not been answered; there is no media to negotiate",
                ));
            }
            CallState::Ended => {
                return Err(fault::conflict("the call has ended"));
            }
        }
        if call.account_of_device(frame.from_device) != Some(caller.account_id) {
            return Err(fault::permission_denied(
                "only a device in the call may relay through it",
            ));
        }
        if frame.to_device == frame.from_device || call.account_of_device(frame.to_device).is_none()
        {
            return Err(fault::permission_denied(
                "the relay target is not the other device in this call",
            ));
        }
        Ok(call)
    }

    /// The answer to a re-invite: whatever the first invite came to.
    ///
    /// No second ring. The callee already has the event from the attempt
    /// that landed, and a retry that re-rang would make one call sound like
    /// two — the one behaviour a callee cannot tolerate, because it is
    /// indistinguishable from harassment by a client that is merely buggy.
    fn invite_again(
        &self,
        caller: &Caller,
        invite: &CallInviteWire,
        existing: Call,
    ) -> Result<(WireOutcome, Option<CallInviteEvent>)> {
        if existing.caller_id != caller.account_id {
            self.meters.invite(InviteOutcome::Unknown);
            return Err(fault::not_found("call"));
        }
        if existing.conversation_id != invite.conversation_id
            || existing.callee_id != invite.callee_id
        {
            self.meters.invite(InviteOutcome::Conflict);
            return Err(fault::error(
                codes::IDEMPOTENCY_MISMATCH,
                "this call id was already used for a different invite",
            ));
        }
        let status = match existing.end_reason {
            Some(EndReason::Declined) => crate::model::invite_status::DECLINED,
            Some(EndReason::NoAnswer) => crate::model::invite_status::EXPIRED,
            // Cancelled, failed, or dropped: the id is spent, and the caller
            // is told so rather than handed a status the vocabulary has no
            // room for. A cancel-then-retry is a new call with a new id.
            Some(_) => {
                self.meters.invite(InviteOutcome::Conflict);
                return Err(fault::conflict("this call already ended"));
            }
            None => {
                if matches!(existing.state, CallState::Ringing)
                    && caller.now.is_at_or_after(existing.expires_at)
                {
                    // The sweep above retired every expired ring, so this is
                    // the same-millisecond race: expired, and the caller is
                    // told rather than left ringing against a dead call.
                    crate::model::invite_status::EXPIRED
                } else {
                    // Ringing, connecting, or connected — the invite was
                    // accepted, and the honest answer to "is it ringing?" is
                    // the one the first attempt got.
                    crate::model::invite_status::RINGING
                }
            }
        };
        self.meters.invite(InviteOutcome::Duplicate);
        Ok((
            WireOutcome {
                status,
                expires_at: existing.expires_at,
            },
            None,
        ))
    }
}

/// The routing headers a relay frame carries, flattened so the SDP and ICE
/// shapes share one set of checks.
///
/// No payload field, on purpose: the payload is the one part of a relay
/// frame this crate never reads, and a routing struct that cannot even name
/// it is the shape of that promise.
struct RelayFrame {
    call_id: Id,
    from_device: Id,
    to_device: Id,
}

/// The `Connecting` event for a call that just changed state.
fn state_event(call: &Call) -> CallStateEvent {
    CallStateEvent {
        call_id: call.call_id,
        state: call.state.to_wire(),
        reason: None,
    }
}

#[async_trait]
impl<S, L> Callkeeper for Calls<S, L>
where
    S: CallStore + ?Sized + Send + Sync,
    L: RateLimiter + ?Sized + Send + Sync,
{
    async fn invite(
        &self,
        caller: &Caller,
        invite: CallInviteWire,
    ) -> Result<(WireOutcome, Option<CallInviteEvent>)> {
        // Shape first, and before the rate limiter: a malformed frame is a
        // client bug, and charging for it would let one broken build exhaust
        // its user's allowance and take their working devices down with it.
        if invite.call_id.is_nil() {
            self.meters.invite(InviteOutcome::Invalid);
            return Err(fault::field_required("call_id"));
        }
        if invite.conversation_id.is_nil() {
            self.meters.invite(InviteOutcome::Invalid);
            return Err(fault::field_required("conversation_id"));
        }
        if invite.callee_id.is_nil() {
            self.meters.invite(InviteOutcome::Invalid);
            return Err(fault::field_required("callee_id"));
        }
        if invite.callee_id == caller.account_id {
            self.meters.invite(InviteOutcome::Invalid);
            return Err(fault::validation(
                "callee_id",
                "a call needs somebody other than its caller to answer it",
            ));
        }
        if invite.media_kind > MEDIA_VIDEO {
            self.meters.invite(InviteOutcome::Invalid);
            return Err(fault::validation("media_kind", "a call is audio or video"));
        }
        if invite.sealed_offer.is_empty() {
            self.meters.invite(InviteOutcome::Invalid);
            return Err(fault::field_required("sealed_offer"));
        }
        if invite.sealed_offer.len() > MAX_SEALED_LEN {
            self.meters.invite(InviteOutcome::Invalid);
            return Err(fault::field_too_long("sealed_offer", MAX_SEALED_LEN));
        }

        if let Err(error) = self.charge(caller, Opcode::CallInvite).await {
            self.meters.invite(InviteOutcome::RateLimited);
            return Err(error);
        }

        // The v1 sweeper. Retires this caller's own dead rings — and
        // everyone else's — before a new one is created; see the module docs
        // for why this runs here rather than in a task.
        let retired = self.store.sweep_expired(caller.now).await?;
        if !retired.is_empty() {
            self.meters.expired(retired.len());
        }

        if let Some(existing) = self.store.get(invite.call_id).await? {
            return self.invite_again(caller, &invite, existing);
        }

        // The gate, asked in the order the refusals compound: membership
        // first (the caller's own standing, which they can fix), the block
        // second (somebody else's decision, which they cannot), and the
        // callee's call policy third — the same visibility choice they
        // already make about messages, applied to the ring.
        if !self
            .gate
            .may_invite(invite.conversation_id, caller.account_id)
            .await
        {
            self.meters.invite(InviteOutcome::Unknown);
            return Err(fault::not_found("conversation"));
        }
        if self
            .gate
            .blocked_either_way(caller.account_id, invite.callee_id)
            .await
        {
            self.meters.invite(InviteOutcome::Blocked);
            // An outcome and not an error: the caller's own screen knows what
            // "blocked" renders as, and nothing is stored — a block that is
            // lifted tomorrow must not have left a call row behind that
            // answers a re-invite with a stale status today.
            return Ok((
                WireOutcome {
                    status: crate::model::invite_status::BLOCKED,
                    expires_at: caller.now,
                },
                None,
            ));
        }
        if !self.gate.can_call(caller, invite.callee_id).await {
            self.meters.invite(InviteOutcome::Blocked);
            // The same outcome, deliberately, and for the same reason the
            // block returns one: a callee whose policy excludes the caller
            // and a callee who blocked them are the same answer on the
            // caller's screen (brief section 180), and a policy widened
            // tomorrow must not find a stored row answering a re-invite with
            // today's refusal.
            return Ok((
                WireOutcome {
                    status: crate::model::invite_status::BLOCKED,
                    expires_at: caller.now,
                },
                None,
            ));
        }

        let call = Call {
            call_id: invite.call_id,
            conversation_id: invite.conversation_id,
            caller_id: caller.account_id,
            // The authenticated device, never the frame's own claim: the
            // server watched this frame arrive on a device it proved.
            caller_device: caller.device_id,
            callee_id: invite.callee_id,
            callee_device: None,
            media_kind: invite.media_kind,
            state: CallState::Ringing,
            end_reason: None,
            expires_at: caller.now.saturating_add_millis(self.config.ring_ttl_ms),
            answered_at: None,
            ended_at: None,
        };
        self.store.put(&call).await?;
        self.meters.invite(InviteOutcome::Ringing);

        let event = CallInviteEvent {
            call_id: call.call_id,
            conversation_id: call.conversation_id,
            caller_id: call.caller_id,
            caller_device: call.caller_device,
            media_kind: call.media_kind,
            expires_at: call.expires_at,
            // Moved, not copied, and never read: the sealed offer is the
            // callee's to open, and this method is a mail slot.
            sealed_offer: invite.sealed_offer,
        };
        Ok((
            WireOutcome {
                status: crate::model::invite_status::RINGING,
                expires_at: call.expires_at,
            },
            Some(event),
        ))
    }

    async fn answer(
        &self,
        caller: &Caller,
        call_id: Id,
        callee_device: Id,
    ) -> Result<Option<CallStateEvent>> {
        if call_id.is_nil() {
            self.meters.answer(AnswerOutcome::Invalid);
            return Err(fault::field_required("call_id"));
        }
        if callee_device.is_nil() {
            self.meters.answer(AnswerOutcome::Invalid);
            return Err(fault::field_required("callee_device"));
        }
        if let Err(error) = self.charge(caller, Opcode::CallAnswer).await {
            self.meters.answer(AnswerOutcome::RateLimited);
            return Err(error);
        }
        let mut call = self.load(call_id).await?;
        if call.callee_id != caller.account_id {
            // The caller of a call cannot answer it either; one answer for
            // both, so the endpoint is not a probe.
            self.meters.answer(AnswerOutcome::Unknown);
            return Err(fault::not_found("call"));
        }

        match call.state {
            CallState::Ringing => {
                if caller.now.is_at_or_after(call.expires_at) {
                    // The answer raced the deadline and lost. Retiring the
                    // ring here keeps the store free of zombies, and the
                    // event still names the party who needed to know.
                    self.meters.answer(AnswerOutcome::Expired);
                    self.meters.expired(1);
                    return Ok(Some(
                        self.terminate(&mut call, EndReason::NoAnswer, caller.now)
                            .await?,
                    ));
                }
                call.state = CallState::Connecting;
                call.callee_device = Some(callee_device);
                call.answered_at = Some(caller.now);
                self.store.put(&call).await?;
                self.meters.answer(AnswerOutcome::Answered);
                Ok(Some(state_event(&call)))
            }
            CallState::Connecting | CallState::Connected => {
                if call.callee_device == Some(callee_device) {
                    // The same answer twice: a retry, and a success that
                    // changes nothing. The caller of the call already has the
                    // `Connecting` event from the attempt that landed.
                    self.meters.answer(AnswerOutcome::Duplicate);
                    Ok(None)
                } else {
                    self.meters.answer(AnswerOutcome::Conflict);
                    Err(fault::conflict(
                        "another device of this account already answered",
                    ))
                }
            }
            CallState::Ended => {
                // The answer raced a decline or a cancel and lost. Nothing
                // changes, so nothing is sent (brief section 156).
                self.meters.answer(AnswerOutcome::Duplicate);
                Ok(None)
            }
        }
    }

    async fn decline(&self, caller: &Caller, call_id: Id) -> Result<Option<CallStateEvent>> {
        if call_id.is_nil() {
            return Err(fault::field_required("call_id"));
        }
        self.charge(caller, Opcode::CallDecline).await?;
        let mut call = self.load(call_id).await?;
        if call.callee_id != caller.account_id {
            return Err(fault::not_found("call"));
        }
        if call.state == CallState::Ended {
            // A decline of a call that is already over is a retry, not an
            // error — the outcome the declining client wanted is the one the
            // call already has.
            return Ok(None);
        }
        // From any live state, including one where the callee's own answer
        // had just landed: a client that declines after answering still wants
        // out, and refusing here would leave a call ringing that nobody can
        // hear.
        Ok(Some(
            self.terminate(&mut call, EndReason::Declined, caller.now)
                .await?,
        ))
    }

    async fn cancel(&self, caller: &Caller, call_id: Id) -> Result<Option<CallStateEvent>> {
        if call_id.is_nil() {
            return Err(fault::field_required("call_id"));
        }
        self.charge(caller, Opcode::CallCancel).await?;
        let mut call = self.load(call_id).await?;
        if call.caller_id != caller.account_id {
            // Only the caller can un-ring their own call; the callee's way
            // out is decline or end.
            return Err(fault::not_found("call"));
        }
        if call.state == CallState::Ended {
            return Ok(None);
        }
        Ok(Some(
            self.terminate(&mut call, EndReason::ByCaller, caller.now)
                .await?,
        ))
    }

    async fn end(
        &self,
        caller: &Caller,
        call_id: Id,
        reason: u32,
    ) -> Result<Option<CallStateEvent>> {
        if call_id.is_nil() {
            return Err(fault::field_required("call_id"));
        }
        let Some(reason) = EndReason::from_wire(reason) else {
            return Err(fault::validation(
                "reason",
                "not an end reason this build knows",
            ));
        };
        self.charge(caller, Opcode::CallEnd).await?;
        let mut call = self.load(call_id).await?;
        if call.other_party(caller.account_id).is_none() {
            // A stranger to the call ends nothing and learns nothing.
            return Err(fault::not_found("call"));
        }
        if call.state == CallState::Ended {
            // Over is over, whatever reason arrives last: the first reason
            // is the truth the other party already heard.
            return Ok(None);
        }
        Ok(Some(self.terminate(&mut call, reason, caller.now).await?))
    }

    async fn relay_sdp(&self, caller: &Caller, sdp: CallSdpWire) -> Result<CallSdpWire> {
        if sdp.call_id.is_nil() {
            return Err(fault::field_required("call_id"));
        }
        if sdp.sealed_sdp.is_empty() {
            return Err(fault::field_required("sealed_sdp"));
        }
        if sdp.sealed_sdp.len() > MAX_SEALED_LEN {
            return Err(fault::field_too_long("sealed_sdp", MAX_SEALED_LEN));
        }
        self.charge(caller, Opcode::CallSdp).await?;
        let mut call = self
            .route(
                caller,
                RelayFrame {
                    call_id: sdp.call_id,
                    from_device: sdp.from_device,
                    to_device: sdp.to_device,
                },
            )
            .await?;
        // The callee's sealed answer, relayed toward the caller, is the
        // moment both devices hold what they need — the call is connected.
        // Later relays (renegotiations, ICE restarts) pass through without
        // touching the state again.
        if call.state == CallState::Connecting && Some(sdp.from_device) == call.callee_device {
            call.state = CallState::Connected;
            self.store.put(&call).await?;
            self.meters.connected();
        }
        self.meters.relayed(RelayKind::Sdp);
        // Untouched. Not logged, not measured, not copied: the payload is
        // returned as it arrived and the routing decision above is the only
        // thing this method did with the frame.
        Ok(sdp)
    }

    async fn relay_ice(&self, caller: &Caller, ice: CallIceWire) -> Result<CallIceWire> {
        if ice.call_id.is_nil() {
            return Err(fault::field_required("call_id"));
        }
        if ice.sealed_candidates.is_empty() {
            return Err(fault::field_required("sealed_candidates"));
        }
        if ice.sealed_candidates.len() > MAX_SEALED_LEN {
            return Err(fault::field_too_long("sealed_candidates", MAX_SEALED_LEN));
        }
        self.charge(caller, Opcode::CallIce).await?;
        self.route(
            caller,
            RelayFrame {
                call_id: ice.call_id,
                from_device: ice.from_device,
                to_device: ice.to_device,
            },
        )
        .await?;
        // Candidates never mark anything connected — not here, where there is
        // no state to mark, and not in the relay's caller: connectivity is an
        // answer fact, not a candidate-arrival fact.
        self.meters.relayed(RelayKind::Ice);
        Ok(ice)
    }

    async fn mark_connected(&self, call_id: Id, at: Timestamp) -> Result<()> {
        if call_id.is_nil() {
            return Err(fault::field_required("call_id"));
        }
        let mut call = self.load(call_id).await?;
        match call.state {
            CallState::Connected => Ok(()),
            CallState::Connecting => {
                call.state = CallState::Connected;
                self.store.put(&call).await?;
                self.meters.connected();
                // The timestamp is this mark's own provenance; there is no
                // column for it yet, so it is recorded where every other
                // cross-layer fact is: the trace.
                tracing::trace!(call_id = %call.call_id, at = at.as_unix_ms(), "call connected");
                Ok(())
            }
            // A ring cannot be connected — no answer has arrived — and a
            // refusal here is the honest answer to a client that skipped the
            // answer path entirely.
            CallState::Ringing => Err(fault::conflict(
                "the call has not been answered; it cannot be connected",
            )),
            // The mark raced the end. The end is the later truth and the
            // caller of this method has nothing to undo, so it is a success
            // that changed nothing rather than an error a retry loop would
            // hammer.
            CallState::Ended => Ok(()),
        }
    }

    async fn turn_servers(&self, call_id: Id) -> Result<Vec<TurnServerWire>> {
        if call_id.is_nil() {
            return Err(fault::field_required("call_id"));
        }
        // From configuration, never minted here: TURN credentials are signed
        // material an operator owns, and the empty list until they exist is
        // the answer a client can act on (relay off, direct connection only)
        // rather than a promise pointing at nothing.
        Ok(self.config.turn_servers.clone())
    }

    async fn sweep(&self, now: Timestamp) -> Result<Vec<Call>> {
        let retired = self.store.sweep_expired(now).await?;
        if !retired.is_empty() {
            self.meters.expired(retired.len());
        }
        Ok(retired)
    }

    async fn call(&self, caller: &Caller, call_id: Id) -> Result<Call> {
        let call = self.load(call_id).await?;
        if call.other_party(caller.account_id).is_none() {
            return Err(fault::not_found("call"));
        }
        Ok(call)
    }
}
