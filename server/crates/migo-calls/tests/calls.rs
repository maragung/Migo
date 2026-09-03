//! The call state machine, tested where getting it wrong is invisible.
//!
//! A call is the one flow where the server's mistake is heard by two people
//! at once: the caller's phone says one thing, the callee's says another,
//! and the disagreement is the bug report. The tests here pin the properties
//! that keep the two sides agreeing:
//!
//! **A retried id is the first attempt's answer.** The callee already has
//! the ring; a retry that rang again would be a second call wearing the
//! first one's id, which is indistinguishable from harassment by a client
//! that is merely buggy.
//!
//! **Every path writes an end.** A ring that times out, an answer that
//! arrives late, a decline of an already-ended call — none of them may leave
//! a row in a state no further frame can move.
//!
//! **The sealed bytes are mail, not cargo.** The relay tests assert on the
//! whole frame, because the failure mode is not "the bytes got mangled" (the
//! codec would notice) but "the server started reading them".
//!
//! The rate limiter is the real one over a real cache, so the arithmetic is
//! part of the test: an invite costs twenty against an account's burst,
//! which no test here approaches — each builds a fresh harness so a budget
//! spent in one test cannot fail another.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::MemoryCache;
use migo_calls::model::{
    invite_status, CallIceWire, CallInviteWire, CallSdpWire, CallState, Caller, CallsConfig,
    EndReason, RING_TTL_MS,
};
use migo_calls::store::{CallStore, MemoryCallStore};
use migo_calls::traits::{CallGate, Callkeeper};
use migo_calls::Calls;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Timestamp};
use migo_protocol::{codes, TurnServer};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};

const SECOND: i64 = 1_000;
const NOW: i64 = 1_700_000_000 * SECOND;

const ALICE: u128 = 1;
const BOB: u128 = 2;
const CAROL: u128 = 3;

const ALICE_PHONE: u128 = 101;
const BOB_PHONE: u128 = 102;
const BOB_LAPTOP: u128 = 103;
const CAROL_PHONE: u128 = 104;

const CONVERSATION: u128 = 50;
const CALL: u128 = 60;

type TestCalls = Calls<MemoryCallStore, CacheRateLimiter<MemoryCache>>;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

fn caller(account: u128, device: u128, now: i64) -> Caller {
    Caller::new(id(account), id(device), TrustTier::Established, ts(now))
}

fn alice(now: i64) -> Caller {
    caller(ALICE, ALICE_PHONE, now)
}

fn bob(now: i64) -> Caller {
    caller(BOB, BOB_PHONE, now)
}

fn invite(call_id: u128, callee: u128) -> CallInviteWire {
    CallInviteWire {
        call_id: id(call_id),
        conversation_id: id(CONVERSATION),
        callee_id: id(callee),
        media_kind: 0,
        caller_device: id(ALICE_PHONE),
        capabilities: 0,
        sealed_offer: b"sealed-offer".to_vec(),
    }
}

/// The gate as a test needs it: a membership list, a block list, and a list
/// of callees the social graph refuses, all answerable without a store.
struct TestGate {
    members: HashMap<Id, Vec<Id>>,
    blocked: Vec<(Id, Id)>,
    unreachable: Vec<Id>,
}

impl TestGate {
    /// Alice and Bob may call inside the conversation.
    fn open() -> Self {
        Self {
            members: HashMap::from([(id(CONVERSATION), vec![id(ALICE), id(BOB), id(CAROL)])]),
            blocked: Vec::new(),
            unreachable: Vec::new(),
        }
    }

    /// The same, with a block between Alice and Bob.
    fn with_block() -> Self {
        Self {
            blocked: vec![(id(ALICE), id(BOB))],
            ..Self::open()
        }
    }

    /// The same, with the graph refusing Bob as a callee: Bob's call policy
    /// excludes Alice, exactly as `may_interact(Interaction::Call)` would.
    fn refused() -> Self {
        Self {
            unreachable: vec![id(BOB)],
            ..Self::open()
        }
    }

    /// Nobody may call: the conversation is closed to the caller.
    fn closed() -> Self {
        Self {
            members: HashMap::new(),
            blocked: Vec::new(),
            unreachable: Vec::new(),
        }
    }
}

#[async_trait]
impl CallGate for TestGate {
    async fn may_invite(&self, conversation_id: Id, caller_id: Id) -> bool {
        self.members
            .get(&conversation_id)
            .is_some_and(|members| members.contains(&caller_id))
    }

