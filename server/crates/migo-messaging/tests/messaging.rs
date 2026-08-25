//! Integration tests for the messaging service.
//!
//! Everything runs against `MemoryStore`, `MemoryCache`, and a `CacheRateLimiter` over
//! the same cache, with a seeded `SeededRandom` and hand-written timestamps. No clock is
//! read and no socket is opened, so a failure here is a failure in the code rather than
//! in the machine — which is the property that makes a test worth keeping when it fails
//! at three in the morning.
//!
//! The tests are written against the properties that would be expensive to get wrong in
//! production rather than against the shape of the code: that a sequence is never reused,
//! that a retry is a success and not a second delivery, that a watermark only moves
//! forward and only speaks when it moved, that a client is told when history is gone
//! instead of being handed a shorter history that looks complete, and that paging a list
//! that is changing underneath the reader neither repeats a row nor drops one.

use std::sync::Arc;

use migo_cache::traits::TypingCache;
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Random, Result, SeededRandom, Timestamp};
use migo_messaging::fanout::Broadcast;
use migo_messaging::model::{Caller, MAX_GROUP_MEMBERS};
use migo_messaging::service::Messages;
use migo_messaging::traits::Messaging;
use migo_protocol::{
    codes, ConversationCreateRequest, ConversationKind, ConversationListRequest,
    ConversationSummary, MessageAccepted, MessageDelete, MessageKind, MessageReceipt, MessageSend,
    Opcode, ReceiptKind, RelationshipKind, SyncRequest, SyncResponse, SyncStatus, TypingEvent,
    TypingState,
};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::Relationship;
use migo_store::traits::{MessagingStore, SocialStore};
use migo_store::MemoryStore;

/// One second in milliseconds.
const SECOND: i64 = 1_000;
/// One minute.
const MINUTE: i64 = 60 * SECOND;

/// Alice, who sends most of the messages.
const ALICE: u128 = 1;
/// Bob, who receives them.
const BOB: u128 = 2;
/// Carol, who is in the group but not the direct conversation.
const CAROL: u128 = 3;
/// Someone with no membership anywhere, for the authorisation tests.
const STRANGER: u128 = 9;

/// Alice's phone. Fanout excludes it when Alice is the sender.
const ALICE_PHONE: u128 = 101;
/// Bob's laptop.
const BOB_LAPTOP: u128 = 102;

type TestMessaging = Messages<MemoryStore, MemoryCache, CacheRateLimiter<MemoryCache>>;

/// Everything a test needs, built the way `migod` builds it.
struct Harness {
    messaging: TestMessaging,
    store: Arc<MemoryStore>,
    cache: Arc<MemoryCache>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        let config = Config::default();
        let store = Arc::new(MemoryStore::new());
        let cache = Arc::new(MemoryCache::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&config.rate_limit).expect("default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::clone(&cache),
            policies,
            &registry,
        ));
        let messaging = Messages::new(
            Arc::clone(&store),
            Arc::clone(&cache),
            limiter,
            &registry,
            Box::new(SeededRandom::new(0x5eed_9001)) as Box<dyn Random>,
        );
        Self {
            messaging,
            store,
            cache,
            registry,
        }
    }

    /// The direct conversation between Alice and Bob, created by Alice.
    async fn direct(&self, millis: i64) -> Id {
        self.messaging
            .create(
                &caller(ALICE, ALICE_PHONE, millis),
                ConversationCreateRequest {
                    kind: ConversationKind::Direct,
                    members: vec![id(BOB)],
                    title: None,
                },
            )
            .await
            .expect("a direct conversation between two strangers is allowed")
            .conversation_id
    }

    /// A group of Alice, Bob, and Carol, created by Alice.
    async fn group(&self, millis: i64) -> Id {
        self.messaging
            .create(
                &caller(ALICE, ALICE_PHONE, millis),
                ConversationCreateRequest {
                    kind: ConversationKind::Group,
                    members: vec![id(BOB), id(CAROL)],
                    title: None,
                },
            )
            .await
            .expect("a group of three is allowed")
            .conversation_id
    }

    /// Sends one text message from Alice's phone.
    async fn send(
        &self,
        conversation: Id,
        message: u128,
        body: &[u8],
        millis: i64,
    ) -> MessageAccepted {
        self.messaging
            .send(
                &caller(ALICE, ALICE_PHONE, millis),
                MessageSend {
                    message_id: id(message),
                    conversation_id: conversation,
                    kind: MessageKind::Text,
                    envelope: body.to_vec(),
                    ..MessageSend::default()
                },
            )
            .await
            .expect("a member may send to their own conversation")
            .0
    }

    /// Blocks `b` from `a`'s side.
    async fn block(&self, a: u128, b: u128, millis: i64) {
        self.store
            .put_relationship(Relationship {
                account_id: id(a),
                other_id: id(b),
                kind: RelationshipKind::Block,
                created_at: ts(millis),
                accepted_at: None,
            })
            .await
            .expect("a block can always be recorded");
    }

    /// The value of a single metric series, or `None` if it was never registered.
    fn metric(&self, series: &str) -> Option<f64> {
        self.registry
            .render()
            .lines()
            .find(|line| line.starts_with(series))
            .and_then(|line| line.rsplit(' ').next().map(str::to_string))
            .and_then(|value| value.parse().ok())
    }
}

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

/// A caller at `millis`, on the ordinary trust tier.
///
/// `Established` and not `Trusted`: the tier a real user has is the tier the tests should
/// meet the limits on, or the limits are only ever exercised by people who do not have
/// them.
fn caller(account: u128, device: u128, millis: i64) -> Caller {
    Caller::new(id(account), id(device), TrustTier::Established, ts(millis))
}

