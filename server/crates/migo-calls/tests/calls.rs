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
    call_direction, call_outcome, invite_status, CallIceWire, CallInviteWire, CallSdpWire,
    CallState, Caller, CallsConfig, EndReason, GroupJoinOutcome, HISTORY_RETENTION_MS,
    MAX_GROUP_PARTICIPANTS, RING_TTL_MS,
};
use migo_calls::store::{CallStore, MemoryCallStore};
use migo_calls::traits::{CallGate, Callkeeper};
use migo_calls::{Calls, GroupCallStore, MemoryGroupCallStore, SharedGroupCallStore};
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Timestamp};
use migo_protocol::{codes, CallRating, CallStats, Opcode, TurnServer};
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
/// A second call id: a refusal stores nothing, so the video invite below needs
/// an id of its own rather than reusing the ringing one.
const CALL_VIDEO: u128 = 61;

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

/// The same invite, as a video call: the only field that differs is the one
/// section 180's split is about.
fn video_invite(call_id: u128, callee: u128) -> CallInviteWire {
    CallInviteWire {
        media_kind: 1,
        ..invite(call_id, callee)
    }
}

/// The gate as a test needs it: a membership list, a block list, and a list
/// of callees the social graph refuses, all answerable without a store.
struct TestGate {
    members: HashMap<Id, Vec<Id>>,
    blocked: Vec<(Id, Id)>,
    unreachable: Vec<Id>,
    /// Callees who take an audio call and refuse a video one — the split
    /// section 180 asks for, as a gate answer.
    no_video: Vec<Id>,
}