    async fn blocked_either_way(&self, a: Id, b: Id) -> bool {
        self.blocked.contains(&(a, b)) || self.blocked.contains(&(b, a))
    }

    async fn can_call(&self, _caller: &migo_calls::Caller, callee_id: Id) -> bool {
        !self.unreachable.contains(&callee_id)
    }
}

/// Everything a test needs, with the real limiter over a real cache.
struct Harness {
    calls: TestCalls,
    store: Arc<MemoryCallStore>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::gated(TestGate::open())
    }

    fn blocked() -> Self {
        Self::gated(TestGate::with_block())
    }

    fn refused() -> Self {
        Self::gated(TestGate::refused())
    }

    fn closed() -> Self {
        Self::gated(TestGate::closed())
    }

    fn gated(gate: TestGate) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryCallStore::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        let calls = Calls::new(
            Arc::clone(&store),
            limiter,
            Arc::new(gate),
            &registry,
            CallsConfig::default(),
        );
        Self {
            calls,
            store,
            registry,
        }
    }
}

#[tokio::test]
async fn an_invite_rings_and_the_callee_gets_the_event() {
    let harness = Harness::new();
    let (outcome, event) = harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let event = event.expect("the callee is told");

    assert_eq!(outcome.status, invite_status::RINGING);
    // The deadline is the server's, from its own clock: the two clients
    // agree on when the ring gives up without either trusting the other.
    assert_eq!(outcome.expires_at, ts(NOW + RING_TTL_MS));
    assert_eq!(event.call_id, id(CALL));
    assert_eq!(event.conversation_id, id(CONVERSATION));
    assert_eq!(event.caller_id, id(ALICE));
    // The authenticated device, not the frame's own claim.
    assert_eq!(event.caller_device, id(ALICE_PHONE));
    assert_eq!(event.expires_at, outcome.expires_at);
    assert_eq!(event.sealed_offer, b"sealed-offer".to_vec());

    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Ringing);
    assert_eq!(call.callee_id, id(BOB));
    assert_eq!(call.callee_device, None);
    assert!(call.end_reason.is_none());
}

#[tokio::test]
async fn a_retried_invite_gets_the_same_answer_and_rings_once() {
    let harness = Harness::new();
    let first = harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let first_event = first.1.expect("the first invite rings");
    assert_eq!(first.0.status, invite_status::RINGING);

    // The retry: same id, same intent, milliseconds later. Same answer, and
    // no second event — the callee already has the ring.
    let again = harness
        .calls
        .invite(&alice(NOW + SECOND), invite(CALL, BOB))
        .await
        .unwrap();
    assert_eq!(again.0.status, invite_status::RINGING);
    assert_eq!(again.0.expires_at, first.0.expires_at);
    assert!(again.1.is_none(), "a retry must not ring twice");

    // The same id aimed at somebody else is not a retry.
    let other = harness
        .calls
        .invite(&alice(NOW + SECOND), invite(CALL, CAROL))
        .await
        .unwrap_err();
    assert_eq!(other.code(), codes::IDEMPOTENCY_MISMATCH);
    assert!(harness.store.get(id(CALL)).await.unwrap().is_some());

    // And a stranger's re-invite of Alice's id learns nothing.
    let stranger = Harness::stranger_reinvite(&harness).await;
    assert_eq!(stranger.code(), codes::NOT_FOUND);

    let _ = first_event;
}

impl Harness {
    /// A different account retrying a call id that is not theirs.
    async fn stranger_reinvite(&self) -> migo_core::Error {
        self.calls
            .invite(&caller(CAROL, CAROL_PHONE, NOW + SECOND), invite(CALL, BOB))
            .await
            .unwrap_err()
    }
}

#[tokio::test]
async fn a_blocked_invite_never_rings_and_stores_nothing() {
    let harness = Harness::blocked();
    let (outcome, event) = harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    assert_eq!(outcome.status, invite_status::BLOCKED);
    // No deadline to wait for: nothing is ringing.
    assert_eq!(outcome.expires_at, ts(NOW));
    assert!(event.is_none());
    // Nothing stored. A block lifted tomorrow must not leave a call row that
    // answers a re-invite with a stale status today.
    assert!(harness.store.get(id(CALL)).await.unwrap().is_none());
}