/// Asserts that a call failed with one specific protocol code.
///
/// The code and not the message, so that rewording an internal string does not break a
/// test while a change of failure *class* does.
#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    match result {
        Ok(_) => panic!("expected error {code}, got success"),
        Err(error) => assert_eq!(error.code(), code, "wrong failure class: {error}"),
    }
}

/// The message inside a broadcast, or a panic naming what arrived instead.
#[track_caller]
fn message_of(broadcast: &Broadcast) -> &migo_protocol::MessageEvent {
    match broadcast {
        Broadcast::Message(event) => event,
        other => panic!("expected a message broadcast, got {other:?}"),
    }
}

/// The receipt inside a broadcast.
#[track_caller]
fn receipt_of(broadcast: &Broadcast) -> &MessageReceipt {
    match broadcast {
        Broadcast::Receipt(receipt) => receipt,
        other => panic!("expected a receipt broadcast, got {other:?}"),
    }
}

/// A forward sync from `have_seq`.
fn sync_from(conversation: Id, have_seq: u64, limit: u32) -> SyncRequest {
    SyncRequest {
        conversation_id: conversation,
        have_seq,
        limit,
        ..SyncRequest::default()
    }
}

/// The sequences carried by a sync response, for comparing against an expectation that
/// reads like the conversation does.
fn seqs(response: &SyncResponse) -> Vec<u64> {
    response.messages.iter().map(|m| m.seq).collect()
}

/// The conversation ids of a list page, in order.
fn listed(page: &[ConversationSummary]) -> Vec<Id> {
    page.iter().map(|row| row.conversation_id).collect()
}

// --- sending -------------------------------------------------------------------------

#[tokio::test]
async fn a_message_is_sequenced_from_one_and_fanned_out_to_everyone_but_the_sending_device() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;

    let (accepted, fanout) = harness
        .messaging
        .send(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
            MessageSend {
                message_id: id(1_001),
                conversation_id: conversation,
                kind: MessageKind::Text,
                envelope: b"sealed".to_vec(),
                ..MessageSend::default()
            },
        )
        .await
        .expect("a member may send");

    assert_eq!(
        accepted.seq, 1,
        "the first message in a conversation is seq 1"
    );
    assert_eq!(accepted.duplicate, None, "a first send is not a duplicate");

    let fanout = fanout.expect("a new message has an audience");
    assert_eq!(fanout.conversation_id, conversation);
    assert_eq!(
        fanout.exclude_device,
        Some(id(ALICE_PHONE)),
        "the sending device gets an acknowledgement, not a copy of its own message"
    );
    assert_eq!(fanout.event.opcode(), Opcode::MessageEvent);

    let event = message_of(&fanout.event);
    assert_eq!(event.seq, 1);
    assert_eq!(event.sender_id, id(ALICE));
    assert_eq!(event.sender_device, id(ALICE_PHONE));
    assert_eq!(
        event.envelope,
        b"sealed".to_vec(),
        "the envelope is relayed as it arrived"
    );
    assert_eq!(
        event.deleted, None,
        "a live message carries no deletion flag"
    );
    assert_eq!(
        event.sender_key_id, None,
        "the authoritative sender key id is bound inside the envelope, not echoed beside it"
    );
}

#[tokio::test]
async fn sequences_are_gapless_across_senders() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;

    let mut seen = Vec::new();
    for (index, (account, device)) in [
        (ALICE, ALICE_PHONE),
        (BOB, BOB_LAPTOP),
        (ALICE, ALICE_PHONE),
        (CAROL, ALICE_PHONE),
    ]
    .into_iter()
    .enumerate()
    {
        let at = 2 * MINUTE + index as i64 * SECOND;
        let (accepted, _) = harness
            .messaging
            .send(
                &caller(account, device, at),
                MessageSend {
                    message_id: id(2_000 + index as u128),
                    conversation_id: conversation,
                    kind: MessageKind::Text,
                    envelope: vec![index as u8],
                    ..MessageSend::default()
                },
            )
            .await
            .expect("every member may send");
        seen.push(accepted.seq);
    }

    assert_eq!(
        seen,
        vec![1, 2, 3, 4],
        "one sequence per message, no gaps, no reuse"
    );
}

#[tokio::test]
async fn a_retry_of_the_same_message_id_is_a_success_and_not_a_second_delivery() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    let request = MessageSend {
        message_id: id(3_001),
        conversation_id: conversation,
        kind: MessageKind::Text,
        envelope: b"exactly once".to_vec(),
        ..MessageSend::default()
    };

    let (first, first_fanout) = harness
        .messaging
        .send(&caller(ALICE, ALICE_PHONE, 2 * MINUTE), request.clone())
        .await
        .expect("the first send is accepted");
    let (second, second_fanout) = harness
        .messaging
        .send(&caller(ALICE, ALICE_PHONE, 3 * MINUTE), request)
        .await
        .expect("a retry is a success, not an error");

    assert_eq!(
        second.seq, first.seq,
        "a retry does not consume a second sequence"
    );
    assert_eq!(
        second.created_at, first.created_at,
        "a retry keeps the original's time"
    );
    assert_eq!(
        second.duplicate,
        Some(true),
        "the client is told it was a retry"
    );
    assert!(first_fanout.is_some(), "the first send is delivered");
    assert!(
        second_fanout.is_none(),
        "a retry produces no second broadcast, and therefore no second notification"
    );
}