impl TestGate {
    /// Alice and Bob may call inside the conversation.
    fn open() -> Self {
        Self {
            members: HashMap::from([(id(CONVERSATION), vec![id(ALICE), id(BOB), id(CAROL)])]),
            blocked: Vec::new(),
            unreachable: Vec::new(),
            no_video: Vec::new(),
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

    /// The same, with Bob refusing video calls only: his voice line stays open.
    fn refused_video_only() -> Self {
        Self {
            no_video: vec![id(BOB)],
            ..Self::open()
        }
    }

    /// Nobody may call: the conversation is closed to the caller.
    fn closed() -> Self {
        Self {
            members: HashMap::new(),
            blocked: Vec::new(),
            unreachable: Vec::new(),
            no_video: Vec::new(),
        }
    }

    /// The same, admitting enough distinct members to fill a roster to its
    /// ceiling and still name one more — the seats are per account, so the
    /// ceiling test needs accounts, not devices.
    fn wide() -> Self {
        let mut members = vec![id(ALICE), id(BOB), id(CAROL)];
        members.extend((200..200 + MAX_GROUP_PARTICIPANTS as u128 + 1).map(id));
        Self {
            members: HashMap::from([(id(CONVERSATION), members)]),
            ..Self::open()
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

    async fn can_call(&self, _caller: &migo_calls::Caller, callee_id: Id, media_kind: u32) -> bool {
        if self.unreachable.contains(&callee_id) {
            return false;
        }
        // Video is the one kind that is not audio; the service refuses any other
        // value before the gate is asked.
        !(media_kind == migo_calls::MEDIA_VIDEO && self.no_video.contains(&callee_id))
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

    fn refused_video_only() -> Self {
        Self::gated(TestGate::refused_video_only())
    }

    fn closed() -> Self {
        Self::gated(TestGate::closed())
    }

    fn gated(gate: TestGate) -> Self {
        Self::gated_with_config(gate, CallsConfig::default())
    }

    /// The same harness over a turned knob: the seat grace, which the sweep's
    /// tests wait out and the production default sizes in tens of seconds.
    fn with_seat_grace(grace_ms: i64) -> Self {
        Self::gated_with_config(
            TestGate::open(),
            CallsConfig {
                seat_grace_ms: grace_ms,
                ..CallsConfig::default()
            },
        )
    }

    /// The same harness, handing back the group store the service seats its
    /// rosters in, for the tests that must write to it directly — the stale
    /// roster clones a retirement has to outlive.
    fn with_exposed_group_store(grace_ms: i64) -> (Self, Arc<MemoryGroupCallStore>) {
        let settings = Config::default();
        let store = Arc::new(MemoryCallStore::new());
        let groups = Arc::new(MemoryGroupCallStore::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        // The service seats its rosters behind the shared trait object; the
        // concrete handle travels on, because the stale-write test must reach
        // the memory backend's own `put`.
        let shared: SharedGroupCallStore = groups.clone();
        let calls = Calls::with_group_store(
            Arc::clone(&store),
            shared,
            limiter,
            Arc::new(TestGate::open()),
            &registry,
            CallsConfig {
                seat_grace_ms: grace_ms,
                ..CallsConfig::default()
            },
        );
        (
            Self {
                calls,
                store,
                registry,
            },
            groups,
        )
    }

    fn gated_with_config(gate: TestGate, config: CallsConfig) -> Self {
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
            config,
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
async fn turning_video_off_leaves_the_voice_line_ringing() {
    let harness = Harness::refused_video_only();

    // The audio call rings, which is the whole point of deciding the two kinds
    // separately: a callee who does not want to be seen has not said anything
    // about being spoken to.
    let (outcome, event) = harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    assert_ne!(outcome.status, invite_status::BLOCKED);
    assert!(event.is_some(), "an audio invite reaches the callee");
    assert!(harness.store.get(id(CALL)).await.unwrap().is_some());

    // The video call answers exactly what a block answers, and stores nothing —
    // so the caller cannot read the callee's video policy off the reply, and a
    // policy widened tomorrow does not find a row answering with today's refusal.
    let (refused, event) = harness
        .calls
        .invite(&alice(NOW), video_invite(CALL_VIDEO, BOB))
        .await
        .unwrap();
    assert_eq!(refused.status, invite_status::BLOCKED);
    assert_eq!(refused.expires_at, ts(NOW));
    assert!(event.is_none());
    assert!(harness.store.get(id(CALL_VIDEO)).await.unwrap().is_none());
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
        .decline(&bob(NOW + SECOND), id(CALL), 1)
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
        .decline(&bob(NOW + 2 * SECOND), id(CALL), 1)
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
        .end(&alice(NOW), id(CALL), 7)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::VALIDATION_FAILED);
}

#[tokio::test]
async fn a_busy_decline_ends_the_call_busy_not_declined() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    // 0=Busy: the callee's devices were occupied. The event must carry that
    // reason, because "busy" invites the caller to try again while
    // "declined" reports a human refusal that never happened.
    let event = harness
        .calls
        .decline(&bob(NOW + SECOND), id(CALL), 0)
        .await
        .unwrap()
        .expect("the caller is told");
    assert_eq!(event.state, CallState::Ended.to_wire());
    assert_eq!(event.reason, Some(EndReason::Busy.to_wire()));

    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Ended);
    assert_eq!(call.end_reason, Some(EndReason::Busy));

    // A retried invite against the busy id reports busy, for the same reason
    // the original decline did: the status is the caller's retry decision.
    let (outcome, event) = harness
        .calls
        .invite(&alice(NOW + 3 * SECOND), invite(CALL, BOB))
        .await
        .unwrap();
    assert_eq!(outcome.status, invite_status::BUSY);
    assert!(event.is_none());
}

#[tokio::test]
async fn an_unknown_decline_reason_is_the_clients_fault() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    // 0=Busy, 1=Declined are the wire's whole decline vocabulary; anything
    // else is refused rather than guessed at, so a caller is never told a
    // gentler (or harsher) fact than the callee stated.
    let error = harness
        .calls
        .decline(&bob(NOW + SECOND), id(CALL), 2)
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::VALIDATION_FAILED);

    // The call is untouched by the refusal: still ringing.
    let call = harness.calls.call(&alice(NOW), id(CALL)).await.unwrap();
    assert_eq!(call.state, CallState::Ringing);
}

#[tokio::test]
async fn the_answer_relay_publishes_the_connected_event_once() {
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
    let sealed = [7u8; 16];
    let sdp = CallSdpWire {
        call_id: id(CALL),
        from_device: id(BOB_PHONE),
        to_device: id(ALICE_PHONE),
        sealed_sdp: sealed.to_vec(),
    };
    let (relayed, event) = harness
        .calls
        .relay_sdp(&bob(NOW + SECOND), sdp)
        .await
        .unwrap();
    assert_eq!(relayed.sealed_sdp, sealed.to_vec());
    let event = event.expect("the callee's first answer connects the call");
    assert_eq!(event.call_id, id(CALL));
    assert_eq!(event.state, CallState::Connected.to_wire());

    // A renegotiation relay changes nothing: no second Connected event, the
    // store already said it, and a client that re-rendered from a duplicate
    // would restart its duration timer.
    let renegotiated = CallSdpWire {
        call_id: id(CALL),
        from_device: id(ALICE_PHONE),
        to_device: id(BOB_PHONE),
        sealed_sdp: [9u8; 16].to_vec(),
    };
    let (_, again) = harness
        .calls
        .relay_sdp(&alice(NOW + 2 * SECOND), renegotiated)
        .await
        .unwrap();
    assert!(again.is_none());
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
    let (relayed, connected) = harness
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
    // The callee's first answer is what connects the call, and the event
    // says so to both parties.
    let connected = connected.expect("the first answer connects the call");
    assert_eq!(connected.state, CallState::Connected.to_wire());
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
    let (relayed, again) = harness
        .calls
        .relay_sdp(&alice(NOW + 3 * SECOND), offer.clone())
        .await
        .unwrap();
    assert_eq!(relayed, offer);
    assert!(again.is_none(), "a renegotiation connects nothing");
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
async fn stats_from_a_party_count_and_stats_from_a_stranger_do_not() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .expect("the call rings");

    // A party's sample, with both quality numbers the series care about.
    harness
        .calls
        .stats(
            &alice(NOW),
            CallStats {
                call_id: id(CALL),
                setup_ms: Some(2_500),
                used_turn: Some(true),
                ..CallStats::default()
            },
        )
        .await
        .expect("a party may report quality numbers");
    assert_eq!(setup_samples(&harness.registry), 1);
    assert_eq!(
        harness
            .registry
            .counter("migo_call_turn_fallback_total", "", &[])
            .get(),
        1
    );

    // The same party's post-call rating, with two problems ticked. Section 180
    // asks for both halves, and they land on separate series: one verdict, and
    // one increment per problem named, because a rating that named two of them
    // is two facts about the call rather than one.
    harness
        .calls
        .stats(
            &alice(NOW),
            CallStats {
                call_id: id(CALL),
                rating: Some(CallRating::Poor),
                issues: Some(0b0101),
                ..CallStats::default()
            },
        )
        .await
        .expect("a party may rate its own call");
    assert_eq!(
        harness
            .registry
            .counter("migo_call_rating_total", "", &[("rating", "poor")])
            .get(),
        1
    );
    assert_eq!(
        harness
            .registry
            .counter("migo_call_issue_total", "", &[("issue", "audio")])
            .get(),
        1,
        "bit 0 is the audio problem"
    );
    assert_eq!(
        harness
            .registry
            .counter("migo_call_issue_total", "", &[("issue", "connection")])
            .get(),
        1,
        "bit 2 is the connection problem"
    );
    // The bit nobody set, and the verdict nobody gave: both are registered at
    // zero, which is what lets an alert be written before the first one lands.
    assert_eq!(
        harness
            .registry
            .counter("migo_call_issue_total", "", &[("issue", "dropped")])
            .get(),
        0
    );
    assert_eq!(
        harness
            .registry
            .counter("migo_call_rating_total", "", &[("rating", "excellent")])
            .get(),
        0
    );

    // A stranger naming the same id: answered silence, and no series moves —
    // a metrics frame must not become a probe for which calls exist.
    harness
        .calls
        .stats(
            &caller(CAROL, CAROL_PHONE, NOW),
            CallStats {
                call_id: id(CALL),
                setup_ms: Some(9_999),
                used_turn: Some(true),
                rating: Some(CallRating::Excellent),
                issues: Some(0b1111),
                ..CallStats::default()
            },
        )
        .await
        .expect("a stranger is not refused, only uncounted");
    assert_eq!(setup_samples(&harness.registry), 1);
    assert_eq!(
        harness
            .registry
            .counter("migo_call_turn_fallback_total", "", &[])
            .get(),
        1
    );
    assert_eq!(
        harness
            .registry
            .counter("migo_call_rating_total", "", &[("rating", "excellent")])
            .get(),
        0,
        "a stranger's verdict about a call it is not on is not counted"
    );
    assert_eq!(
        harness
            .registry
            .counter("migo_call_issue_total", "", &[("issue", "video")])
            .get(),
        0
    );

    // A nil id is a shape error, the same as every other method here.
    let error = harness
        .calls
        .stats(
            &alice(NOW),
            CallStats {
                call_id: Id::NIL,
                ..CallStats::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::FIELD_REQUIRED);
}

/// How many observations the setup histogram holds.
fn setup_samples(registry: &Registry) -> u64 {
    registry
        .histogram("migo_call_setup_seconds", "", &[], &[1.0])
        .count()
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
        "migo_call_setup_seconds",
        "migo_call_turn_fallback_total",
    ] {
        assert!(
            rendered.contains(series),
            "{series} must exist before anything happens"
        );
    }
}
// --- the group call, as this node forwards it ---------------------------------
//
// The SFU's questions, asked where the answers are invisible when wrong:
// who gets a seat, who hears about it, what a retry costs (nothing), and
// what the relay refuses to move.

const GROUP_CALL: u128 = 70;

#[allow(dead_code)]
fn group_join_frame(call_id: u128, sealed: &[u8]) -> CallInviteWire {
    CallInviteWire {
        call_id: id(call_id),
        conversation_id: id(CONVERSATION),
        callee_id: id(BOB), // ignored by the group path; the roster is the audience
        media_kind: 0,
        caller_device: id(ALICE_PHONE),
        capabilities: 0,
        sealed_offer: sealed.to_vec(),
    }
}

#[tokio::test]
async fn a_group_join_seats_the_roster_and_announces_it() {
    let harness = Harness::new();
    let (outcome, call, events) = harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, GroupJoinOutcome::Joined);
    assert_eq!(call.participants.len(), 1);
    assert_eq!(call.participants[0].account_id, id(ALICE));
    // The announcement names the joiner and the size after the change.
    assert_eq!(events.len(), 1, "a first join announces itself once");
    let event = &events[0];
    assert_eq!(event.user_id, Some(id(ALICE)));
    assert_eq!(event.participant_count, Some(1));
    assert_eq!(
        event.sealed_offer.as_deref(),
        Some(b"alice-sealed".as_ref())
    );
}

#[tokio::test]
async fn a_retried_group_join_is_the_same_seat() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    let (outcome, call, events) = harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, GroupJoinOutcome::Duplicate);
    assert_eq!(call.participants.len(), 1, "no second seat for one device");
    assert!(events.is_empty(), "nobody is told about a retry");
}