#[tokio::test]
async fn a_graph_refusal_never_rings_and_stores_nothing() {
    let harness = Harness::refused();
    let (outcome, event) = harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    // The callee's policy excludes the caller, and the answer is the same
    // one a block produces: a refused call is indistinguishable from a
    // missing one (brief section 180), on the caller's screen and in the
    // store alike.
    assert_eq!(outcome.status, invite_status::BLOCKED);
    assert_eq!(outcome.expires_at, ts(NOW));
    assert!(event.is_none());
    assert!(harness.store.get(id(CALL)).await.unwrap().is_none());
}

#[tokio::test]
async fn a_non_member_cannot_invite() {
    let harness = Harness::closed();
    let error = harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap_err();
    // The same answer as a conversation that does not exist: the endpoint is
    // not a probe for which conversations are real.
    assert_eq!(error.code(), codes::NOT_FOUND);
    assert!(harness.store.get(id(CALL)).await.unwrap().is_none());
}

#[tokio::test]
async fn an_answer_connects_and_the_caller_is_told() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let event = harness
        .calls
        .answer(&bob(NOW + SECOND), id(CALL), id(BOB_PHONE))
        .await
        .unwrap()
        .expect("the caller is told the call is connecting");
    assert_eq!(event.call_id, id(CALL));
    assert_eq!(event.state, CallState::Connecting.to_wire());
    assert!(event.reason.is_none());

    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Connecting);
    assert_eq!(call.callee_device, Some(id(BOB_PHONE)));
    assert_eq!(call.answered_at, Some(ts(NOW + SECOND)));

    // The same answer from the same device again: a retry, and nothing moves.
    let again = harness
        .calls
        .answer(&bob(NOW + 2 * SECOND), id(CALL), id(BOB_PHONE))
        .await
        .unwrap();
    assert!(again.is_none());
}

#[tokio::test]
async fn the_second_device_to_answer_loses() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .answer(&bob(NOW + SECOND), id(CALL), id(BOB_PHONE))
        .await
        .unwrap();

    let laptop = Caller::new(
        id(BOB),
        id(BOB_LAPTOP),
        TrustTier::Established,
        ts(NOW + SECOND),
    );
    let error = harness
        .calls
        .answer(&laptop, id(CALL), id(BOB_LAPTOP))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::CONFLICT);

    // The call is still connecting on the first device's answer.
    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.callee_device, Some(id(BOB_PHONE)));
}

#[tokio::test]
async fn only_the_callee_can_answer() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    // The caller of the call cannot answer their own ring.
    let error = harness
        .calls
        .answer(&alice(NOW), id(CALL), id(ALICE_PHONE))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
    // Nor can a stranger.
    let error = harness
        .calls
        .answer(&caller(CAROL, CAROL_PHONE, NOW), id(CALL), id(CAROL_PHONE))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
}

#[tokio::test]
async fn an_answer_that_raced_the_deadline_retires_the_ring() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    // One millisecond past the deadline: the answer cannot connect a ring
    // that is already over, and the caller is told the call is a no-answer.
    let event = harness
        .calls
        .answer(&bob(NOW + RING_TTL_MS + 1), id(CALL), id(BOB_PHONE))
        .await
        .unwrap()
        .expect("the caller is told the ring died");
    assert_eq!(event.state, CallState::Ended.to_wire());
    assert_eq!(event.reason, Some(EndReason::NoAnswer.to_wire()));

    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Ended);
    assert_eq!(call.end_reason, Some(EndReason::NoAnswer));
}

#[tokio::test]
async fn a_decline_ends_the_call_declined() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let event = harness
        .calls
        .decline(&bob(NOW + SECOND), id(CALL))
        .await
        .unwrap()
        .expect("the caller is told");
    assert_eq!(event.state, CallState::Ended.to_wire());
    assert_eq!(event.reason, Some(EndReason::Declined.to_wire()));

    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Ended);
    assert_eq!(call.end_reason, Some(EndReason::Declined));
    assert_eq!(call.ended_at, Some(ts(NOW + SECOND)));

    // Declining again is a retry of a decision that already stands.
    let again = harness
        .calls
        .decline(&bob(NOW + 2 * SECOND), id(CALL))
        .await
        .unwrap();
    assert!(again.is_none());

    // A retried invite against the declined id reports what happened.
    let (outcome, event) = harness
        .calls
        .invite(&alice(NOW + 3 * SECOND), invite(CALL, BOB))
        .await
        .unwrap();
    assert_eq!(outcome.status, invite_status::DECLINED);
    assert!(event.is_none());
}