#[tokio::test]
async fn the_same_message_id_carrying_a_different_message_is_refused() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    let base = MessageSend {
        message_id: id(4_001),
        conversation_id: conversation,
        kind: MessageKind::Text,
        envelope: b"the original".to_vec(),
        ..MessageSend::default()
    };
    harness
        .messaging
        .send(&caller(ALICE, ALICE_PHONE, 2 * MINUTE), base.clone())
        .await
        .expect("the first send is accepted");

    expect_code(
        harness
            .messaging
            .send(
                &caller(ALICE, ALICE_PHONE, 3 * MINUTE),
                MessageSend {
                    envelope: b"something else entirely".to_vec(),
                    ..base.clone()
                },
            )
            .await
            .map(|_| ()),
        codes::IDEMPOTENCY_MISMATCH,
    );
    expect_code(
        harness
            .messaging
            .send(
                &caller(ALICE, ALICE_PHONE, 4 * MINUTE),
                MessageSend {
                    kind: MessageKind::Sticker,
                    ..base.clone()
                },
            )
            .await
            .map(|_| ()),
        codes::IDEMPOTENCY_MISMATCH,
    );
    expect_code(
        harness
            .messaging
            .send(&caller(BOB, BOB_LAPTOP, 5 * MINUTE), base)
            .await
            .map(|_| ()),
        codes::IDEMPOTENCY_MISMATCH,
    );
}

#[tokio::test]
async fn sending_marks_the_senders_own_copy_read() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    harness
        .send(conversation, 5_001, b"hello", 2 * MINUTE)
        .await;

    let unread = harness
        .store
        .conversations_with_unread(id(ALICE))
        .await
        .expect("the store can always answer");
    assert!(
        unread.is_empty(),
        "a sender's own message must not count against their unread badge: {unread:?}"
    );
    let waiting = harness
        .store
        .conversations_with_unread(id(BOB))
        .await
        .expect("the store can always answer");
    assert_eq!(waiting.len(), 1, "everyone else has one message waiting");
}

#[tokio::test]
async fn a_stranger_cannot_tell_a_missing_conversation_from_one_they_are_not_in() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;

    let outsider = harness
        .messaging
        .send(
            &caller(STRANGER, BOB_LAPTOP, 2 * MINUTE),
            MessageSend {
                message_id: id(6_001),
                conversation_id: conversation,
                kind: MessageKind::Text,
                envelope: b"let me in".to_vec(),
                ..MessageSend::default()
            },
        )
        .await;
    let absent = harness
        .messaging
        .send(
            &caller(STRANGER, BOB_LAPTOP, 2 * MINUTE),
            MessageSend {
                message_id: id(6_002),
                conversation_id: id(999_999),
                kind: MessageKind::Text,
                envelope: b"anybody there".to_vec(),
                ..MessageSend::default()
            },
        )
        .await;

    expect_code(outsider.map(|_| ()), codes::NOT_FOUND);
    expect_code(absent.map(|_| ()), codes::NOT_FOUND);
}

#[tokio::test]
async fn a_block_stops_a_direct_message_in_both_directions() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness
        .send(conversation, 7_001, b"before", 2 * MINUTE)
        .await;
    harness.block(BOB, ALICE, 3 * MINUTE).await;

    expect_code(
        harness
            .messaging
            .send(
                &caller(ALICE, ALICE_PHONE, 4 * MINUTE),
                MessageSend {
                    message_id: id(7_002),
                    conversation_id: conversation,
                    kind: MessageKind::Text,
                    envelope: b"after".to_vec(),
                    ..MessageSend::default()
                },
            )
            .await
            .map(|_| ()),
        codes::BLOCKED_BY_USER,
    );
    expect_code(
        harness
            .messaging
            .send(
                &caller(BOB, BOB_LAPTOP, 4 * MINUTE),
                MessageSend {
                    message_id: id(7_003),
                    conversation_id: conversation,
                    kind: MessageKind::Text,
                    envelope: b"also blocked".to_vec(),
                    ..MessageSend::default()
                },
            )
            .await
            .map(|_| ()),
        codes::BLOCKED_BY_USER,
    );
}

#[tokio::test]
async fn a_block_does_not_reach_inside_a_group() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    harness.block(BOB, ALICE, 2 * MINUTE).await;

    harness
        .send(conversation, 8_001, b"still a group", 3 * MINUTE)
        .await;
}

#[tokio::test]
async fn a_malformed_send_is_refused_before_anything_is_charged_for_it() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    let good = MessageSend {
        message_id: id(9_001),
        conversation_id: conversation,
        kind: MessageKind::Text,
        envelope: b"fine".to_vec(),
        ..MessageSend::default()
    };

    let cases: [(MessageSend, u32); 5] = [
        (
            MessageSend {
                message_id: Id::NIL,
                ..good.clone()
            },
            codes::FIELD_REQUIRED,
        ),
        (
            MessageSend {
                envelope: Vec::new(),
                ..good.clone()
            },
            codes::FIELD_REQUIRED,
        ),
        (
            MessageSend {
                envelope: vec![0u8; migo_wire::limits::MAX_BYTES_LEN + 1],
                ..good.clone()
            },
            codes::FIELD_TOO_LONG,
        ),
        (
            MessageSend {
                kind: MessageKind::Unknown,
                ..good.clone()
            },
            codes::VALIDATION_FAILED,
        ),
        (
            MessageSend {
                expires_in_ms: Some(0),
                ..good.clone()
            },
            codes::VALIDATION_FAILED,
        ),
    ];
    for (request, code) in cases {
        expect_code(
            harness
                .messaging
                .send(&caller(ALICE, ALICE_PHONE, 2 * MINUTE), request)
                .await
                .map(|_| ()),
            code,
        );
    }

    assert_eq!(
        harness.metric("migo_messaging_send_total{outcome=\"invalid\"}"),
        Some(5.0),
        "every refusal is counted, so a client bug is visible without reading logs"
    );
    harness
        .messaging
        .send(&caller(ALICE, ALICE_PHONE, 3 * MINUTE), good)
        .await
        .expect("a malformed frame must not have spent the sender's budget");
}