#[tokio::test]
async fn two_members_join_and_each_roster_holds_both() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    let (outcome, call, events) = harness
        .calls
        .group_join(
            &bob(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"bob-sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, GroupJoinOutcome::Joined);
    assert_eq!(call.participants.len(), 2);
    assert_eq!(
        events
            .last()
            .expect("the second join announces")
            .participant_count,
        Some(2)
    );
    assert_eq!(call.participants[0].account_id, id(ALICE));
    assert_eq!(call.participants[1].account_id, id(BOB));
    // The roster preserves each participant's sealed offer untouched.
    assert_eq!(call.participants[0].sealed_offer, b"alice-sealed");
    assert_eq!(call.participants[1].sealed_offer, b"bob-sealed");
}

#[tokio::test]
async fn a_stranger_cannot_join_the_call() {
    let harness = Harness::closed();
    let error = harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
}

#[tokio::test]
async fn a_new_device_replaces_the_seat_and_the_roster_hears_both_halves() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"phone-sealed".to_vec(),
        )
        .await
        .unwrap();
    let laptop = caller(ALICE, BOB_LAPTOP, NOW);
    let (outcome, call, events) = harness
        .calls
        .group_join(
            &laptop,
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"laptop-sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, GroupJoinOutcome::Joined);
    assert_eq!(call.participants.len(), 1, "one account holds one seat");
    assert_eq!(call.participants[0].device_id, id(BOB_LAPTOP));
    // The replacement is two facts in order: the departure of the seat the
    // account held, then the arrival of the new one. The old device's clients
    // act on the departure; everyone else renders the arrival.
    assert_eq!(
        events.len(),
        2,
        "a replacement is a departure and an arrival"
    );
    assert_eq!(events[0].state, migo_calls::group_store::GROUP_STATE_ENDED);
    assert_eq!(events[0].user_id, Some(id(ALICE)));
    assert_eq!(events[0].device_id, Some(id(ALICE_PHONE)));
    assert_eq!(events[0].participant_count, Some(0));
    assert_eq!(
        events[1].state,
        migo_calls::group_store::GROUP_STATE_CONNECTED
    );
    assert_eq!(events[1].user_id, Some(id(ALICE)));
    assert_eq!(events[1].device_id, Some(id(BOB_LAPTOP)));
    assert_eq!(events[1].participant_count, Some(1));
}

#[tokio::test]
async fn the_roster_ceiling_is_refused_not_silently_dropped() {
    let harness = Harness::gated(TestGate::wide());
    // Fill the roster to the ceiling with one seat per account — the seat is
    // the account's, so cycling devices of a few accounts would only ever
    // replace seats, never fill them.
    let mut account: u128 = 200;
    for _ in 0..MAX_GROUP_PARTICIPANTS {
        let who = caller(account, account + 1000, NOW);
        let sealed = format!("seat-{account}").into_bytes();
        harness
            .calls
            .group_join(&who, id(GROUP_CALL), id(CONVERSATION), 0, sealed)
            .await
            .unwrap();
        account += 1;
    }
    // An account that holds no seat now finds the roster full.
    let who = caller(account, account + 1000, NOW);
    let error = harness
        .calls
        .group_join(&who, id(GROUP_CALL), id(CONVERSATION), 0, b"x".to_vec())
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::VALIDATION_FAILED);
    // A seated account's replacement still fits: one account holds one seat,
    // and a replacement does not grow the roster.
    let seated = caller(200, 9000, NOW);
    let (outcome, call, _) = harness
        .calls
        .group_join(
            &seated,
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"replacement-sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, GroupJoinOutcome::Joined);
    assert_eq!(call.participants.len(), MAX_GROUP_PARTICIPANTS);
}

#[tokio::test]
async fn a_leave_empties_the_seat_and_the_last_leave_retires_the_call() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    harness
        .calls
        .group_join(
            &bob(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"bob-sealed".to_vec(),
        )
        .await
        .unwrap();
    let event = harness
        .calls
        .group_leave(&alice(NOW + SECOND), id(GROUP_CALL))
        .await
        .unwrap()
        .expect("the departure is announced");
    assert_eq!(event.user_id, Some(id(ALICE)));
    assert_eq!(event.participant_count, Some(1));
    // The last leave retires the call: the id is spent, and a later leave
    // under it finds no call at all.
    let event = harness
        .calls
        .group_leave(&bob(NOW + 2 * SECOND), id(GROUP_CALL))
        .await
        .unwrap()
        .expect("the last departure is announced");
    assert_eq!(event.participant_count, Some(0));
    let error = harness
        .calls
        .group_leave(&bob(NOW + 3 * SECOND), id(GROUP_CALL))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
}

#[tokio::test]
async fn a_stranger_s_leave_changes_nothing_and_is_told_so() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    // Bob is a member of the conversation but holds no seat.
    let event = harness
        .calls
        .group_leave(&bob(NOW + SECOND), id(GROUP_CALL))
        .await
        .unwrap();
    assert!(event.is_none(), "a seat nobody holds cannot leave");
    // And the call is untouched.
    let (_, call, _) = harness
        .calls
        .group_join(
            &alice(NOW + SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(call.participants.len(), 1);
}

#[tokio::test]
async fn the_id_is_spent_on_the_conversation_it_was_minted_for() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    let error = harness
        .calls
        .group_join(&bob(NOW), id(GROUP_CALL), id(51), 0, b"bob-sealed".to_vec())
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::IDEMPOTENCY_MISMATCH);
}