#[tokio::test]
async fn a_cancel_stops_the_ring() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let event = harness
        .calls
        .cancel(&alice(NOW + SECOND), id(CALL))
        .await
        .unwrap()
        .expect("the callee is told to stop ringing");
    assert_eq!(event.state, CallState::Ended.to_wire());
    assert_eq!(event.reason, Some(EndReason::ByCaller.to_wire()));

    // The callee cannot cancel; their way out is decline.
    harness
        .calls
        .invite(&alice(NOW), invite(CALL + 1, BOB))
        .await
        .unwrap();
    let error = harness
        .calls
        .cancel(&bob(NOW + SECOND), id(CALL + 1))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
}

#[tokio::test]
async fn an_end_carries_each_reason() {
    const REASONS: [(u32, EndReason); 6] = [
        (0, EndReason::ByCaller),
        (1, EndReason::ByCallee),
        (2, EndReason::Declined),
        (3, EndReason::NoAnswer),
        (4, EndReason::Failed),
        (5, EndReason::Network),
    ];
    for (wire, reason) in REASONS {
        let harness = Harness::new();
        harness
            .calls
            .invite(&alice(NOW), invite(CALL, BOB))
            .await
            .unwrap();
        harness
            .calls
            .answer(&bob(NOW + SECOND), id(CALL), id(BOB_PHONE))
            .await
            .unwrap();

        // Either party may end it; the reason is the sender's claim.
        let event = harness
            .calls
            .end(&bob(NOW + 2 * SECOND), id(CALL), wire)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("an end with reason {wire} produces an event"));
        assert_eq!(event.state, CallState::Ended.to_wire(), "reason {wire}");
        assert_eq!(event.reason, Some(wire), "reason {wire}");

        let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
        assert_eq!(call.state, CallState::Ended, "reason {wire}");
        assert_eq!(call.end_reason, Some(reason), "reason {wire}");
        assert_eq!(call.ended_at, Some(ts(NOW + 2 * SECOND)), "reason {wire}");

        // A second end, same reason or any other, changes nothing.
        let again = harness
            .calls
            .end(&alice(NOW + 3 * SECOND), id(CALL), wire)
            .await
            .unwrap();
        assert!(again.is_none(), "reason {wire}");
    }

    // A reason this build does not know is the client's fault.
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let error = harness
        .calls
        .end(&alice(NOW), id(CALL), 6)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::VALIDATION_FAILED);
}

#[tokio::test]
async fn a_stranger_cannot_end_a_call() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let error = harness
        .calls
        .end(&caller(CAROL, CAROL_PHONE, NOW), id(CALL), 0)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
}

#[tokio::test]
async fn a_relayed_sdp_passes_through_unchanged() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .answer(&bob(NOW + SECOND), id(CALL), id(BOB_PHONE))
        .await
        .unwrap();

    let frame = CallSdpWire {
        call_id: id(CALL),
        from_device: id(BOB_PHONE),
        to_device: id(ALICE_PHONE),
        sealed_sdp: b"the-sealed-answer".to_vec(),
    };
    let relayed = harness
        .calls
        .relay_sdp(&bob(NOW + 2 * SECOND), frame)
        .await
        .unwrap();
    // The whole frame, byte for byte: the server is a mail slot, and this
    // test is the assertion that it stayed one.
    assert_eq!(
        relayed,
        CallSdpWire {
            call_id: id(CALL),
            from_device: id(BOB_PHONE),
            to_device: id(ALICE_PHONE),
            sealed_sdp: b"the-sealed-answer".to_vec(),
        }
    );

    // The callee's first answer is what connects the call.
    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Connected);

    // Once connected, the caller's renegotiation offers relay too, and
    // connect nothing.
    let offer = CallSdpWire {
        call_id: id(CALL),
        from_device: id(ALICE_PHONE),
        to_device: id(BOB_PHONE),
        sealed_sdp: b"the-sealed-re-offer".to_vec(),
    };
    let relayed = harness
        .calls
        .relay_sdp(&alice(NOW + 3 * SECOND), offer.clone())
        .await
        .unwrap();
    assert_eq!(relayed, offer);
    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Connected);
}