// --- receipts ------------------------------------------------------------------------

#[tokio::test]
async fn a_receipt_moves_a_watermark_once_and_says_nothing_the_second_time() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness.send(conversation, 10_001, b"one", 2 * MINUTE).await;

    let request = MessageReceipt {
        conversation_id: conversation,
        kind: ReceiptKind::Read,
        seq: 1,
        ..MessageReceipt::default()
    };
    let first = harness
        .messaging
        .receipt(&caller(BOB, BOB_LAPTOP, 3 * MINUTE), request.clone())
        .await
        .expect("a member may report a receipt");
    let again = harness
        .messaging
        .receipt(&caller(BOB, BOB_LAPTOP, 4 * MINUTE), request)
        .await
        .expect("a repeated receipt is not an error");

    let fanout = first.expect("a watermark that moved is worth telling the sender about");
    let receipt = receipt_of(&fanout.event);
    assert_eq!(receipt.seq, 1);
    assert_eq!(receipt.kind, ReceiptKind::Read);
    assert_eq!(
        receipt.user_id,
        Some(id(BOB)),
        "the subject is filled in by the server, so no member can speak for another"
    );
    assert_eq!(receipt.at, Some(ts(3 * MINUTE)));
    assert!(
        again.is_none(),
        "nothing changed the second time, so nothing is sent (brief section 156)"
    );
}

#[tokio::test]
async fn a_read_receipt_implies_delivery() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness.send(conversation, 11_001, b"one", 2 * MINUTE).await;
    harness.send(conversation, 11_002, b"two", 3 * MINUTE).await;

    harness
        .messaging
        .receipt(
            &caller(BOB, BOB_LAPTOP, 4 * MINUTE),
            MessageReceipt {
                conversation_id: conversation,
                kind: ReceiptKind::Read,
                seq: 2,
                ..MessageReceipt::default()
            },
        )
        .await
        .expect("a read receipt is accepted on its own");

    let cursor = harness
        .store
        .cursor(conversation, id(BOB))
        .await
        .expect("the store can always answer");
    assert_eq!(cursor.read_seq, 2);
    assert_eq!(
        cursor.delivered_seq, 2,
        "a message that was read was necessarily delivered, and the two must not disagree"
    );
}

#[tokio::test]
async fn a_receipt_past_the_end_is_clamped_and_one_that_goes_backwards_is_ignored() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness.send(conversation, 12_001, b"one", 2 * MINUTE).await;
    harness.send(conversation, 12_002, b"two", 3 * MINUTE).await;

    let ahead = harness
        .messaging
        .receipt(
            &caller(BOB, BOB_LAPTOP, 4 * MINUTE),
            MessageReceipt {
                conversation_id: conversation,
                kind: ReceiptKind::Read,
                seq: u64::MAX,
                ..MessageReceipt::default()
            },
        )
        .await
        .expect("a client racing a new message is not making an error");
    let ahead = ahead.expect("the watermark moved");
    assert_eq!(
        receipt_of(&ahead.event).seq,
        2,
        "a sequence beyond the end is clamped to the end, not refused"
    );

    let backwards = harness
        .messaging
        .receipt(
            &caller(BOB, BOB_LAPTOP, 5 * MINUTE),
            MessageReceipt {
                conversation_id: conversation,
                kind: ReceiptKind::Read,
                seq: 1,
                ..MessageReceipt::default()
            },
        )
        .await
        .expect("an out-of-order receipt is not an error either");
    assert!(
        backwards.is_none(),
        "a watermark only moves forward, and a no-op is not broadcast"
    );
    assert_eq!(
        harness
            .store
            .cursor(conversation, id(BOB))
            .await
            .expect("the store can always answer")
            .read_seq,
        2,
        "a confused client must not be able to reset its own read state"
    );
}

#[tokio::test]
async fn a_receipt_in_an_empty_conversation_says_nothing() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;

    let answer = harness
        .messaging
        .receipt(
            &caller(BOB, BOB_LAPTOP, 2 * MINUTE),
            MessageReceipt {
                conversation_id: conversation,
                kind: ReceiptKind::Delivered,
                seq: 5,
                ..MessageReceipt::default()
            },
        )
        .await
        .expect("a receipt for nothing is not an error");
    assert!(answer.is_none(), "there is no watermark to move yet");
}

// --- deletion ------------------------------------------------------------------------