#[tokio::test]
async fn a_group_relay_moves_only_between_seated_devices() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    harness
        .calls
        .group_join(
            &bob(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"bob-sealed".to_vec(),
        )
        .await
        .unwrap();
    // Alice's device may relay toward Bob's.
    let call = harness
        .calls
        .group_relay(
            &alice(NOW + SECOND),
            Opcode::CallSdp,
            id(GROUP_CALL),
            id(ALICE_PHONE),
            id(BOB_PHONE),
            b"sealed-sdp",
        )
        .await
        .unwrap();
    assert_eq!(call.participants.len(), 2);
    // A device that holds no seat cannot relay.
    let error = harness
        .calls
        .group_relay(
            &bob(NOW + SECOND),
            Opcode::CallSdp,
            id(GROUP_CALL),
            id(CAROL_PHONE),
            id(BOB_PHONE),
            b"sealed-sdp",
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::PERMISSION_DENIED);
    // And nobody may relay to a device outside the roster.
    let error = harness
        .calls
        .group_relay(
            &alice(NOW + SECOND),
            Opcode::CallSdp,
            id(GROUP_CALL),
            id(ALICE_PHONE),
            id(BOB_LAPTOP),
            b"sealed-sdp",
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::PERMISSION_DENIED);
    // Nor to themselves.
    let error = harness
        .calls
        .group_relay(
            &alice(NOW + SECOND),
            Opcode::CallSdp,
            id(GROUP_CALL),
            id(ALICE_PHONE),
            id(ALICE_PHONE),
            b"sealed-sdp",
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::PERMISSION_DENIED);
    // An empty payload is refused by the field it names, and an ICE batch is
    // told about the field ICE actually carries — the group relay serves
    // three frames, so the name has to come from the one that arrived.
    let error = harness
        .calls
        .group_relay(
            &alice(NOW + SECOND),
            Opcode::CallIce,
            id(GROUP_CALL),
            id(ALICE_PHONE),
            id(BOB_PHONE),
            b"",
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::FIELD_REQUIRED);
    assert!(
        format!("{error}").contains("sealed_candidates"),
        "an ICE batch is named by its own field, not the SDP's: {error}"
    );
}

/// The key update's audience is the whole roster, and it is the *seats* that
/// decide it.
///
/// Section 166 rotates a group call's frame key through `CALL_KEY_UPDATE`
/// whenever the membership changes, and section 180 requires it — a leaver
/// must not read what follows, a joiner must not read what came before. The
/// frame names no target, so a rotation that reached only some participants
/// would leave the rest unable to read the media that follows it. So the
/// audience is every seated account, and it is read from the roster rather
/// than from the frame.
#[tokio::test]
async fn a_group_key_update_reaches_the_whole_roster_once_per_account() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    harness
        .calls
        .group_join(
            &bob(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"bob-sealed".to_vec(),
        )
        .await
        .unwrap();
    // Both seats, and the sender's own among them: Alice's other devices hold
    // the same frame key and rotate with everyone else, so the account that
    // minted the rotation is a recipient of it too.
    let audience = harness
        .calls
        .group_key_audience(&alice(NOW + SECOND), id(GROUP_CALL))
        .await
        .unwrap();
    assert_eq!(audience, vec![id(ALICE), id(BOB)]);
    // The rotation is symmetrical: Bob's device reaches the same set.
    let audience = harness
        .calls
        .group_key_audience(&bob(NOW + SECOND), id(GROUP_CALL))
        .await
        .unwrap();
    assert_eq!(audience, vec![id(ALICE), id(BOB)]);
    // A *seat* is what counts, not an account with one: Bob's other device
    // holds no seat, so it holds no frame key either, and a device with no
    // key has nothing to rotate.
    let error = harness
        .calls
        .group_key_audience(&caller(BOB, BOB_LAPTOP, NOW + SECOND), id(GROUP_CALL))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
    // A stranger to the call gets the same answer.
    let error = harness
        .calls
        .group_key_audience(&caller(CAROL, CAROL_PHONE, NOW + SECOND), id(GROUP_CALL))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
    // And an id that names no group call at all is the same answer, which is
    // what lets the dispatcher fall through to the 1:1 read.
    let error = harness
        .calls
        .group_key_audience(&alice(NOW + SECOND), id(1_000))
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::NOT_FOUND);
}

#[tokio::test]
async fn every_group_series_is_registered_at_zero() {
    let harness = Harness::new();
    let rendered = harness.registry.render();
    for series in [
        "migo_calls_group_join_total",
        "migo_calls_group_left_total",
        "migo_calls_group_relayed_total",
        "migo_calls_group_rekeyed_total",
    ] {
        assert!(
            rendered.contains(series),
            "{series} must exist before anything happens"
        );
    }
}

// --- the deaths a session edge reports -----------------------------------------
//
// A socket that closes without a leave is the one death no frame reports: the
// tests here pin what the dispatcher's session_ended edge and the sweep it arms
// do with that fact — a group seat stamped, swept after the grace, and a
// connected 1:1 call ended for the survivor.