#[tokio::test]
async fn a_relayed_ice_batch_passes_through_and_connects_nothing() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .answer(&bob(NOW + SECOND), id(CALL), id(BOB_PHONE))
        .await
        .unwrap();

    let frame = CallIceWire {
        call_id: id(CALL),
        from_device: id(BOB_PHONE),
        to_device: id(ALICE_PHONE),
        sealed_candidates: b"a-batch-of-sealed-candidates".to_vec(),
    };
    let relayed = harness
        .calls
        .relay_ice(&bob(NOW + 2 * SECOND), frame.clone())
        .await
        .unwrap();
    assert_eq!(relayed, frame);
    // Candidates arrive while the call is still connecting, and connect
    // nothing: an answer is an SDP fact, not a candidate fact.
    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Connecting);
}

#[tokio::test]
async fn a_relay_only_moves_between_the_call_s_own_devices() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();

    // Before an answer there is no negotiation to relay.
    let early = CallSdpWire {
        call_id: id(CALL),
        from_device: id(ALICE_PHONE),
        to_device: id(BOB_PHONE),
        sealed_sdp: b"sealed".to_vec(),
    };
    let error = harness
        .calls
        .relay_sdp(&alice(NOW + SECOND), early)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::CONFLICT);

    harness
        .calls
        .answer(&bob(NOW + SECOND), id(CALL), id(BOB_PHONE))
        .await
        .unwrap();

    // A device that is not in the call cannot use it as a route.
    let stranger_frame = CallSdpWire {
        call_id: id(CALL),
        from_device: id(CAROL_PHONE),
        to_device: id(ALICE_PHONE),
        sealed_sdp: b"sealed".to_vec(),
    };
    let error = harness
        .calls
        .relay_sdp(&caller(CAROL, CAROL_PHONE, NOW), stranger_frame)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::PERMISSION_DENIED);

    // Nor can a party relay to a device outside the call.
    let off_call = CallIceWire {
        call_id: id(CALL),
        from_device: id(ALICE_PHONE),
        to_device: id(CAROL_PHONE),
        sealed_candidates: b"sealed".to_vec(),
    };
    let error = harness
        .calls
        .relay_ice(&alice(NOW + SECOND), off_call)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::PERMISSION_DENIED);

    // Nor to themselves.
    let looped = CallIceWire {
        call_id: id(CALL),
        from_device: id(ALICE_PHONE),
        to_device: id(ALICE_PHONE),
        sealed_candidates: b"sealed".to_vec(),
    };
    let error = harness
        .calls
        .relay_ice(&alice(NOW + SECOND), looped)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::PERMISSION_DENIED);

    // After an end, the relay is closed.
    harness
        .calls
        .end(
            &alice(NOW + 2 * SECOND),
            id(CALL),
            EndReason::Failed.to_wire(),
        )
        .await
        .unwrap();
    let late = CallSdpWire {
        call_id: id(CALL),
        from_device: id(ALICE_PHONE),
        to_device: id(BOB_PHONE),
        sealed_sdp: b"sealed".to_vec(),
    };
    let error = harness
        .calls
        .relay_sdp(&alice(NOW + 3 * SECOND), late)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::CONFLICT);
}

#[tokio::test]
async fn mark_connected_is_idempotent_and_honest() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();

    // A ring cannot be connected: no answer has arrived.
    let error = harness
        .calls
        .mark_connected(id(CALL), ts(NOW + SECOND))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::CONFLICT);

    harness
        .calls
        .answer(&bob(NOW + SECOND), id(CALL), id(BOB_PHONE))
        .await
        .unwrap();
    harness
        .calls
        .mark_connected(id(CALL), ts(NOW + 2 * SECOND))
        .await
        .unwrap();
    // Twice is fine.
    harness
        .calls
        .mark_connected(id(CALL), ts(NOW + 3 * SECOND))
        .await
        .unwrap();
    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Connected);

    // A mark that races an end loses, quietly: the end is the later truth.
    harness
        .calls
        .end(
            &alice(NOW + 4 * SECOND),
            id(CALL),
            EndReason::Network.to_wire(),
        )
        .await
        .unwrap();
    harness
        .calls
        .mark_connected(id(CALL), ts(NOW + 5 * SECOND))
        .await
        .unwrap();
    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Ended);
}