#[tokio::test]
async fn only_the_sender_may_delete_and_the_tombstone_keeps_its_sequence() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    harness
        .send(conversation, 13_001, b"first", 2 * MINUTE)
        .await;
    let target = harness
        .send(conversation, 13_002, b"regrettable", 3 * MINUTE)
        .await;
    harness
        .send(conversation, 13_003, b"third", 4 * MINUTE)
        .await;

    expect_code(
        harness
            .messaging
            .delete(
                &caller(BOB, BOB_LAPTOP, 5 * MINUTE),
                MessageDelete {
                    message_id: id(13_002),
                    conversation_id: conversation,
                    for_everyone: true,
                },
            )
            .await
            .map(|_| ()),
        codes::PERMISSION_DENIED,
    );

    let (accepted, fanout) = harness
        .messaging
        .delete(
            &caller(ALICE, ALICE_PHONE, 6 * MINUTE),
            MessageDelete {
                message_id: id(13_002),
                conversation_id: conversation,
                for_everyone: true,
            },
        )
        .await
        .expect("a sender may withdraw their own message");

    assert_eq!(
        accepted.seq, target.seq,
        "a tombstone keeps its sequence, so the numbering has no hole to read as data loss"
    );
    let fanout = fanout.expect("a deletion has an audience");
    let event = message_of(&fanout.event);
    assert_eq!(event.deleted, Some(true));
    assert!(
        event.envelope.is_empty(),
        "the payload goes with the deletion; keeping it would make delete mean hide"
    );

    let history = harness
        .messaging
        .sync(
            &caller(BOB, BOB_LAPTOP, 7 * MINUTE),
            sync_from(conversation, 0, 10),
        )
        .await
        .expect("history is readable");
    assert_eq!(
        seqs(&history),
        vec![1, 2, 3],
        "the tombstone stays in the sequence so every client converges on the deletion"
    );
}

#[tokio::test]
async fn deleting_twice_is_a_success_with_no_second_broadcast() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness
        .send(conversation, 14_001, b"oops", 2 * MINUTE)
        .await;
    let request = MessageDelete {
        message_id: id(14_001),
        conversation_id: conversation,
        for_everyone: true,
    };
    harness
        .messaging
        .delete(&caller(ALICE, ALICE_PHONE, 3 * MINUTE), request.clone())
        .await
        .expect("the first deletion succeeds");

    let (accepted, fanout) = harness
        .messaging
        .delete(&caller(ALICE, ALICE_PHONE, 4 * MINUTE), request)
        .await
        .expect("a retried deletion is a success, like a retried send");
    assert_eq!(accepted.duplicate, Some(true));
    assert!(
        fanout.is_none(),
        "the tombstone already exists, so nothing changed"
    );
}

#[tokio::test]
async fn deleting_for_me_only_is_refused_rather_than_silently_ignored() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness
        .send(conversation, 15_001, b"stays", 2 * MINUTE)
        .await;

    expect_code(
        harness
            .messaging
            .delete(
                &caller(ALICE, ALICE_PHONE, 3 * MINUTE),
                MessageDelete {
                    message_id: id(15_001),
                    conversation_id: conversation,
                    for_everyone: false,
                },
            )
            .await
            .map(|_| ()),
        codes::FEATURE_DISABLED,
    );
}

// --- sync ----------------------------------------------------------------------------

#[tokio::test]
async fn sync_walks_forward_in_pages_and_reports_what_remains() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    for index in 0..5u128 {
        harness
            .send(
                conversation,
                16_000 + index,
                &[index as u8],
                2 * MINUTE + index as i64 * SECOND,
            )
            .await;
    }

    let mut have = 0u64;
    let mut walked = Vec::new();
    loop {
        let page = harness
            .messaging
            .sync(
                &caller(BOB, BOB_LAPTOP, 3 * MINUTE),
                sync_from(conversation, have, 2),
            )
            .await
            .expect("a member may read history");
        assert_eq!(
            page.status,
            SyncStatus::Ok,
            "nothing was lost, so nothing is claimed lost"
        );
        assert!(
            page.from_seq <= page.to_seq,
            "a range is reported low end first"
        );
        walked.extend(seqs(&page));
        if !page.more {
            break;
        }
        have = page.to_seq;
        assert!(
            have > 0,
            "a page that reports more must have moved the watermark"
        );
    }

    assert_eq!(
        walked,
        vec![1, 2, 3, 4, 5],
        "paging covers everything exactly once"
    );
}

#[tokio::test]
async fn a_client_that_is_already_current_gets_an_empty_range() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness.send(conversation, 17_001, b"one", 2 * MINUTE).await;

    let page = harness
        .messaging
        .sync(
            &caller(BOB, BOB_LAPTOP, 3 * MINUTE),
            sync_from(conversation, 1, 50),
        )
        .await
        .expect("a member may read history");
    assert!(page.messages.is_empty());
    assert!(!page.more);
    assert_eq!(page.status, SyncStatus::Ok);
    assert_eq!(
        (page.from_seq, page.to_seq),
        (0, 0),
        "sequences start at one, so a zero range is unambiguously empty"
    );
}

#[tokio::test]
async fn sync_tells_the_truth_when_history_is_gone() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness
        .messaging
        .send(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
            MessageSend {
                message_id: id(18_001),
                conversation_id: conversation,
                kind: MessageKind::Text,
                envelope: b"disappearing".to_vec(),
                expires_in_ms: Some(SECOND as u32),
                ..MessageSend::default()
            },
        )
        .await
        .expect("a disappearing message is accepted");
    harness
        .send(conversation, 18_002, b"still here", 3 * MINUTE)
        .await;

    let purged = harness
        .messaging
        .purge_expired(ts(4 * MINUTE), 100)
        .await
        .expect("the sweeper can always run");
    assert_eq!(purged, 1);
    assert_eq!(harness.metric("migo_messaging_expired_total"), Some(1.0));

    let page = harness
        .messaging
        .sync(
            &caller(BOB, BOB_LAPTOP, 5 * MINUTE),
            sync_from(conversation, 0, 50),
        )
        .await
        .expect("a member may read history");
    assert_eq!(seqs(&page), vec![2]);
    assert_eq!(
        page.status,
        SyncStatus::Truncated,
        "the gap is reported, not hidden behind a shorter history that looks complete"
    );
    assert!(!page.more, "there is nothing to come back for");
}