#[tokio::test]
async fn a_seat_whose_session_died_is_swept_after_the_grace() {
    let harness = Harness::with_seat_grace(SECOND);
    for who in [alice(NOW), bob(NOW)] {
        harness
            .calls
            .group_join(
                &who,
                id(GROUP_CALL),
                id(CONVERSATION),
                0,
                b"sealed".to_vec(),
            )
            .await
            .unwrap();
    }

    // The session edge: Bob's socket died, and only the mark is written — the
    // grace window belongs to the re-join that may still come.
    harness
        .calls
        .group_session_ended(id(BOB), id(BOB_PHONE), ts(NOW + SECOND))
        .await
        .unwrap();

    // Before the grace ends, the sweep is a no-op: the roster still holds two
    // seats, and nothing is announced. The read is a duplicate re-join — the
    // one call that hands the roster back.
    let early = harness
        .calls
        .group_sweep(ts(NOW + SECOND + SECOND / 2))
        .await
        .unwrap();
    assert!(early.is_empty(), "the grace window is still open");
    let (_, call, _) = harness
        .calls
        .group_join(
            &alice(NOW + SECOND + SECOND / 2),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(call.participants.len(), 2);

    // Once the grace has passed, the sweep retires the seat and hands back the
    // departure announcement the roster is owed — the same shape an explicit
    // leave produces, with the reason telling the truth about a departure
    // nobody chose to send.
    let departures = harness
        .calls
        .group_sweep(ts(NOW + 3 * SECOND))
        .await
        .unwrap();
    assert_eq!(departures.len(), 1, "the dead seat is retired");
    let event = &departures[0];
    assert_eq!(event.call_id, id(GROUP_CALL));
    assert_eq!(event.conversation_id, Some(id(CONVERSATION)));
    assert_eq!(event.user_id, Some(id(BOB)));
    assert_eq!(event.device_id, Some(id(BOB_PHONE)));
    assert_eq!(event.participant_count, Some(1));
    assert_eq!(event.state, CallState::Ended.to_wire());
    assert_eq!(event.reason, Some(EndReason::Network.to_wire()));

    let (_, call, _) = harness
        .calls
        .group_join(
            &alice(NOW + 3 * SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(
        call.participants.len(),
        1,
        "the roster no longer renders Bob"
    );
    assert_eq!(call.participants[0].account_id, id(ALICE));

    // A swept roster is not swept twice.
    let again = harness
        .calls
        .group_sweep(ts(NOW + 10 * SECOND))
        .await
        .unwrap();
    assert!(
        again.is_empty(),
        "the live seat is not retirement's business"
    );
}

#[tokio::test]
async fn a_rejoin_inside_the_grace_keeps_the_seat() {
    let harness = Harness::with_seat_grace(SECOND);
    for who in [alice(NOW), bob(NOW)] {
        harness
            .calls
            .group_join(
                &who,
                id(GROUP_CALL),
                id(CONVERSATION),
                0,
                b"sealed".to_vec(),
            )
            .await
            .unwrap();
    }
    harness
        .calls
        .group_session_ended(id(BOB), id(BOB_PHONE), ts(NOW + SECOND))
        .await
        .unwrap();

    // Bob's client came back before the grace expired — same device, same
    // sealed offer — so the join is the duplicate it always was, and the mark
    // the dead socket left is cleared.
    let (outcome, call, events) = harness
        .calls
        .group_join(
            &bob(NOW + SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, GroupJoinOutcome::Duplicate);
    assert_eq!(call.participants.len(), 2);
    assert!(events.is_empty(), "a re-join announces nothing");

    // The grace has long passed, and the seat is still there: the sweep has
    // nothing to retire.
    let departures = harness
        .calls
        .group_sweep(ts(NOW + 10 * SECOND))
        .await
        .unwrap();
    assert!(
        departures.is_empty(),
        "the re-joined seat survived the sweep"
    );
    let (_, call, _) = harness
        .calls
        .group_join(
            &alice(NOW + 10 * SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(call.participants.len(), 2);
}

#[tokio::test]
async fn two_sweeps_that_race_retire_the_seat_once() {
    let harness = Harness::with_seat_grace(SECOND);
    for who in [alice(NOW), bob(NOW)] {
        harness
            .calls
            .group_join(
                &who,
                id(GROUP_CALL),
                id(CONVERSATION),
                0,
                b"sealed".to_vec(),
            )
            .await
            .unwrap();
    }
    harness
        .calls
        .group_session_ended(id(BOB), id(BOB_PHONE), ts(NOW + SECOND))
        .await
        .unwrap();

    // Two sweeps inside the same grace-expired instant, the timer's tick and
    // an opportunistic pass: both read the same stamped roster, and only the
    // one whose retirement took the seat may announce it — one death, one
    // departure, whichever won.
    let (first, second) = tokio::join!(
        harness.calls.group_sweep(ts(NOW + 3 * SECOND)),
        harness.calls.group_sweep(ts(NOW + 3 * SECOND)),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(
        first.len() + second.len(),
        1,
        "the racing sweeps retire the seat exactly once between them"
    );
    let event = first
        .iter()
        .chain(second.iter())
        .next()
        .expect("one of the two sweeps took the seat");
    assert_eq!(event.user_id, Some(id(BOB)));
    assert_eq!(event.reason, Some(EndReason::Network.to_wire()));

    // And the roster one reader holds afterwards names one seat.
    let (_, call, _) = harness
        .calls
        .group_join(
            &alice(NOW + 3 * SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(call.participants.len(), 1);
}

#[tokio::test]
async fn a_retired_seat_cannot_come_back_through_a_stale_roster_write() {
    let (harness, groups) = Harness::with_exposed_group_store(SECOND);
    for who in [alice(NOW), bob(NOW)] {
        harness
            .calls
            .group_join(
                &who,
                id(GROUP_CALL),
                id(CONVERSATION),
                0,
                b"sealed".to_vec(),
            )
            .await
            .unwrap();
    }

    // The stale clone: a writer that read the roster before the retirement —
    // captured here as the roster a duplicate re-join hands back, the one
    // read every writer's own read-modify-write begins with.
    let (_, stale, _) = harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(stale.participants.len(), 2);

    harness
        .calls
        .group_session_ended(id(BOB), id(BOB_PHONE), ts(NOW + SECOND))
        .await
        .unwrap();
    let departures = harness
        .calls
        .group_sweep(ts(NOW + 3 * SECOND))
        .await
        .unwrap();
    assert_eq!(departures.len(), 1, "the dead seat is retired");

    // The stale writer's put: the clone still carries Bob's seat at the
    // `joined_at` it was seated with, and the store must refuse exactly that
    // seat — the roster the retirement already shrunk stays shrunk, and the
    // next sweep has no second death to announce.
    groups.put(&stale).await.unwrap();
    let again = harness
        .calls
        .group_sweep(ts(NOW + 10 * SECOND))
        .await
        .unwrap();
    assert!(
        again.is_empty(),
        "a retired seat cannot be resurrected only to die twice"
    );
    let (_, call, _) = harness
        .calls
        .group_join(
            &alice(NOW + 10 * SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(
        call.participants.len(),
        1,
        "the stale write did not put the retired seat back"
    );
    assert_eq!(call.participants[0].account_id, id(ALICE));

    // A fresh join re-seats the device: the tombstone refuses the dead seat,
    // not the member, so the return is the arrival it is announced as.
    let (outcome, call, events) = harness
        .calls
        .group_join(
            &bob(NOW + 10 * SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"sealed-anew".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, GroupJoinOutcome::Joined);
    assert_eq!(call.participants.len(), 2);
    assert_eq!(events.len(), 1, "the return is announced as an arrival");
}

#[tokio::test]
async fn a_disconnected_last_session_ends_its_established_calls_network() {
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
        .unwrap()
        .expect("the answer connects the call");

    // The session edge for Bob's last session: the call he can no longer end
    // himself is ended for him, and the survivor is owed the row's event.
    let retired = harness
        .calls
        .end_disconnected(id(BOB), ts(NOW + 2 * SECOND))
        .await
        .unwrap();
    assert_eq!(retired.len(), 1, "the one connected call is retired");
    let call = &retired[0];
    assert_eq!(call.call_id, id(CALL));
    assert_eq!(call.state, CallState::Ended);
    assert_eq!(call.end_reason, Some(EndReason::Network));

    // The event the dispatcher publishes to Alice's topic says the same thing.
    let event = call.ended_event();
    assert_eq!(event.call_id, id(CALL));
    assert_eq!(event.state, CallState::Ended.to_wire());
    assert_eq!(event.reason, Some(EndReason::Network.to_wire()));

    // Idempotent, as every end is: a second edge finds nothing live.
    let again = harness
        .calls
        .end_disconnected(id(BOB), ts(NOW + 3 * SECOND))
        .await
        .unwrap();
    assert!(again.is_empty(), "an ended call cannot end twice");

    // And a ring is not the disconnect path's business: its own deadline
    // retires it, so an account whose ring died with its session leaves the
    // row to the ring sweeper.
    harness
        .calls
        .invite(&alice(NOW), invite(CALL + 1, CAROL))
        .await
        .unwrap();
    let rings = harness
        .calls
        .end_disconnected(id(CAROL), ts(NOW + 4 * SECOND))
        .await
        .unwrap();
    assert!(rings.is_empty(), "a ring is the ring sweeper's business");
}

// --- the listing (section 165: the one call question that is a read) ---------

/// The listing is the union of the scans the stores already offer, and what it
/// must never be is a way to learn about a call you cannot join: the gates that
/// protect the ring protect the read, per roster rather than per call.
#[tokio::test]
async fn a_listing_reports_the_calls_this_account_can_see_and_nothing_else() {
    let harness = Harness::new();
    // A call Alice placed and Bob has answered: Alice is in it.
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
    // A ring aimed at Alice, which she has not answered: she is being offered
    // it, not in it.
    harness
        .calls
        .invite(&bob(NOW + 2 * SECOND), invite(CALL + 1, ALICE))
        .await
        .unwrap();
    // A group call in a conversation Alice is a member of, which she has not
    // joined.
    harness
        .calls
        .group_join(
            &bob(NOW + 3 * SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"bob-sealed".to_vec(),
        )
        .await
        .unwrap();
    // A group call in a conversation she is not: the same service, a gate that
    // admits Bob and not Alice, and the row the listing must not carry. The
    // gate is the one questioned here — the roster exists and holds a seat, so
    // nothing but the membership read keeps it out of Alice's answer.
    let closed = id(CONVERSATION + 1);
    let stranger = Harness::gated(TestGate {
        members: HashMap::from([(closed, vec![id(BOB)])]),
        blocked: Vec::new(),
        unreachable: Vec::new(),
        no_video: Vec::new(),
    });
    stranger
        .calls
        .group_join(
            &bob(NOW + 3 * SECOND),
            id(GROUP_CALL + 1),
            closed,
            0,
            b"bob-sealed".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(
        stranger
            .calls
            .list(&bob(NOW + 4 * SECOND), None)
            .await
            .unwrap()
            .len(),
        1,
        "the roster is real: its own member sees it"
    );
    assert!(
        stranger
            .calls
            .list(&alice(NOW + 4 * SECOND), None)
            .await
            .unwrap()
            .is_empty(),
        "a roster in a conversation the account cannot enter is not a listing line"
    );

    let listed = harness
        .calls
        .list(&alice(NOW + 4 * SECOND), None)
        .await
        .unwrap();
    let ids: Vec<Id> = listed.iter().map(|entry| entry.call_id).collect();
    assert_eq!(
        ids,
        vec![id(CALL + 1), id(CALL), id(GROUP_CALL)],
        "ringing first, then the answered call, then the roster — and nothing else"
    );

    let offered = &listed[0];
    assert_eq!(offered.kind, 0, "a 1:1 call is kind Direct");
    assert_eq!(offered.state, 0, "a ring reports the ring's own state");
    assert_eq!(offered.peer_id, id(BOB), "the peer is the other party");
    assert_eq!(offered.participant_count, 2);
    assert_eq!(
        offered.joined, 0,
        "a ring aimed at the account is not a call it is in"
    );
    assert_eq!(offered.media_kind, Some(0));
    assert_eq!(offered.expires_at, Some(ts(NOW + 2 * SECOND + RING_TTL_MS)));
    assert_eq!(offered.answered_at, None);
    assert_eq!(offered.started_at, None, "a direct row keeps no start");

    let answered = &listed[1];
    assert_eq!(answered.call_id, id(CALL));
    assert_eq!(answered.state, 1, "an answered call is Connecting");
    assert_eq!(answered.peer_id, id(BOB));
    assert_eq!(answered.joined, 1, "the account placed this call");
    assert_eq!(answered.answered_at, Some(ts(NOW + SECOND)));
    // The deadline survives the answer, which is why the field is documented as
    // readable only while the state says the call is still ringing.
    assert_eq!(answered.expires_at, Some(ts(NOW + RING_TTL_MS)));

    let group = &listed[2];
    assert_eq!(group.kind, 1, "a roster is kind Group");
    assert_eq!(group.call_id, id(GROUP_CALL));
    assert_eq!(group.state, 2, "a roster that holds a seat is Connected");
    assert_eq!(
        group.peer_id,
        id(BOB),
        "the peer is the account that founded it"
    );
    assert_eq!(group.participant_count, 1);
    assert_eq!(
        group.joined, 0,
        "the account is a member, not a participant"
    );
    assert_eq!(
        group.media_kind, None,
        "a roster carries no single media kind"
    );
    assert_eq!(group.expires_at, None, "no deadline retires a group call");
    assert_eq!(group.started_at, Some(ts(NOW + 3 * SECOND)));

    // The scope narrows to one conversation, which is the question a screen
    // already showing one asks.
    let scoped = harness
        .calls
        .list(&alice(NOW + 4 * SECOND), Some(id(CONVERSATION)))
        .await
        .unwrap();
    assert!(
        scoped
            .iter()
            .all(|entry| entry.conversation_id == id(CONVERSATION)),
        "every line of a scoped listing belongs to the scope"
    );
    assert_eq!(
        scoped.len(),
        listed.len(),
        "and here the scope excludes nothing"
    );
}

/// The order is total and it is the listing's own promise, so it is asserted
/// rather than left to a sort implementation: a client redrawing from a second
/// listing must not reshuffle a screen it already showed.
#[tokio::test]
async fn a_listing_is_ordered_most_urgent_first_and_stably() {
    let harness = Harness::new();
    // A ring that expires later, placed first, and one that expires sooner,
    // placed second: the deadline is what orders them, not the arrival.
    harness
        .calls
        .invite(&bob(NOW), invite(CALL + 1, ALICE))
        .await
        .unwrap();
    harness
        .calls
        .invite(&bob(NOW), invite(CALL + 2, ALICE))
        .await
        .unwrap();
    // And an answered call, which is less urgent than any ring.
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

    let first = harness
        .calls
        .list(&alice(NOW + 2 * SECOND), None)
        .await
        .unwrap();
    let states: Vec<u32> = first.iter().map(|entry| entry.state).collect();
    assert_eq!(states, vec![0, 0, 1], "ringing before connecting");
    // Both rings share a deadline, so the id is the tiebreak — and it is the
    // same tiebreak on a second listing.
    assert_eq!(first[0].call_id, id(CALL + 1));
    assert_eq!(first[1].call_id, id(CALL + 2));

    let again = harness
        .calls
        .list(&alice(NOW + 3 * SECOND), None)
        .await
        .unwrap();
    let ids: Vec<Id> = again.iter().map(|entry| entry.call_id).collect();
    let first_ids: Vec<Id> = first.iter().map(|entry| entry.call_id).collect();
    assert_eq!(
        ids, first_ids,
        "a second listing does not reshuffle the first"
    );

    // A ring past its deadline is not a line: the sweep has not run, and the
    // read must not report a ring the clock already killed.
    let later = harness
        .calls
        .list(&alice(NOW + RING_TTL_MS + SECOND), None)
        .await
        .unwrap();
    let ids: Vec<Id> = later.iter().map(|entry| entry.call_id).collect();
    assert_eq!(
        ids,
        vec![id(CALL)],
        "the expired rings are gone, the answered call is not"
    );
}

/// The ring an account placed is a call it is in, on every device it owns —
/// which is why the caller's own side of the ring scan exists.
#[tokio::test]
async fn a_listing_shows_the_account_the_ring_it_placed() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();

    // Alice's laptop asks, not the phone that dialled: same account, and the
    // ring is hers.
    let from_laptop = harness
        .calls
        .list(&caller(ALICE, ALICE_PHONE + 1, NOW + SECOND), None)
        .await
        .unwrap();
    assert_eq!(from_laptop.len(), 1);
    assert_eq!(from_laptop[0].call_id, id(CALL));
    assert_eq!(from_laptop[0].joined, 1, "the account placed this ring");
    assert_eq!(from_laptop[0].peer_id, id(BOB));

    // Bob sees the same ring as one aimed at him, and it is not his call yet.
    let for_bob = harness.calls.list(&bob(NOW + SECOND), None).await.unwrap();
    assert_eq!(for_bob.len(), 1);
    assert_eq!(
        for_bob[0].joined, 0,
        "the ring is offered to the callee, not in them"
    );
    assert_eq!(for_bob[0].peer_id, id(ALICE));
}

// --- the call history ------------------------------------------------------
//
// The listing's other half. Every test above ends a call and stops; these read
// what the stores kept after the call died, which is the one question the three
// live scans cannot answer.

#[tokio::test]
async fn a_declined_call_is_history_for_both_parties_one_row_each() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .decline(&bob(NOW + SECOND), id(CALL), 1)
        .await
        .unwrap();

    // The caller's side: she placed it, and it was refused.
    let hers = harness
        .calls
        .history(&alice(NOW + 2 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(hers.len(), 1);
    assert_eq!(hers[0].call_id, id(CALL));
    assert_eq!(hers[0].kind, 0, "a direct call");
    assert_eq!(hers[0].peer_id, id(BOB), "the other party, from her side");
    assert_eq!(hers[0].direction, call_direction::OUTGOING);
    assert_eq!(hers[0].outcome, call_outcome::DECLINED);
    assert_eq!(hers[0].ended_at, ts(NOW + SECOND));
    assert_eq!(hers[0].media_kind, Some(0));
    assert!(
        hers[0].answered_at.is_none(),
        "nobody picked up, so there is no answer time to print"
    );
    assert!(
        hers[0].started_at.is_none(),
        "a direct row has never kept a start"
    );

    // The callee's side: the same call, the other arrow.
    let his = harness
        .calls
        .history(&bob(NOW + 2 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(his.len(), 1);
    assert_eq!(his[0].call_id, id(CALL));
    assert_eq!(his[0].peer_id, id(ALICE));
    assert_eq!(his[0].direction, call_direction::INCOMING);
    assert_eq!(his[0].outcome, call_outcome::DECLINED);
}

#[tokio::test]
async fn an_answered_call_is_history_with_its_answer_time() {
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
    harness
        .calls
        .end(&bob(NOW + 31 * SECOND), id(CALL), 1)
        .await
        .unwrap();

    let hers = harness
        .calls
        .history(&alice(NOW + 32 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(hers.len(), 1);
    assert_eq!(
        hers[0].outcome,
        call_outcome::ANSWERED,
        "a call that connected is answered however it ended"
    );
    assert_eq!(hers[0].answered_at, Some(ts(NOW + SECOND)));
    assert_eq!(hers[0].ended_at, ts(NOW + 31 * SECOND));
}

#[tokio::test]
async fn a_ring_nobody_answered_is_missed_for_both_parties() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    // The sweep is what writes NoAnswer; nothing else ends an ignored ring.
    let retired = harness
        .calls
        .sweep(ts(NOW + RING_TTL_MS + 1))
        .await
        .unwrap();
    assert_eq!(retired.len(), 1);

    let hers = harness
        .calls
        .history(&alice(NOW + RING_TTL_MS + 2), None, None, None)
        .await
        .unwrap();
    assert_eq!(hers[0].outcome, call_outcome::MISSED);
    assert_eq!(hers[0].direction, call_direction::OUTGOING);

    let his = harness
        .calls
        .history(&bob(NOW + RING_TTL_MS + 2), None, None, None)
        .await
        .unwrap();
    assert_eq!(his[0].outcome, call_outcome::MISSED);
    assert_eq!(his[0].direction, call_direction::INCOMING);
}

#[tokio::test]
async fn a_stranger_holds_no_row_in_this_history() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .cancel(&alice(NOW + SECOND), id(CALL))
        .await
        .unwrap();

    // Carol was not a party to it; both accounts that were each hold it.
    let hers = harness
        .calls
        .history(&alice(NOW + 2 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(hers.len(), 1);
    assert_eq!(hers[0].outcome, call_outcome::CANCELLED);

    let carol = harness
        .calls
        .history(
            &caller(CAROL, CAROL_PHONE, NOW + 2 * SECOND),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert!(carol.is_empty());
}

#[tokio::test]
async fn the_history_pages_backwards_from_a_cursor_and_scopes_to_a_conversation() {
    let harness = Harness::new();
    // Three calls, ended a second apart, so the cursor has something to cut.
    for (index, call_id) in [CALL, CALL + 1, CALL + 2].into_iter().enumerate() {
        let at = NOW + (index as i64) * SECOND;
        harness
            .calls
            .invite(&alice(at), invite(call_id, BOB))
            .await
            .unwrap();
        harness
            .calls
            .cancel(&alice(at + SECOND / 2), id(call_id))
            .await
            .unwrap();
        assert_eq!(
            harness
                .calls
                .history(&alice(at + SECOND), Some(id(CONVERSATION)), None, None)
                .await
                .unwrap()
                .len(),
            index + 1,
            "every call above belongs to the scoped conversation"
        );
    }

    // The newest page, one row at a time, then the page behind the oldest row
    // held: the cursor is exclusive, so the row it names never comes back.
    let first = harness
        .calls
        .history(&alice(NOW + 5 * SECOND), None, None, Some(1))
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].call_id, id(CALL + 2));

    let second = harness
        .calls
        .history(
            &alice(NOW + 5 * SECOND),
            None,
            Some(first[0].ended_at),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].call_id, id(CALL + 1));

    let third = harness
        .calls
        .history(
            &alice(NOW + 5 * SECOND),
            None,
            Some(second[0].ended_at),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].call_id, id(CALL));

    // Past the oldest row there is nothing, which is how a client learns it
    // reached the end rather than by counting.
    let past = harness
        .calls
        .history(
            &alice(NOW + 5 * SECOND),
            None,
            Some(third[0].ended_at),
            Some(1),
        )
        .await
        .unwrap();
    assert!(past.is_empty());

    // A conversation nothing happened in is an empty page, not an error.
    let elsewhere = harness
        .calls
        .history(
            &alice(NOW + 5 * SECOND),
            Some(id(CONVERSATION + 1)),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(elsewhere.is_empty());
}

#[tokio::test]
async fn a_group_call_is_history_for_every_account_that_held_a_seat() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    harness
        .calls
        .group_join(
            &bob(NOW + SECOND),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"bob-sealed".to_vec(),
        )
        .await
        .unwrap();
    harness
        .calls
        .group_leave(&bob(NOW + 2 * SECOND), id(GROUP_CALL))
        .await
        .unwrap();
    harness
        .calls
        .group_leave(&alice(NOW + 3 * SECOND), id(GROUP_CALL))
        .await
        .unwrap();

    // Alice founded it, so her side reads outgoing; Bob's reads incoming.
    let hers = harness
        .calls
        .history(&alice(NOW + 4 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(hers.len(), 1);
    assert_eq!(hers[0].kind, 1, "a group call");
    assert_eq!(hers[0].peer_id, id(ALICE), "the founder");
    assert_eq!(hers[0].direction, call_direction::OUTGOING);
    assert_eq!(hers[0].outcome, call_outcome::ANSWERED);
    assert_eq!(hers[0].started_at, Some(ts(NOW)));
    assert_eq!(hers[0].ended_at, ts(NOW + 3 * SECOND));
    assert_eq!(
        hers[0].participant_count,
        Some(2),
        "both accounts that held a seat, counted once each"
    );
    assert!(
        hers[0].answered_at.is_none(),
        "a roster keeps no answer time"
    );
    assert!(hers[0].media_kind.is_none(), "a roster has no single kind");

    let his = harness
        .calls
        .history(&bob(NOW + 4 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(his.len(), 1);
    assert_eq!(his[0].direction, call_direction::INCOMING);
    assert_eq!(his[0].peer_id, id(ALICE));

    // Carol never joined, so the call is not hers to remember.
    let carol = harness
        .calls
        .history(
            &caller(CAROL, CAROL_PHONE, NOW + 4 * SECOND),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert!(carol.is_empty());
}

#[tokio::test]
async fn a_group_call_that_never_emptied_is_not_history_yet() {
    let harness = Harness::new();
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();

    // The roster still holds a seat: the call is live, so the listing has it
    // and the history does not. The two reads must not both answer.
    let listed = harness
        .calls
        .list(&alice(NOW + SECOND), None)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let history = harness
        .calls
        .history(&alice(NOW + SECOND), None, None, None)
        .await
        .unwrap();
    assert!(history.is_empty());
}

#[tokio::test]
async fn both_halves_of_a_history_are_on_one_page_in_one_order() {
    let harness = Harness::new();
    // A group call that ends first, and a direct call that ends second: the
    // page order is when each ended, not which store answered it.
    harness
        .calls
        .group_join(
            &alice(NOW),
            id(GROUP_CALL),
            id(CONVERSATION),
            0,
            b"alice-sealed".to_vec(),
        )
        .await
        .unwrap();
    harness
        .calls
        .group_leave(&alice(NOW + SECOND), id(GROUP_CALL))
        .await
        .unwrap();
    harness
        .calls
        .invite(&alice(NOW + 2 * SECOND), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .cancel(&alice(NOW + 3 * SECOND), id(CALL))
        .await
        .unwrap();

    let page = harness
        .calls
        .history(&alice(NOW + 4 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].call_id, id(CALL), "the direct call ended last");
    assert_eq!(page[1].call_id, id(GROUP_CALL));
}

#[tokio::test]
async fn the_prune_drops_what_aged_out_for_both_parties_at_once() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .cancel(&alice(NOW + SECOND), id(CALL))
        .await
        .unwrap();

    // Nothing is old enough yet.
    let kept = harness
        .calls
        .prune_history(ts(NOW + 2 * SECOND))
        .await
        .unwrap();
    assert_eq!(kept, 0);

    // Past the retention the row goes, and it goes for both parties: it is one
    // row, and half-deleting it would show one side a call the other never had.
    let dropped = harness
        .calls
        .prune_history(ts(NOW + HISTORY_RETENTION_MS + 3 * SECOND))
        .await
        .unwrap();
    assert_eq!(dropped, 1);
    let after = NOW + HISTORY_RETENTION_MS + 4 * SECOND;
    assert!(harness
        .calls
        .history(&alice(after), None, None, None)
        .await
        .unwrap()
        .is_empty());
    assert!(harness
        .calls
        .history(&bob(after), None, None, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_prune_caps_one_accounts_history_by_count() {
    // A store that keeps two ended calls per account, so the third evicts the
    // oldest rather than the map growing without end.
    let harness = Harness::gated_with_config(
        TestGate::open(),
        CallsConfig {
            history_max_per_account: 2,
            ..CallsConfig::default()
        },
    );
    for (index, call_id) in [CALL, CALL + 1, CALL + 2].into_iter().enumerate() {
        let at = NOW + (index as i64) * SECOND;
        harness
            .calls
            .invite(&alice(at), invite(call_id, BOB))
            .await
            .unwrap();
        harness
            .calls
            .cancel(&alice(at + SECOND / 2), id(call_id))
            .await
            .unwrap();
    }

    let dropped = harness
        .calls
        .prune_history(ts(NOW + 4 * SECOND))
        .await
        .unwrap();
    assert_eq!(dropped, 1, "one row over the cap of two");
    let page = harness
        .calls
        .history(&alice(NOW + 4 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].call_id, id(CALL + 2), "the newest is kept");
    assert_eq!(page[1].call_id, id(CALL + 1));

    // Bob was a party to all three, so his own cap is over by the same row: the
    // eviction is the row's, not one side's.
    let his = harness
        .calls
        .history(&bob(NOW + 4 * SECOND), None, None, None)
        .await
        .unwrap();
    assert_eq!(his.len(), 2);
}

#[tokio::test]
async fn a_page_is_clamped_rather_than_refused() {
    let harness = Harness::new();
    harness
        .calls
        .invite(&alice(NOW), invite(CALL, BOB))
        .await
        .unwrap();
    harness
        .calls
        .cancel(&alice(NOW + SECOND), id(CALL))
        .await
        .unwrap();

    // Asking for more than exists is a question, not a mistake: the answer is
    // what there is.
    let page = harness
        .calls
        .history(&alice(NOW + 2 * SECOND), None, None, Some(u32::MAX))
        .await
        .unwrap();
    assert_eq!(page.len(), 1);

    // Zero is a page size no client means, and it is clamped up rather than
    // answered with an empty page a UI would render as "no calls".
    let none = harness
        .calls
        .history(&alice(NOW + 2 * SECOND), None, None, Some(0))
        .await
        .unwrap();
    assert_eq!(none.len(), 1);
}