#[tokio::test]
async fn the_sweep_retires_expired_invites_as_no_answer() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL + 1, CAROL))
        .await
        .unwrap();

    // One millisecond before the deadline, nothing is retired.
    let none = harness
        .calls
        .sweep(ts(NOW + RING_TTL_MS - 1))
        .await
        .unwrap();
    assert!(none.is_empty());

    // One millisecond after, both rings died as no-answers. The sweep returns
    // the retired calls themselves — the publisher decides which topics each
    // `ended_event` goes to — so the wire shape is asserted through that.
    let retired = harness
        .calls
        .sweep(ts(NOW + RING_TTL_MS + 1))
        .await
        .unwrap();
    assert_eq!(retired.len(), 2);
    for call in &retired {
        assert_eq!(call.state, CallState::Ended);
        assert_eq!(call.end_reason, Some(EndReason::NoAnswer));
        let event = call.ended_event();
        assert_eq!(event.call_id, call.call_id);
        assert_eq!(event.state, CallState::Ended.to_wire());
        assert_eq!(event.reason, Some(EndReason::NoAnswer.to_wire()));
    }
    for call_id in [id(CALL), id(CALL + 1)] {
        let call = harness.calls.call(&alice(NOW), call_id).await.unwrap();
        assert_eq!(call.state, CallState::Ended);
        assert_eq!(call.end_reason, Some(EndReason::NoAnswer));
        assert_eq!(call.ended_at, Some(ts(NOW + RING_TTL_MS + 1)));
    }

    // A second sweep finds nothing: the dead are already buried.
    let again = harness
        .calls
        .sweep(ts(NOW + RING_TTL_MS + 2))
        .await
        .unwrap();
    assert!(again.is_empty());

    // And a re-invite of a swept id reports the expiry.
    let (outcome, event) = harness
        .calls
        .invite(&alice(NOW + RING_TTL_MS + 3), invite(CALL, BOB))
        .await
        .unwrap();
    assert_eq!(outcome.status, invite_status::EXPIRED);
    assert!(event.is_none());
}

#[tokio::test]
async fn the_store_reports_only_live_calls_for_a_callee() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let active = harness
        .store
        .active_for_callee(id(BOB), ts(NOW + SECOND))
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].call_id, id(CALL));

    // Past the deadline the ring is not live, even before any sweep.
    let expired = harness
        .store
        .active_for_callee(id(BOB), ts(NOW + RING_TTL_MS + 1))
        .await
        .unwrap();
    assert!(expired.is_empty());

    // Nor is somebody else's ring.
    let other = harness
        .store
        .active_for_callee(id(CAROL), ts(NOW + SECOND))
        .await
        .unwrap();
    assert!(other.is_empty());
}

#[tokio::test]
async fn turn_servers_is_configured_and_empty_by_default() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    let servers = harness.calls.turn_servers(id(CALL)).await.unwrap();
    assert!(
        servers.is_empty(),
        "no relay is configured, so none is claimed"
    );
}

#[tokio::test]
async fn turn_servers_returns_what_was_configured() {
    let settings = Config::default();
    let registry = Registry::new();
    let policies =
        Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
    let limiter = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        policies,
        &registry,
    ));
    let relay = TurnServer {
        url: "turn:turn.example.com:3478".to_string(),
        username: "user".to_string(),
        credential: "secret".to_string(),
        ttl_seconds: 300,
        region: "ap-southeast-1".to_string(),
    };
    let config = CallsConfig {
        turn_servers: vec![relay.clone()],
        ..CallsConfig::default()
    };
    let calls = Calls::new(
        Arc::new(MemoryCallStore::new()),
        limiter,
        Arc::new(TestGate::open()),
        &registry,
        config,
    );
    let servers = calls.turn_servers(id(CALL)).await.unwrap();
    assert_eq!(servers, vec![relay]);
}

#[tokio::test]
async fn every_series_is_registered_at_zero() {
    // A counter that springs into existence on its first occurrence cannot
    // be alerted on beforehand, so construction must create them all. The
    // registry renders every series the service owns; a render that finds
    // them proves none is missing.
    let harness = Harness::new();
    let rendered = harness.registry.render();
    for series in [
        "migo_calls_invite_total",
        "migo_calls_answer_total",
        "migo_calls_ended_total",
        "migo_calls_relayed_total",
        "migo_calls_connected_total",
        "migo_calls_expired_total",
    ] {
        assert!(
            rendered.contains(series),
            "{series} must exist before anything happens"
        );
    }
}