#[tokio::test]
async fn sync_backwards_returns_an_ascending_page_ending_where_it_was_asked_to() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    for index in 0..5u128 {
        harness
            .send(
                conversation,
                19_000 + index,
                &[index as u8],
                2 * MINUTE + index as i64 * SECOND,
            )
            .await;
    }

    let newest = harness
        .messaging
        .sync(
            &caller(BOB, BOB_LAPTOP, 3 * MINUTE),
            SyncRequest {
                conversation_id: conversation,
                have_seq: 0,
                limit: 2,
                backwards: Some(true),
                ..SyncRequest::default()
            },
        )
        .await
        .expect("scrolling up is reading history");
    assert_eq!(
        seqs(&newest),
        vec![4, 5],
        "a backwards page is the newest messages, handed over oldest first"
    );
    assert_eq!((newest.from_seq, newest.to_seq), (4, 5));
    assert!(newest.more, "there is older history above");

    let older = harness
        .messaging
        .sync(
            &caller(BOB, BOB_LAPTOP, 4 * MINUTE),
            SyncRequest {
                conversation_id: conversation,
                have_seq: newest.from_seq,
                limit: 2,
                backwards: Some(true),
                ..SyncRequest::default()
            },
        )
        .await
        .expect("scrolling up again");
    assert_eq!(
        seqs(&older),
        vec![2, 3],
        "the next page continues below the first"
    );
}

#[tokio::test]
async fn a_ranged_sync_fetches_exactly_the_gap_a_client_detected() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    for index in 0..6u128 {
        harness
            .send(
                conversation,
                20_000 + index,
                &[index as u8],
                2 * MINUTE + index as i64 * SECOND,
            )
            .await;
    }

    let gap = harness
        .messaging
        .sync(
            &caller(BOB, BOB_LAPTOP, 3 * MINUTE),
            SyncRequest {
                conversation_id: conversation,
                have_seq: 2,
                limit: 50,
                to_seq: Some(4),
                ..SyncRequest::default()
            },
        )
        .await
        .expect("a member may ask for one range");
    assert_eq!(
        seqs(&gap),
        vec![3, 4],
        "the range is inclusive at both ends"
    );
    assert!(
        gap.more,
        "the conversation continues past the requested range"
    );
}

#[tokio::test]
async fn an_abusive_sync_page_is_clamped_not_refused() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    for index in 0..3u128 {
        harness
            .send(
                conversation,
                21_000 + index,
                &[index as u8],
                2 * MINUTE + index as i64 * SECOND,
            )
            .await;
    }

    let page = harness
        .messaging
        .sync(
            &caller(BOB, BOB_LAPTOP, 3 * MINUTE),
            sync_from(conversation, 0, u32::MAX),
        )
        .await
        .expect("brief section 157 clamps a page, it does not reject one");
    assert_eq!(seqs(&page), vec![1, 2, 3]);
}

#[tokio::test]
async fn history_stays_readable_to_a_member_who_never_sent_anything() {
    let harness = Harness::new();
    let conversation = harness.group(MINUTE).await;
    harness.send(conversation, 22_001, b"one", 2 * MINUTE).await;

    expect_code(
        harness
            .messaging
            .sync(
                &caller(STRANGER, BOB_LAPTOP, 3 * MINUTE),
                sync_from(conversation, 0, 10),
            )
            .await
            .map(|_| ()),
        codes::NOT_FOUND,
    );
    let page = harness
        .messaging
        .sync(
            &caller(CAROL, BOB_LAPTOP, 3 * MINUTE),
            sync_from(conversation, 0, 10),
        )
        .await
        .expect("a member reads history whether or not they have spoken");
    assert_eq!(seqs(&page), vec![1]);
}

// --- the conversation list -----------------------------------------------------------

#[tokio::test]
async fn the_conversation_list_is_activity_ordered_and_carries_the_callers_own_state() {
    let harness = Harness::new();
    let quiet = harness.direct(MINUTE).await;
    let busy = harness.group(2 * MINUTE).await;
    harness.send(quiet, 23_001, b"early", 3 * MINUTE).await;
    harness.send(busy, 23_002, b"late", 4 * MINUTE).await;

    let page = harness
        .messaging
        .conversations(
            &caller(ALICE, ALICE_PHONE, 5 * MINUTE),
            ConversationListRequest::default(),
        )
        .await
        .expect("a caller may list their own conversations");

    assert_eq!(
        listed(&page.conversations),
        vec![busy, quiet],
        "most recently active first"
    );
    assert_eq!(page.next_cursor, None, "a short page is the last page");
    let first = &page.conversations[0];
    assert_eq!(first.last_seq, 1);
    assert_eq!(first.read_seq, 1, "the sender has read their own message");
    assert_eq!(
        first.pinned, None,
        "an unset flag is absent, not present and false"
    );
    assert_eq!(first.muted_until, None);
    assert_eq!(first.archived, None);
    assert_eq!(
        first.title, None,
        "a name belongs to the room aggregate, not to a list row assembled here"
    );
    let preview = first
        .last_message
        .as_ref()
        .expect("a conversation with a message previews it");
    assert_eq!(preview.envelope, b"late".to_vec());
    assert_eq!(
        page.conversations[0].members.as_ref().map(Vec::len),
        Some(3),
        "the row carries a bounded member preview, not the whole roster"
    );
}

#[tokio::test]
async fn the_conversation_list_pages_by_cursor_without_repeating_or_dropping_a_row() {
    let harness = Harness::new();
    let mut created = Vec::new();
    for index in 0..5u128 {
        let at = MINUTE + index as i64 * SECOND;
        let conversation = harness
            .messaging
            .create(
                &caller(ALICE, ALICE_PHONE, at),
                ConversationCreateRequest {
                    kind: ConversationKind::Group,
                    members: vec![id(BOB), id(24_000 + index)],
                    title: None,
                },
            )
            .await
            .expect("a group is creatable")
            .conversation_id;
        harness
            .send(
                conversation,
                24_100 + index,
                &[index as u8],
                2 * MINUTE + index as i64 * SECOND,
            )
            .await;
        created.push(conversation);
    }

    let whole = harness
        .messaging
        .conversations(
            &caller(ALICE, ALICE_PHONE, 5 * MINUTE),
            ConversationListRequest {
                limit: 50,
                cursor: None,
            },
        )
        .await
        .expect("the whole list fits in one page");
    let expected = listed(&whole.conversations);
    assert_eq!(expected.len(), 5);

    let mut walked = Vec::new();
    let mut cursor = None;
    loop {
        let page = harness
            .messaging
            .conversations(
                &caller(ALICE, ALICE_PHONE, 6 * MINUTE),
                ConversationListRequest { limit: 2, cursor },
            )
            .await
            .expect("paging a list is not a privilege");
        walked.extend(listed(&page.conversations));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(
        walked, expected,
        "two rows at a time covers exactly what one page of five did"
    );
}

#[tokio::test]
async fn an_abusive_list_page_is_clamped_and_a_malformed_cursor_is_refused() {
    let harness = Harness::new();
    harness.direct(MINUTE).await;

    harness
        .messaging
        .conversations(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
            ConversationListRequest {
                limit: u32::MAX,
                cursor: None,
            },
        )
        .await
        .expect("brief section 157 clamps a page, it does not reject one");

    for bad in ["", "v1", "v2.1.2.3", "v1.-.-.notanid", "v1.1.2.3.4"] {
        expect_code(
            harness
                .messaging
                .conversations(
                    &caller(ALICE, ALICE_PHONE, 3 * MINUTE),
                    ConversationListRequest {
                        limit: 10,
                        cursor: Some(bad.to_string()),
                    },
                )
                .await
                .map(|_| ()),
            codes::VALIDATION_FAILED,
        );
    }
}

// --- creating ------------------------------------------------------------------------

#[tokio::test]
async fn creating_a_direct_conversation_twice_converges_on_one() {
    let harness = Harness::new();
    let first = harness.direct(MINUTE).await;
    harness.send(first, 25_001, b"history", 2 * MINUTE).await;

    let again = harness
        .messaging
        .create(
            &caller(ALICE, ALICE_PHONE, 3 * MINUTE),
            ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![id(BOB)],
                title: None,
            },
        )
        .await
        .expect("tapping message twice is not an error");
    assert_eq!(
        again.conversation_id, first,
        "two devices converge on one conversation"
    );
    assert_eq!(
        again.last_seq, 1,
        "an existing conversation reports its real history, not an empty one"
    );

    let from_the_other_side = harness
        .messaging
        .create(
            &caller(BOB, BOB_LAPTOP, 4 * MINUTE),
            ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![id(ALICE)],
                title: None,
            },
        )
        .await
        .expect("the pair is unordered");
    assert_eq!(from_the_other_side.conversation_id, first);
}

#[tokio::test]
async fn creating_drops_the_caller_from_their_own_member_list() {
    let harness = Harness::new();
    let summary = harness
        .messaging
        .create(
            &caller(ALICE, ALICE_PHONE, MINUTE),
            ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![id(ALICE), id(BOB), id(BOB)],
                title: None,
            },
        )
        .await
        .expect("a redundant member list is a client habit, not an error");

    assert_eq!(summary.kind, ConversationKind::Direct);
    assert_eq!(
        summary.members,
        Some(vec![id(ALICE), id(BOB)]),
        "the caller and the duplicate collapse into one two-person conversation"
    );
}

#[tokio::test]
async fn a_conversation_needs_somebody_other_than_its_creator() {
    let harness = Harness::new();
    for members in [vec![], vec![id(ALICE)]] {
        expect_code(
            harness
                .messaging
                .create(
                    &caller(ALICE, ALICE_PHONE, MINUTE),
                    ConversationCreateRequest {
                        kind: ConversationKind::Group,
                        members,
                        title: None,
                    },
                )
                .await
                .map(|_| ()),
            codes::VALIDATION_FAILED,
        );
    }
}

#[tokio::test]
async fn the_shapes_a_create_refuses() {
    let harness = Harness::new();
    let cases: [(ConversationCreateRequest, u32); 5] = [
        (
            ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![id(BOB), id(CAROL)],
                title: None,
            },
            codes::VALIDATION_FAILED,
        ),
        (
            ConversationCreateRequest {
                kind: ConversationKind::Room,
                members: vec![id(BOB)],
                title: None,
            },
            codes::VALIDATION_FAILED,
        ),
        (
            ConversationCreateRequest {
                kind: ConversationKind::Unknown,
                members: vec![id(BOB)],
                title: None,
            },
            codes::VALIDATION_FAILED,
        ),
        (
            ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: (0..MAX_GROUP_MEMBERS as u128)
                    .map(|n| id(30_000 + n))
                    .collect(),
                title: None,
            },
            codes::VALIDATION_FAILED,
        ),
        (
            ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![id(BOB)],
                title: Some("Book club".to_string()),
            },
            codes::FEATURE_DISABLED,
        ),
    ];

    for (request, code) in cases {
        expect_code(
            harness
                .messaging
                .create(&caller(ALICE, ALICE_PHONE, MINUTE), request)
                .await
                .map(|_| ()),
            code,
        );
    }
}

#[tokio::test]
async fn a_blocked_member_cannot_be_pulled_into_a_new_conversation() {
    let harness = Harness::new();
    harness.block(CAROL, ALICE, MINUTE).await;

    expect_code(
        harness
            .messaging
            .create(
                &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
                ConversationCreateRequest {
                    kind: ConversationKind::Group,
                    members: vec![id(BOB), id(CAROL)],
                    title: None,
                },
            )
            .await
            .map(|_| ()),
        codes::BLOCKED_BY_USER,
    );
}

// --- typing --------------------------------------------------------------------------

#[tokio::test]
async fn typing_is_marked_in_the_cache_and_a_refresh_is_not_suppressed() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    let start = TypingEvent {
        conversation_id: conversation,
        state: TypingState::Start,
        user_id: None,
    };

    let first = harness
        .messaging
        .typing(&caller(ALICE, ALICE_PHONE, 2 * MINUTE), start.clone())
        .await
        .expect("a member may say they are typing");
    let fanout = first.expect("typing has an audience");
    assert_eq!(fanout.event.opcode(), Opcode::Typing);
    match &fanout.event {
        Broadcast::Typing(event) => {
            assert_eq!(event.state, TypingState::Start);
            assert_eq!(
                event.user_id,
                Some(id(ALICE)),
                "the subject is the authenticated caller, never whoever the frame claimed"
            );
        }
        other => panic!("expected a typing broadcast, got {other:?}"),
    }
    assert_eq!(
        harness
            .cache
            .typing(conversation, ts(2 * MINUTE))
            .await
            .expect("the cache can answer"),
        vec![id(ALICE)]
    );

    let refresh = harness
        .messaging
        .typing(&caller(ALICE, ALICE_PHONE, 2 * MINUTE + 3 * SECOND), start)
        .await
        .expect("a refresh is accepted");
    assert!(
        refresh.is_some(),
        "a refresh moved the deadline, so it is a change and must be relayed"
    );
    assert_eq!(
        harness
            .cache
            .typing(conversation, ts(2 * MINUTE + 12 * SECOND))
            .await
            .expect("the cache can answer"),
        vec![id(ALICE)],
        "the refreshed mark outlives the first one's deadline"
    );
}

#[tokio::test]
async fn typing_stop_clears_the_mark() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;
    harness
        .messaging
        .typing(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
            TypingEvent {
                conversation_id: conversation,
                state: TypingState::Start,
                user_id: None,
            },
        )
        .await
        .expect("start is accepted");

    harness
        .messaging
        .typing(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE + SECOND),
            TypingEvent {
                conversation_id: conversation,
                state: TypingState::Stop,
                user_id: None,
            },
        )
        .await
        .expect("stop is accepted")
        .expect("stop is relayed, so the indicator goes away before its TTL");
    assert!(
        harness
            .cache
            .typing(conversation, ts(2 * MINUTE + SECOND))
            .await
            .expect("the cache can answer")
            .is_empty(),
        "a client that stopped early is not left showing as typing"
    );
}

#[tokio::test]
async fn typing_needs_a_membership_and_a_known_state() {
    let harness = Harness::new();
    let conversation = harness.direct(MINUTE).await;

    expect_code(
        harness
            .messaging
            .typing(
                &caller(STRANGER, BOB_LAPTOP, 2 * MINUTE),
                TypingEvent {
                    conversation_id: conversation,
                    state: TypingState::Start,
                    user_id: None,
                },
            )
            .await
            .map(|_| ()),
        codes::NOT_FOUND,
    );
    expect_code(
        harness
            .messaging
            .typing(
                &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
                TypingEvent {
                    conversation_id: conversation,
                    state: TypingState::Unknown,
                    user_id: None,
                },
            )
            .await
            .map(|_| ()),
        codes::VALIDATION_FAILED,
    );
}

// --- observability -------------------------------------------------------------------

#[tokio::test]
async fn every_series_is_registered_before_anything_happens() {
    let harness = Harness::new();
    for series in [
        "migo_messaging_send_total{outcome=\"accepted\"}",
        "migo_messaging_send_total{outcome=\"duplicate\"}",
        "migo_messaging_send_total{outcome=\"blocked\"}",
        "migo_messaging_sync_total{outcome=\"truncated\"}",
        "migo_messaging_receipts_total",
        "migo_messaging_deletes_total",
        "migo_messaging_typing_total",
        "migo_messaging_conversations_created_total",
        "migo_messaging_conversation_pages_total",
        "migo_messaging_receipts_ignored_total",
        "migo_messaging_expired_total",
    ] {
        assert_eq!(
            harness.metric(series),
            Some(0.0),
            "{series} must exist at zero, so a dashboard has a line before the first incident"
        );
    }

    let conversation = harness.direct(MINUTE).await;
    harness.send(conversation, 26_001, b"one", 2 * MINUTE).await;
    assert_eq!(
        harness.metric("migo_messaging_send_total{outcome=\"accepted\"}"),
        Some(1.0)
    );
    assert_eq!(
        harness.metric("migo_messaging_conversations_created_total"),
        Some(1.0)
    );
}
