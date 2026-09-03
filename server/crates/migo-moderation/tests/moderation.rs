//! Moderation, tested where getting it wrong is expensive and quiet.
//!
//! This crate has two callers and one rule that binds them: every account may file a
//! report, and only staff may act, and the difference between the two is enforced by the
//! type system and by a staff directory the service consults on *every* call. The tests
//! here are about the places where a mistake would not show up as a broken screen:
//!
//! **Identity is proved before anything is spent.** A report from an unidentified session
//! is refused before the rate limiter is touched, because a limiter that charged first
//! would let an attacker drain a stranger's budget by spoofing their account id.
//!
//! **Authorization is resolved here, never trusted.** The `Operator` a caller hands in
//! carries a `powers` field, and the service throws it away and asks the `Roster` again.
//! A test that granted a power on the struct and revoked it in the directory is a test
//! that the directory wins.
//!
//! **Existence is a secret.** Whether there is a report about an account is worth money to
//! the wrong person, so a caller who may not read the queue cannot tell a real report from
//! a missing one: both are `NOT_FOUND`.
//!
//! **No metric names a person.** Brief section 174 forbids a series labelled by an account
//! or a device, and moderation is where that rule is easiest to break with good
//! intentions. One test renders the whole registry and reads it for anybody's id.
//!
//! The rate limiter is the real one over a real cache, so the arithmetic is part of the
//! test: an Established account's burst is two hundred, a report costs twenty, and an
//! action costs ten, which is why the budget tests count to ten and to twenty.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::random::OsRandom;
use migo_core::{Id, Result, Secret, Timestamp};
use migo_moderation::model::REPORT_WINDOW_MS;
use migo_moderation::service::Moderation;
use migo_moderation::traits::Roster;
use migo_moderation::{
    effective_tier, risk_of, score, Action, Assessment, Caller, Filing, ModerationConfig, Notice,
    Operator, Outcome, Powers, Reason, Resolution, Risk, Signals, Subject, Warden, DEFAULT_PAGE,
    MAX_NOTE_LEN, MAX_PAGE, MAX_REASON_LEN,
};
use migo_protocol::{codes, ConversationKind, EncryptionMode, MessageKind, RoomKind};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::{
    media_scan, report_status, report_subject, AccountStatus, AuditActorKind, AuditEntry,
    AuditTargetKind, Conversation, MediaObject, NewAccount, NewBot, NewMessage, NewRoom, Report,
};
use migo_store::traits::{
    AccountStore, BotStore, MediaStore, MessagingStore, RoomStore, SafetyStore,
};
use migo_store::MemoryStore;

// --- Time and identity helpers.

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const NOW: i64 = 1_700_000_000 * SECOND;

/// Devices are derived from their owner so a caller helper needs only one number, and so
/// that no test accidentally shares one device between two accounts.
const DEVICE_OFFSET: u128 = 1_000_000;

// Ordinary accounts.
const ALICE: u128 = 1;
const BOB: u128 = 2;
const CAROL: u128 = 3;
const MALLORY: u128 = 4;
const STRANGER: u128 = 9;

// Staff accounts, each granted its powers by the test that needs them.
const TRIAGER: u128 = 10;
const AUDITOR: u128 = 11;
const SUSPENDER: u128 = 12;
const REMOVER: u128 = 13;
const ADMIN: u128 = 14;

// Non-account subjects.
const A_MESSAGE: u128 = 501;
const A_CONVERSATION: u128 = 502;
const A_ROOM: u128 = 503;
const A_MEDIUM: u128 = 504;
const A_BOT: u128 = 505;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

fn device_of(account: u128) -> Id {
    id(account + DEVICE_OFFSET)
}

/// A reporter at `NOW`, on an Established connection.
fn caller(account: u128) -> Caller {
    Caller::new(
        id(account),
        device_of(account),
        TrustTier::Established,
        ts(NOW),
    )
}

/// A member of staff at `NOW`. The `powers` here are a placeholder: the service resolves
/// the real ones from the [`StaffList`] on every call, so what matters is the account id.
fn operator(account: u128) -> Operator {
    Operator::new(id(account), device_of(account), Powers::NONE, ts(NOW))
}

/// An operator who has proved a factor recently, which every action requires.
fn fresh_operator(account: u128) -> Operator {
    operator(account).reauthenticated()
}

// --- The staff directory, as a port the service asks on every call.

/// A [`Roster`] backed by a map a test writes before it calls. Returning [`Powers::NONE`]
/// for an unknown account is the contract: an account nobody granted anything is not staff.
#[derive(Default)]
struct StaffList {
    grants: Mutex<HashMap<Id, Powers>>,
}

impl StaffList {
    fn grant(&self, account: u128, powers: Powers) {
        self.grants.lock().unwrap().insert(id(account), powers);
    }

    fn revoke(&self, account: u128) {
        self.grants.lock().unwrap().remove(&id(account));
    }
}

#[async_trait]
impl Roster for StaffList {
    async fn powers(&self, account_id: Id) -> Result<Powers> {
        Ok(self
            .grants
            .lock()
            .unwrap()
            .get(&account_id)
            .copied()
            .unwrap_or(Powers::NONE))
    }
}

type TestModeration = Moderation<MemoryStore, CacheRateLimiter<MemoryCache>, StaffList>;

/// Everything a test needs, with the real limiter over a real cache and the real store.
struct Harness {
    moderation: TestModeration,
    store: Arc<MemoryStore>,
    staff: Arc<StaffList>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::configured(ModerationConfig::default())
    }

    fn configured(config: ModerationConfig) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryStore::new());
        let staff = Arc::new(StaffList::default());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        let moderation = Moderation::new(
            Arc::clone(&store),
            limiter,
            Arc::clone(&staff),
            Box::new(OsRandom),
            config,
            &registry,
        );
        Self {
            moderation,
            store,
            staff,
            registry,
        }
    }

    /// An active account, which is what a suspend, warn, or reinstate needs to find.
    async fn account(&self, account: u128, username: &str) {
        self.store
            .create_account(NewAccount {
                account_id: id(account),
                username: username.to_string(),
                email: Some(format!("{username}@example.test")),
                phone: None,
                password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
                locale: "id-ID".to_string(),
                country: Some("ID".to_string()),
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh username is free");
    }

    async fn status_of(&self, account: u128) -> AccountStatus {
        self.store
            .account_by_id(id(account))
            .await
            .expect("the store can be read")
            .expect("the account exists")
            .status
    }

    /// A media object owned by `owner`, scanned clean, which a takedown can find.
    async fn medium(&self, media_id: u128, owner: u128) {
        self.store
            .create_media(MediaObject {
                media_id: id(media_id),
                owner_id: id(owner),
                kind: 0,
                mime: "image/webp".to_string(),
                byte_size: 4_096,
                width: Some(640),
                height: Some(480),
                duration_ms: None,
                storage_key: format!("m/{media_id}"),
                conversation_id: None,
                checksum: None,
                scan_status: media_scan::CLEAN,
                created_at: ts(SECOND),
                deleted_at: None,
            })
            .await
            .expect("a fresh media id is free");
    }

    async fn media_row(&self, media_id: u128) -> MediaObject {
        self.store
            .media(id(media_id))
            .await
            .expect("the store can be read")
            .expect("the media row exists")
    }

    /// A public room owned by `owner`, which an archive action can find.
    async fn room(&self, room_id: u128, owner: u128) {
        self.store
            .create_room(NewRoom {
                room_id: id(room_id),
                conversation_id: id(room_id + 900_000),
                slug: format!("room-{room_id}"),
                name: format!("Room {room_id}"),
                topic: None,
                kind: RoomKind::Public,
                owner_id: id(owner),
                home_region: "local".to_string(),
                max_members: 100,
                encryption: EncryptionMode::Transport,
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh room id and slug are free");
    }

    async fn room_row(&self, room_id: u128) -> migo_store::model::Room {
        self.store
            .room(id(room_id))
            .await
            .expect("the store can be read")
            .expect("the room exists")
    }

    /// A bot owned by `owner`, backed by its own account, which a disable action can find.
    async fn bot(&self, bot_id: u128, owner: u128, account: u128) {
        self.store
            .register_bot(NewBot {
                bot_id: id(bot_id),
                owner_id: id(owner),
                account_id: id(account),
                username: format!("bot{bot_id}"),
                display_name: format!("Bot {bot_id}"),
                password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
                token_hash: vec![bot_id as u8, 0xAB, 0xCD, (bot_id >> 8) as u8],
                scopes: 0,
                webhook_url: None,
                locale: "id-ID".to_string(),
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh bot, username, and token are free");
    }

    async fn bot_row(&self, bot_id: u128) -> migo_store::model::Bot {
        self.store
            .bot(id(bot_id))
            .await
            .expect("the store can be read")
            .expect("the bot exists")
    }

    /// A conversation carrying one message, which a message removal can find.
    async fn message(&self, conversation_id: u128, message_id: u128, sender: u128) {
        self.store
            .create_conversation(
                Conversation {
                    conversation_id: id(conversation_id),
                    kind: ConversationKind::Group,
                    encryption: EncryptionMode::Transport,
                    room_id: None,
                    last_seq: 0,
                    created_by: id(sender),
                    created_at: ts(SECOND),
                    last_message_at: None,
                    archived_at: None,
                    title: None,
                },
                vec![id(sender)],
            )
            .await
            .expect("a fresh conversation id is free");
        self.store
            .append_message(NewMessage {
                message_id: id(message_id),
                conversation_id: id(conversation_id),
                sender_id: id(sender),
                sender_device: Some(device_of(sender)),
                kind: MessageKind::Text,
                envelope: vec![1, 2, 3, 4],
                reply_to: None,
                expires_at: None,
                created_at: ts(SECOND),
            })
            .await
            .expect("the first message appends");
    }

    /// Writes an open report straight to the store, so the queue and resolve tests do not
    /// have to spend a reporter's whole budget seeding one.
    async fn seed_report(&self, report_id: u128, reporter: u128, subject: u128, created: i64) {
        self.store
            .create_report(Report {
                report_id: id(report_id),
                reporter_id: id(reporter),
                subject_kind: report_subject::USER,
                subject_id: id(subject),
                room_id: None,
                reason: Reason::Spam.to_i16(),
                note: None,
                evidence_ref: None,
                status: report_status::OPEN,
                created_at: ts(created),
                resolved_at: None,
                resolved_by: None,
                resolution: None,
            })
            .await
            .expect("a fresh report id is free");
    }

    /// The stored row, by the id the service minted, so a test can look past the `Case`
    /// the API returns at the columns that were actually written.
    async fn report_row(&self, report_id: Id) -> Report {
        self.store
            .report(report_id)
            .await
            .expect("the store can be read")
            .expect("the report exists")
    }

    async fn audit_for(&self, kind: AuditTargetKind, target: u128) -> Vec<AuditEntry> {
        self.audit_of(kind, id(target)).await
    }

    /// The same read for a target whose id the service minted rather than the test.
    async fn audit_of(&self, kind: AuditTargetKind, target: Id) -> Vec<AuditEntry> {
        self.store
            .audit_for_target(kind.to_i16(), target, 200)
            .await
            .expect("the audit trail can be read")
    }

    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn plain(&self, name: &'static str) -> u64 {
        self.counter(name, &[])
    }

    fn refusals(&self, reason: &'static str) -> u64 {
        self.counter("migo_moderation_refusals_total", &[("reason", reason)])
    }

    fn filed(&self, subject: &'static str) -> u64 {
        self.counter(
            "migo_moderation_reports_filed_total",
            &[("subject", subject)],
        )
    }

    fn by_reason(&self, reason: &'static str) -> u64 {
        self.counter(
            "migo_moderation_reports_by_reason_total",
            &[("reason", reason)],
        )
    }

    fn resolved(&self, resolution: &'static str) -> u64 {
        self.counter(
            "migo_moderation_reports_resolved_total",
            &[("resolution", resolution)],
        )
    }

    fn actions(&self, action: &'static str) -> u64 {
        self.counter("migo_moderation_actions_total", &[("action", action)])
    }

    fn assessments(&self, risk: &'static str) -> u64 {
        self.counter("migo_moderation_assessments_total", &[("risk", risk)])
    }
}

#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    let error = result.err().expect("this call must be refused");
    assert_eq!(
        error.code(),
        code,
        "expected code {code}, got {}: {error}",
        error.code()
    );
}

// ---------------------------------------------------------------------------
// Identity, and the order of the gate.
//
// Every method here is reached from the network, so the first question is not "may this
// caller do this" but "who is this caller at all". A report from a session with no
// account or no device is a request from nobody, and it must be refused before the rate
// limiter is consulted: a limiter charged before identity is proved is a limiter an
// attacker uses to drain a stranger's budget by naming them, or to make a malformed
// request cost its own sender nothing while the queue fills. These tests pin the code
// (`UNAUTHENTICATED`) and the ordering (nothing is spent, nothing is filed).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filing_a_report_with_a_nil_account_is_unauthenticated() {
    let h = Harness::new();
    let caller = Caller::new(Id::NIL, device_of(ALICE), TrustTier::Established, ts(NOW));
    expect_code(
        h.moderation
            .file_report(&caller, Filing::new(Subject::User(id(BOB)), Reason::Spam))
            .await,
        codes::UNAUTHENTICATED,
    );
    assert_eq!(
        h.filed("user"),
        0,
        "an unauthenticated report is never filed"
    );
}

#[tokio::test]
async fn filing_a_report_with_a_nil_device_is_unauthenticated() {
    let h = Harness::new();
    let caller = Caller::new(id(ALICE), Id::NIL, TrustTier::Established, ts(NOW));
    expect_code(
        h.moderation
            .file_report(&caller, Filing::new(Subject::User(id(BOB)), Reason::Spam))
            .await,
        codes::UNAUTHENTICATED,
    );
    assert_eq!(
        h.filed("user"),
        0,
        "a report from an unidentified device is never filed"
    );
}

#[tokio::test]
async fn an_unidentified_report_is_never_rate_limited_first() {
    // The refusal is an identity refusal, counted as invalid, and the limiter is never
    // reached: a session that cannot be identified cannot have a bucket to charge.
    let h = Harness::new();
    let caller = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    expect_code(
        h.moderation
            .file_report(&caller, Filing::new(Subject::User(id(BOB)), Reason::Spam))
            .await,
        codes::UNAUTHENTICATED,
    );
    assert_eq!(
        h.refusals("rate_limited"),
        0,
        "identity is checked before the limiter"
    );
    assert_eq!(h.refusals("invalid"), 1);
}

#[tokio::test]
async fn a_malformed_report_never_costs_the_reporter_its_budget() {
    // Every validation in file_report runs before the charge, so a client that sends
    // fifteen self-reports has spent nothing and can still file its ten real ones. Were
    // the order reversed, the budget of two hundred would be gone after ten refusals.
    let h = Harness::new();
    for _ in 0..15 {
        expect_code(
            h.moderation
                .file_report(
                    &caller(ALICE),
                    Filing::new(Subject::User(id(ALICE)), Reason::Spam),
                )
                .await,
            codes::VALIDATION_FAILED,
        );
    }
    for nth in 0..10 {
        let filed = h
            .moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB + nth)), Reason::Spam),
            )
            .await
            .expect("a real report still fits, because the malformed ones cost nothing");
        assert!(!filed.duplicate);
    }
}

#[tokio::test]
async fn assessing_a_nil_account_is_refused() {
    let h = Harness::new();
    expect_code(
        h.moderation
            .assess(Id::NIL, Signals::default(), ts(NOW))
            .await,
        codes::VALIDATION_FAILED,
    );
}

// ---------------------------------------------------------------------------
// Authorization, resolved on every call and never trusted from the caller.
//
// There is no role column in the schema, so who is staff is a question the service asks
// the deployment's Roster and answers with `Powers::NONE` when the deployment says
// nothing. The rules these tests hold: a stranger and a member of staff without the right
// power are refused identically; the powers a caller puts on the Operator struct are
// thrown away and re-resolved, so a forged grant buys nothing and a revoked grant bites
// on the very next call; and for the one read where the existence of the object is itself
// the secret, the refusal is `NOT_FOUND`, not `PERMISSION_DENIED`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stranger_cannot_read_the_queue() {
    let h = Harness::new();
    expect_code(
        h.moderation.queue(&operator(STRANGER), None).await,
        codes::PERMISSION_DENIED,
    );
}

#[tokio::test]
async fn a_triager_can_read_the_queue() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    let queue = h
        .moderation
        .queue(&operator(TRIAGER), None)
        .await
        .expect("triage powers open the queue");
    assert!(queue.is_empty());
}

#[tokio::test]
async fn a_non_moderator_is_refused_exactly_like_a_stranger() {
    // An account the roster knows but granted nothing, and an account it has never heard
    // of, are the same account as far as the queue is concerned.
    let h = Harness::new();
    h.staff.grant(BOB, Powers::NONE);
    let known = h.moderation.queue(&operator(BOB), None).await;
    let unknown = h.moderation.queue(&operator(STRANGER), None).await;
    expect_code(known, codes::PERMISSION_DENIED);
    expect_code(unknown, codes::PERMISSION_DENIED);
}

#[tokio::test]
async fn reading_a_report_hides_its_existence_from_a_stranger() {
    // NOT_FOUND, not PERMISSION_DENIED: a caller who may not triage must not be able to
    // learn that a report about somebody exists by watching which code comes back.
    let h = Harness::new();
    h.seed_report(700, ALICE, BOB, NOW).await;
    expect_code(
        h.moderation.report(&operator(STRANGER), id(700)).await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn a_triager_reading_a_missing_report_gets_the_same_not_found() {
    // The other half of the disguise: a real triager asking for a report that is not there
    // gets exactly what the stranger got, so the two cases are indistinguishable.
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    expect_code(
        h.moderation.report(&operator(TRIAGER), id(700)).await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn a_triager_reads_a_real_report() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(700, ALICE, BOB, NOW).await;
    let case = h
        .moderation
        .report(&operator(TRIAGER), id(700))
        .await
        .expect("a triager may read a report that exists");
    assert_eq!(case.report_id, id(700));
    assert_eq!(case.reporter_id, id(ALICE));
    assert_eq!(case.subject_id, id(BOB));
    assert!(case.is_open());
}

#[tokio::test]
async fn suspending_needs_the_suspend_power() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    expect_code(
        h.moderation
            .act(
                &fresh_operator(TRIAGER),
                Action::Suspend {
                    account_id: id(BOB),
                    until: None,
                },
                None,
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    h.moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Suspend {
                account_id: id(BOB),
                until: None,
            },
            None,
        )
        .await
        .expect("the suspend power suspends");
    assert_eq!(h.status_of(BOB).await, AccountStatus::Suspended);
}

#[tokio::test]
async fn warning_rides_with_the_triage_power() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.moderation
        .act(
            &fresh_operator(TRIAGER),
            Action::Warn {
                account_id: id(BOB),
            },
            None,
        )
        .await
        .expect("a warning is a triage-level action");
}

#[tokio::test]
async fn a_takedown_needs_the_takedown_power() {
    let h = Harness::new();
    h.medium(A_MEDIUM, BOB).await;
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    expect_code(
        h.moderation
            .act(
                &fresh_operator(TRIAGER),
                Action::RemoveMedia {
                    media_id: id(A_MEDIUM),
                },
                None,
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    h.moderation
        .act(
            &fresh_operator(REMOVER),
            Action::RemoveMedia {
                media_id: id(A_MEDIUM),
            },
            None,
        )
        .await
        .expect("the takedown power removes media");
}

#[tokio::test]
async fn reading_the_audit_trail_needs_the_audit_power() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.staff.grant(AUDITOR, Powers::AUDIT);
    expect_code(
        h.moderation
            .audit(&operator(TRIAGER), AuditTargetKind::Account, id(BOB), None)
            .await,
        codes::PERMISSION_DENIED,
    );
    h.moderation
        .audit(&operator(AUDITOR), AuditTargetKind::Account, id(BOB), None)
        .await
        .expect("the audit power reads the trail");
}

#[tokio::test]
async fn an_auditor_can_read_but_cannot_act_or_triage() {
    // The audit power is the one worth giving to somebody who may not act: a reviewer of
    // what moderators did should not thereby be able to do it.
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(AUDITOR, Powers::AUDIT);
    expect_code(
        h.moderation.queue(&operator(AUDITOR), None).await,
        codes::PERMISSION_DENIED,
    );
    expect_code(
        h.moderation
            .act(
                &fresh_operator(AUDITOR),
                Action::Warn {
                    account_id: id(BOB),
                },
                None,
            )
            .await,
        codes::PERMISSION_DENIED,
    );
}

#[tokio::test]
async fn the_powers_on_the_operator_struct_are_ignored() {
    // The caller claims every power; the roster has never heard of them. The service
    // resolves the roster's answer, so the claim buys nothing.
    let h = Harness::new();
    let forged = Operator::new(id(STRANGER), device_of(STRANGER), Powers::ALL, ts(NOW));
    expect_code(
        h.moderation.queue(&forged, None).await,
        codes::PERMISSION_DENIED,
    );
}

#[tokio::test]
async fn powers_are_resolved_fresh_on_every_call() {
    // Granted, the queue opens; revoked, the very next call is refused. This is what makes
    // revocation take effect at once, which is the direction that matters.
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.moderation
        .queue(&operator(TRIAGER), None)
        .await
        .expect("granted");
    h.staff.revoke(TRIAGER);
    expect_code(
        h.moderation.queue(&operator(TRIAGER), None).await,
        codes::PERMISSION_DENIED,
    );
}

// ---------------------------------------------------------------------------
// Re-authentication, required for every action and for no read.
//
// A stolen operator session is the worst credential in the system: it suspends accounts
// and deletes other people's content. So every action demands a recently proved factor,
// exactly as the brief demands one before a room changes hands. Two orderings matter and
// are tested here. The freshness check runs after the power check, so a stranger is told
// "denied" and never learns the freshness rule exists. And it runs before the charge, so
// replaying a stale session cannot drain a real moderator's budget.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolving_without_a_fresh_factor_is_refused() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(700, ALICE, BOB, NOW).await;
    expect_code(
        h.moderation
            .resolve(&operator(TRIAGER), id(700), Resolution::NoAction, None)
            .await,
        codes::REAUTHENTICATION_REQUIRED,
    );
}

#[tokio::test]
async fn acting_without_a_fresh_factor_is_refused() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    expect_code(
        h.moderation
            .act(
                &operator(SUSPENDER),
                Action::Suspend {
                    account_id: id(BOB),
                    until: None,
                },
                None,
            )
            .await,
        codes::REAUTHENTICATION_REQUIRED,
    );
}

#[tokio::test]
async fn reads_do_not_need_a_fresh_factor() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.staff.grant(AUDITOR, Powers::AUDIT);
    h.seed_report(700, ALICE, BOB, NOW).await;
    h.moderation
        .queue(&operator(TRIAGER), None)
        .await
        .expect("a stale session may still read the queue");
    h.moderation
        .report(&operator(TRIAGER), id(700))
        .await
        .expect("and read a report");
    h.moderation
        .audit(&operator(AUDITOR), AuditTargetKind::Account, id(BOB), None)
        .await
        .expect("and read the audit trail");
}

#[tokio::test]
async fn the_freshness_rule_is_invisible_to_a_stranger() {
    // A stranger with a stale session gets PERMISSION_DENIED, not REAUTHENTICATION_REQUIRED:
    // the power check comes first, so somebody who is not staff never learns the rule is
    // there to be probed.
    let h = Harness::new();
    h.seed_report(700, ALICE, BOB, NOW).await;
    expect_code(
        h.moderation
            .resolve(&operator(STRANGER), id(700), Resolution::NoAction, None)
            .await,
        codes::PERMISSION_DENIED,
    );
}

#[tokio::test]
async fn a_stale_action_costs_no_budget() {
    // Thirty stale suspensions, each refused before the charge; then one fresh suspension
    // that succeeds. If a stale action were charged its ten, the budget of two hundred
    // would be gone after twenty and the real action would be rate limited instead.
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    for _ in 0..30 {
        expect_code(
            h.moderation
                .act(
                    &operator(SUSPENDER),
                    Action::Suspend {
                        account_id: id(BOB),
                        until: None,
                    },
                    None,
                )
                .await,
            codes::REAUTHENTICATION_REQUIRED,
        );
    }
    h.moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Suspend {
                account_id: id(BOB),
                until: None,
            },
            None,
        )
        .await
        .expect("a fresh action still has its full budget");
}

// ---------------------------------------------------------------------------
// Validation ceilings, checked at the edge and one step past it.
//
// A ceiling that is only approximately enforced is a ceiling an attacker rounds up: a
// note field that accepts "about five hundred" characters accepts a megabyte from
// somebody who tries. So each limit is tested at the exact value that must pass and at the
// first value that must not, with the specific refusal code the field promises. A field
// that is too long is FIELD_TOO_LONG and not a generic validation failure, because a
// client that cannot tell "you sent too much" from "you sent the wrong thing" retries the
// same oversized payload forever. Two orderings are pinned here too: the reason a
// moderator types is vetted before their powers are, so a malformed action is a malformed
// action whoever sends it; and the queue page size is a clamp, not a refusal, because a
// dashboard asking for more rows than allowed wants the maximum, not an error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filing_a_report_about_nothing_is_refused() {
    let h = Harness::new();
    expect_code(
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(Id::NIL), Reason::Spam),
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(h.refusals("invalid"), 1);
    assert_eq!(h.filed("user"), 0);
}

#[tokio::test]
async fn reporting_yourself_is_refused() {
    let h = Harness::new();
    expect_code(
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(ALICE)), Reason::Harassment),
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(h.filed("user"), 0);
}

#[tokio::test]
async fn a_note_at_the_ceiling_is_accepted() {
    let h = Harness::new();
    let note = "a".repeat(MAX_NOTE_LEN);
    let filed = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam).with_note(note),
        )
        .await
        .expect("a note of exactly the maximum length is allowed");
    assert!(!filed.duplicate);
    assert_eq!(h.filed("user"), 1);
}

#[tokio::test]
async fn a_note_one_character_past_the_ceiling_is_refused() {
    let h = Harness::new();
    let note = "a".repeat(MAX_NOTE_LEN + 1);
    expect_code(
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB)), Reason::Spam).with_note(note),
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
    assert_eq!(h.refusals("invalid"), 1);
    assert_eq!(h.filed("user"), 0);
}

#[tokio::test]
async fn an_empty_note_is_treated_as_too_long_not_as_absent() {
    // A quirk worth pinning rather than hiding: a note of "" is not the same as no note.
    // `with_note("")` fails the usability check and comes back as FIELD_TOO_LONG, so a
    // client that means "no note" must send `None`, not an empty string.
    let h = Harness::new();
    expect_code(
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB)), Reason::Spam).with_note(""),
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
}

#[tokio::test]
async fn a_whitespace_only_note_is_refused() {
    let h = Harness::new();
    expect_code(
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB)), Reason::Spam).with_note("    \t  "),
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
}

#[tokio::test]
async fn a_note_is_trimmed_before_it_is_stored() {
    let h = Harness::new();
    let filed = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam).with_note("  spammy link  "),
        )
        .await
        .expect("a note with surrounding space is fine");
    let stored = h
        .store
        .report(filed.report_id)
        .await
        .expect("readable")
        .expect("the report exists");
    assert_eq!(stored.note.as_deref(), Some("spammy link"));
}

#[tokio::test]
async fn resolving_with_a_reason_at_the_ceiling_is_accepted() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(800, ALICE, BOB, NOW).await;
    let reason = "r".repeat(MAX_REASON_LEN);
    h.moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(800),
            Resolution::NoAction,
            Some(&reason),
        )
        .await
        .expect("a reason of exactly the maximum length is allowed");
}

#[tokio::test]
async fn resolving_with_a_reason_past_the_ceiling_is_refused() {
    // vet_reason runs before the power check, so the malformed reason is caught for a real
    // triager exactly as it would be for anybody: the code is about the input, not the
    // caller.
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(800, ALICE, BOB, NOW).await;
    let reason = "r".repeat(MAX_REASON_LEN + 1);
    expect_code(
        h.moderation
            .resolve(
                &fresh_operator(TRIAGER),
                id(800),
                Resolution::NoAction,
                Some(&reason),
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
}

#[tokio::test]
async fn acting_with_a_reason_past_the_ceiling_is_refused_before_anything_else() {
    // No account is created and no power is granted, and the answer is still FIELD_TOO_LONG
    // rather than NOT_FOUND or PERMISSION_DENIED: the reason is vetted first of all.
    let h = Harness::new();
    let reason = "r".repeat(MAX_REASON_LEN + 1);
    expect_code(
        h.moderation
            .act(
                &fresh_operator(TRIAGER),
                Action::Warn {
                    account_id: id(BOB),
                },
                Some(&reason),
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
}

#[tokio::test]
async fn auditing_a_nil_target_is_refused() {
    // The nil-target check comes before the power check, so this is VALIDATION_FAILED even
    // for somebody holding the audit power.
    let h = Harness::new();
    h.staff.grant(AUDITOR, Powers::AUDIT);
    expect_code(
        h.moderation
            .audit(&operator(AUDITOR), AuditTargetKind::Account, Id::NIL, None)
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(h.refusals("invalid"), 1);
}

#[tokio::test]
async fn the_queue_page_size_is_clamped_up_to_the_maximum() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    for n in 0..(MAX_PAGE as u128 + 1) {
        h.seed_report(2_000 + n, ALICE, BOB + n, NOW).await;
    }
    let page = h
        .moderation
        .queue(&operator(TRIAGER), Some(10_000))
        .await
        .expect("an oversized limit is clamped, not refused");
    assert_eq!(
        page.len(),
        MAX_PAGE as usize,
        "the page is capped at the maximum"
    );
}

#[tokio::test]
async fn the_queue_page_size_is_clamped_up_from_zero() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    for n in 0..5 {
        h.seed_report(2_100 + n, ALICE, BOB + n, NOW).await;
    }
    let page = h
        .moderation
        .queue(&operator(TRIAGER), Some(0))
        .await
        .expect("a zero limit is clamped to one, not refused");
    assert_eq!(page.len(), 1, "a limit of zero still returns a row");
}

// ---------------------------------------------------------------------------
// The rate-limit budget, as arithmetic rather than as a feeling.
//
// The limiter here is the real one over a real cache, and every call in a test shares one
// timestamp, so the bucket never refills mid-test and the numbers are exact. An
// Established account's burst is two hundred tokens. A report costs twenty, so a reporter
// gets exactly ten before the eleventh is refused; an action costs ten, so a moderator
// gets exactly twenty. That two-to-one ratio is deliberate — the report price is the only
// thing between a script and a full queue, and it is charged to the account that would run
// the script. The refusal is counted, and the refused write never lands: a report that was
// rate limited is not a report sitting in the queue.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_reporter_gets_exactly_ten_reports_before_the_budget_runs_out() {
    let h = Harness::new();
    for n in 0..10 {
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB + n)), Reason::Spam),
            )
            .await
            .expect("the first ten reports fit inside the burst of two hundred");
    }
    expect_code(
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB + 100)), Reason::Spam),
            )
            .await,
        codes::RATE_LIMITED,
    );
    assert_eq!(h.refusals("rate_limited"), 1, "the refusal is counted");
    assert_eq!(h.filed("user"), 10, "and only the ten that fit were filed");
}

#[tokio::test]
async fn a_rate_limited_report_never_reaches_the_queue() {
    let h = Harness::new();
    for n in 0..10 {
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB + n)), Reason::Spam),
            )
            .await
            .expect("fits");
    }
    expect_code(
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB + 100)), Reason::Spam),
            )
            .await,
        codes::RATE_LIMITED,
    );
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    let queue = h
        .moderation
        .queue(&operator(TRIAGER), Some(200))
        .await
        .expect("readable");
    assert_eq!(queue.len(), 10, "the refused report is not in the queue");
    assert!(
        h.store
            .open_report_by_reporter(id(ALICE), report_subject::USER, id(BOB + 100))
            .await
            .expect("readable")
            .is_none(),
        "and no row was written for it"
    );
}

#[tokio::test]
async fn the_reporter_budget_is_charged_to_the_account_not_the_device() {
    // Ten reports from one device exhaust the account, and an eleventh from a second device
    // of the same account is refused: the limit is per person, so a second tab does not buy
    // a second budget.
    let h = Harness::new();
    for n in 0..10 {
        h.moderation
            .file_report(
                &caller(ALICE),
                Filing::new(Subject::User(id(BOB + n)), Reason::Spam),
            )
            .await
            .expect("fits");
    }
    let second_device = Caller::new(
        id(ALICE),
        device_of(ALICE + DEVICE_OFFSET),
        TrustTier::Established,
        ts(NOW),
    );
    expect_code(
        h.moderation
            .file_report(
                &second_device,
                Filing::new(Subject::User(id(BOB + 100)), Reason::Spam),
            )
            .await,
        codes::RATE_LIMITED,
    );
}

#[tokio::test]
async fn an_operator_gets_exactly_twenty_actions_before_the_budget_runs_out() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    // A warning changes nothing but the record, so it can be repeated against one account.
    for _ in 0..20 {
        h.moderation
            .act(
                &fresh_operator(TRIAGER),
                Action::Warn {
                    account_id: id(BOB),
                },
                None,
            )
            .await
            .expect("the first twenty actions fit inside the burst of two hundred");
    }
    expect_code(
        h.moderation
            .act(
                &fresh_operator(TRIAGER),
                Action::Warn {
                    account_id: id(BOB),
                },
                None,
            )
            .await,
        codes::RATE_LIMITED,
    );
    assert_eq!(h.refusals("rate_limited"), 1);
    assert_eq!(
        h.actions("warn"),
        20,
        "and only the twenty that fit were taken"
    );
}

#[tokio::test]
async fn the_operator_budget_is_charged_before_the_action_is_applied() {
    // Twenty suspensions of a missing account each pay their ten and then fail to find the
    // account; the twenty-first is refused by the limiter before it can even look. So the
    // charge precedes the work, which is what stops a failing action from being free to
    // retry.
    let h = Harness::new();
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    for _ in 0..20 {
        expect_code(
            h.moderation
                .act(
                    &fresh_operator(SUSPENDER),
                    Action::Suspend {
                        account_id: id(BOB),
                        until: None,
                    },
                    None,
                )
                .await,
            codes::NOT_FOUND,
        );
    }
    expect_code(
        h.moderation
            .act(
                &fresh_operator(SUSPENDER),
                Action::Suspend {
                    account_id: id(BOB),
                    until: None,
                },
                None,
            )
            .await,
        codes::RATE_LIMITED,
    );
}

// ---------------------------------------------------------------------------
// Idempotence, because the client that files twice is usually the client whose first
// answer was lost.
//
// Brief section 153 makes a repeat filing an outcome, not an error: a phone that sent a
// report, dropped the reply, and sent it again has a report sitting in the queue, and
// telling it the second attempt failed would be a lie about a real report. So a duplicate
// comes back as success, carrying the id of the report that is already there, and it
// writes nothing new — no second row, no second audit entry, no second count against the
// subject. The dedup is deliberately scoped to reports that are still open: a subject
// reported, actioned, and misbehaving again is a fresh problem, not an echo of a closed
// one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filing_the_same_report_twice_returns_the_first_and_writes_nothing_new() {
    let h = Harness::new();
    let first = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("the first filing is written");
    assert!(!first.duplicate);
    let second = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("the second filing is an outcome, not an error");
    assert!(second.duplicate, "the repeat is reported as a duplicate");
    assert_eq!(
        second.report_id, first.report_id,
        "and names the report already filed"
    );
    assert_eq!(h.filed("user"), 1, "only one report was ever filed");
    assert_eq!(h.plain("migo_moderation_reports_duplicate_total"), 1);
}

#[tokio::test]
async fn a_duplicate_writes_no_second_audit_entry() {
    let h = Harness::new();
    let first = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("filed");
    h.moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("duplicate");
    let trail = h
        .store
        .audit_for_target(AuditTargetKind::Report.to_i16(), first.report_id, 200)
        .await
        .expect("readable");
    assert_eq!(
        trail.len(),
        1,
        "the create is recorded once and the duplicate not at all"
    );
}

#[tokio::test]
async fn a_duplicate_is_not_counted_against_the_subject_a_second_time() {
    let h = Harness::new();
    h.moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("filed");
    h.moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("duplicate");
    assert_eq!(
        h.by_reason("spam"),
        1,
        "the reason is counted once, not twice"
    );
}

#[tokio::test]
async fn two_reporters_about_one_subject_are_two_reports() {
    // The dedup is per reporter, so two people reporting the same account is two reports,
    // not one with a duplicate: each person's report is their own.
    let h = Harness::new();
    let a = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("filed");
    let c = h
        .moderation
        .file_report(
            &caller(CAROL),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("filed");
    assert!(!a.duplicate && !c.duplicate);
    assert_ne!(a.report_id, c.report_id);
    assert_eq!(h.filed("user"), 2);
}

#[tokio::test]
async fn a_report_filed_after_the_first_is_resolved_is_not_a_duplicate() {
    // The dedup is scoped to open reports. Once the first is closed, the same reporter
    // reporting the same subject again is a new problem, and a new row.
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    let first = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("filed");
    h.moderation
        .resolve(
            &fresh_operator(TRIAGER),
            first.report_id,
            Resolution::NoAction,
            None,
        )
        .await
        .expect("closed");
    let again = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("filed anew");
    assert!(
        !again.duplicate,
        "a closed report does not shadow a new one"
    );
    assert_ne!(again.report_id, first.report_id);
    assert_eq!(h.filed("user"), 2);
}

// ---------------------------------------------------------------------------
// Resolving a report, and the one resolution that does not close it.
//
// Closing a report writes the outcome, the closer, and the time onto the row, and moves
// its status to ACTIONED or DISMISSED depending on whether anything was done. The
// exception is an escalation: it leaves the report open on purpose, because an escalation
// that closed the report would erase the one record that somebody is still waiting for an
// answer. A report can only be closed once — a second attempt is a CONFLICT, not a silent
// re-close — because two moderators resolving the same report from two stale queue views
// must not each believe they were the one who handled it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolving_an_open_report_closes_it_and_records_the_closer() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(900, ALICE, BOB, NOW).await;
    let case = h
        .moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(900),
            Resolution::NoAction,
            None,
        )
        .await
        .expect("an open report can be resolved");
    assert!(!case.is_open());
    assert_eq!(case.status, report_status::DISMISSED);
    assert_eq!(case.resolution, Some(Resolution::NoAction));
    assert_eq!(case.resolved_by, Some(id(TRIAGER)));
    assert!(case.resolved_at.is_some());
    assert_eq!(h.resolved("no_action"), 1);
}

#[tokio::test]
async fn resolving_with_an_action_taken_marks_the_report_actioned() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(901, ALICE, BOB, NOW).await;
    let case = h
        .moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(901),
            Resolution::ContentRemoved,
            None,
        )
        .await
        .expect("resolved");
    assert_eq!(case.status, report_status::ACTIONED);
    assert_eq!(case.resolution, Some(Resolution::ContentRemoved));
    assert_eq!(h.resolved("content_removed"), 1);
}

#[tokio::test]
async fn escalating_a_report_leaves_it_open() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(902, ALICE, BOB, NOW).await;
    let case = h
        .moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(902),
            Resolution::Escalated,
            None,
        )
        .await
        .expect("resolved");
    assert!(case.is_open(), "an escalation does not close the report");
    assert_eq!(case.status, report_status::OPEN);
    assert_eq!(case.resolution, None, "and writes no resolution to the row");
    assert_eq!(
        h.resolved("escalated"),
        1,
        "though it is counted as a resolution action"
    );
    let trail = h
        .store
        .audit_for_target(AuditTargetKind::Report.to_i16(), id(902), 200)
        .await
        .expect("readable");
    assert_eq!(
        trail.len(),
        1,
        "the escalation is in the audit trail even so"
    );
}

#[tokio::test]
async fn an_escalated_report_can_still_be_resolved_afterwards() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(903, ALICE, BOB, NOW).await;
    h.moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(903),
            Resolution::Escalated,
            None,
        )
        .await
        .expect("escalated, still open");
    let closed = h
        .moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(903),
            Resolution::Suspended,
            None,
        )
        .await
        .expect("and now closed by whoever it was escalated to");
    assert!(!closed.is_open());
    assert_eq!(closed.resolution, Some(Resolution::Suspended));
}

#[tokio::test]
async fn resolving_a_missing_report_is_not_found() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    expect_code(
        h.moderation
            .resolve(
                &fresh_operator(TRIAGER),
                id(999),
                Resolution::NoAction,
                None,
            )
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(h.refusals("missing"), 1);
}

#[tokio::test]
async fn resolving_a_report_a_second_time_conflicts() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(904, ALICE, BOB, NOW).await;
    h.moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(904),
            Resolution::NoAction,
            None,
        )
        .await
        .expect("closed once");
    expect_code(
        h.moderation
            .resolve(&fresh_operator(TRIAGER), id(904), Resolution::Warned, None)
            .await,
        codes::CONFLICT,
    );
    assert_eq!(h.refusals("already_resolved"), 1);
}

// ---------------------------------------------------------------------------
// Actions, and what each one lands on.
//
// An action changes one thing and records that it changed it, in that order, so nothing
// downstream ever sees the change without the record. The account actions move a status:
// suspend sets it, reinstate clears both the status and the expiry so no stale date is
// left behind, and a warning moves nothing at all — it is only an audit row, because the
// audit trail *is* the warning history. The takedowns work on ids because the content is
// opaque: a media removal marks the object rejected and then tombstones it, both writes,
// so the scan column a download check reads can never say "clean" for a gone object. Every
// action refuses a target that is not there with NOT_FOUND rather than a silent no-op,
// because an audit row claiming somebody was suspended who never existed is worse than an
// error. And no operator may act on their own account — the one exception being that a
// takedown is never a self-action, since it names content and not a person.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn suspending_an_account_sets_its_status_and_notifies() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    let notice = h
        .moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Suspend {
                account_id: id(BOB),
                until: None,
            },
            None,
        )
        .await
        .expect("suspended")
        .expect("a suspension notifies the account");
    assert_eq!(h.status_of(BOB).await, AccountStatus::Suspended);
    assert_eq!(notice.audience, id(BOB));
    assert_eq!(notice.outcome, Outcome::Suspended { until: None });
    assert_eq!(h.actions("suspend"), 1);
}

#[tokio::test]
async fn a_timed_suspension_carries_its_expiry_on_the_notice() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    let until = ts(NOW + 7 * DAY);
    let notice = h
        .moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Suspend {
                account_id: id(BOB),
                until: Some(until),
            },
            None,
        )
        .await
        .expect("suspended")
        .expect("notice");
    assert_eq!(notice.outcome, Outcome::Suspended { until: Some(until) });
    assert_eq!(h.status_of(BOB).await, AccountStatus::Suspended);
}

#[tokio::test]
async fn reinstating_returns_an_account_to_active() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    h.moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Suspend {
                account_id: id(BOB),
                until: Some(ts(NOW + DAY)),
            },
            None,
        )
        .await
        .expect("suspended");
    let notice = h
        .moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Reinstate {
                account_id: id(BOB),
            },
            Some("appeal upheld"),
        )
        .await
        .expect("reinstated")
        .expect("notice");
    assert_eq!(h.status_of(BOB).await, AccountStatus::Active);
    assert_eq!(notice.outcome, Outcome::Reinstated);
    assert_eq!(
        notice.reason, None,
        "a reinstatement carries no accusing reason"
    );
}

#[tokio::test]
async fn warning_moves_no_status_and_writes_one_audit_row() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.moderation
        .act(
            &fresh_operator(TRIAGER),
            Action::Warn {
                account_id: id(BOB),
            },
            None,
        )
        .await
        .expect("warned");
    assert_eq!(
        h.status_of(BOB).await,
        AccountStatus::Active,
        "a warning changes no status"
    );
    let trail = h.audit_for(AuditTargetKind::Account, BOB).await;
    assert_eq!(trail.len(), 1);
    assert_eq!(trail[0].action, "moderation.account.warn");
}

#[tokio::test]
async fn removing_media_rejects_it_and_then_tombstones_it() {
    // Both writes, and in that order: the scan column is what a download check reads, so
    // marking it rejected before tombstoning means a crash between the two leaves the
    // object unservable rather than servable-and-gone.
    let h = Harness::new();
    h.medium(A_MEDIUM, BOB).await;
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    let notice = h
        .moderation
        .act(
            &fresh_operator(REMOVER),
            Action::RemoveMedia {
                media_id: id(A_MEDIUM),
            },
            None,
        )
        .await
        .expect("removed");
    assert!(notice.is_none(), "a takedown does not notify");
    let row = h.media_row(A_MEDIUM).await;
    assert_eq!(row.scan_status, media_scan::REJECTED, "marked rejected");
    assert!(row.deleted_at.is_some(), "and tombstoned");
    assert_eq!(h.actions("remove_media"), 1);
}

#[tokio::test]
async fn archiving_a_room_stamps_its_archived_time() {
    let h = Harness::new();
    h.room(A_ROOM, BOB).await;
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    h.moderation
        .act(
            &fresh_operator(REMOVER),
            Action::ArchiveRoom {
                room_id: id(A_ROOM),
            },
            None,
        )
        .await
        .expect("archived");
    assert!(h.room_row(A_ROOM).await.archived_at.is_some());
    assert_eq!(h.actions("archive_room"), 1);
}

#[tokio::test]
async fn disabling_a_bot_stamps_it_and_leaves_its_account_standing() {
    let h = Harness::new();
    h.bot(A_BOT, BOB, 700).await;
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    h.moderation
        .act(
            &fresh_operator(REMOVER),
            Action::DisableBot { bot_id: id(A_BOT) },
            None,
        )
        .await
        .expect("disabled");
    assert!(
        h.bot_row(A_BOT).await.disabled_at.is_some(),
        "the bot is disabled"
    );
    assert!(
        h.store
            .account_by_id(id(700))
            .await
            .expect("readable")
            .is_some(),
        "and its backing account survives, so it can be re-enabled"
    );
}

#[tokio::test]
async fn removing_a_message_is_accepted_and_does_not_notify() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.message(A_CONVERSATION, A_MESSAGE, BOB).await;
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    let notice = h
        .moderation
        .act(
            &fresh_operator(REMOVER),
            Action::RemoveMessage {
                conversation_id: id(A_CONVERSATION),
                message_id: id(A_MESSAGE),
            },
            None,
        )
        .await
        .expect("removed");
    assert!(notice.is_none());
    assert_eq!(h.actions("remove_message"), 1);
}

#[tokio::test]
async fn every_takedown_on_a_missing_target_is_not_found() {
    let h = Harness::new();
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    for action in [
        Action::RemoveMessage {
            conversation_id: id(A_CONVERSATION),
            message_id: id(A_MESSAGE),
        },
        Action::RemoveMedia {
            media_id: id(A_MEDIUM),
        },
        Action::ArchiveRoom {
            room_id: id(A_ROOM),
        },
        Action::DisableBot { bot_id: id(A_BOT) },
    ] {
        expect_code(
            h.moderation
                .act(&fresh_operator(REMOVER), action, None)
                .await,
            codes::NOT_FOUND,
        );
    }
    assert_eq!(h.refusals("missing"), 4, "each missing target is counted");
}

#[tokio::test]
async fn suspending_a_missing_account_is_not_found() {
    let h = Harness::new();
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    expect_code(
        h.moderation
            .act(
                &fresh_operator(SUSPENDER),
                Action::Suspend {
                    account_id: id(BOB),
                    until: None,
                },
                None,
            )
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(h.refusals("missing"), 1);
}

#[tokio::test]
async fn an_operator_cannot_suspend_their_own_account() {
    let h = Harness::new();
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    expect_code(
        h.moderation
            .act(
                &fresh_operator(SUSPENDER),
                Action::Suspend {
                    account_id: id(SUSPENDER),
                    until: None,
                },
                None,
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(h.refusals("invalid"), 1);
}

#[tokio::test]
async fn an_operator_cannot_reinstate_their_own_account() {
    // The tracks-covering case: an operator quietly lifting their own suspension.
    let h = Harness::new();
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    expect_code(
        h.moderation
            .act(
                &fresh_operator(SUSPENDER),
                Action::Reinstate {
                    account_id: id(SUSPENDER),
                },
                None,
            )
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn a_takedown_of_ones_own_content_is_not_a_self_action() {
    // The self-action guard is about accounts, not content: an operator removing media they
    // happen to own is a takedown like any other, because the subject is the object, not a
    // person.
    let h = Harness::new();
    h.medium(A_MEDIUM, REMOVER).await;
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    h.moderation
        .act(
            &fresh_operator(REMOVER),
            Action::RemoveMedia {
                media_id: id(A_MEDIUM),
            },
            None,
        )
        .await
        .expect("removing your own media is allowed");
}

// ---------------------------------------------------------------------------
// The audit trail: one entry per action, naming who did it and to what.
//
// The schema comment asks for the strong promise that an action without an audit row
// cannot exist, and this crate keeps it by writing the row in the same step as the change
// and failing the whole action if the row cannot be written. So every test here checks not
// just that something happened but that the trail says who made it happen and to which
// object. Two distinctions are load-bearing. A filing names the *reporter* as the actor and
// the *report* as the target, never the subject — logging the subject as the actor would
// accuse the reported person of filing the report. And the operator's free-text reason
// lives in the audit's reason column and in no summary, error, or notice, because that text
// is written for the next moderator and names other people.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filing_a_report_records_the_reporter_acting_on_the_report() {
    let h = Harness::new();
    let filed = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Spam),
        )
        .await
        .expect("filed");
    let trail = h.audit_of(AuditTargetKind::Report, filed.report_id).await;
    assert_eq!(trail.len(), 1);
    let entry = &trail[0];
    assert_eq!(entry.actor_id, Some(id(ALICE)), "the reporter is the actor");
    assert_eq!(entry.actor_kind, AuditActorKind::User.to_i16());
    assert_eq!(entry.action, "moderation.report.create");
    assert_eq!(entry.target_kind, AuditTargetKind::Report.to_i16());
    assert_eq!(entry.target_id, Some(filed.report_id));
    assert_eq!(entry.reason, None, "a filing carries no operator reason");
}

#[tokio::test]
async fn the_report_subject_is_never_logged_as_the_actor() {
    // A guard against the worst wiring bug in this file: putting the reported person's id
    // where the reporter's belongs, so the trail reads as though the victim filed it.
    let h = Harness::new();
    let filed = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(Subject::User(id(BOB)), Reason::Harassment),
        )
        .await
        .expect("filed");
    let trail = h.audit_of(AuditTargetKind::Report, filed.report_id).await;
    assert_ne!(
        trail[0].actor_id,
        Some(id(BOB)),
        "the subject is not the actor"
    );
    assert_ne!(
        trail[0].target_id,
        Some(id(BOB)),
        "and the target is the report, not the subject"
    );
}

#[tokio::test]
async fn resolving_records_the_operator_the_target_and_the_reason() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(910, ALICE, BOB, NOW).await;
    h.moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(910),
            Resolution::NoAction,
            Some("checked, benign"),
        )
        .await
        .expect("resolved");
    let trail = h.audit_for(AuditTargetKind::Report, 910).await;
    assert_eq!(trail.len(), 1);
    let entry = &trail[0];
    assert_eq!(entry.actor_id, Some(id(TRIAGER)));
    assert_eq!(entry.actor_kind, AuditActorKind::Operator.to_i16());
    assert_eq!(entry.action, "moderation.report.resolve");
    assert_eq!(entry.target_kind, AuditTargetKind::Report.to_i16());
    assert_eq!(entry.target_id, Some(id(910)));
    assert_eq!(entry.reason.as_deref(), Some("checked, benign"));
}

#[tokio::test]
async fn the_operator_reason_stays_out_of_the_summary() {
    // The free text is in the reason column, and the summary is a fixed sentence about the
    // resolution code: a reader without the audit power never sees the words the operator
    // typed, and even the summary that anyone with the report might see does not repeat them.
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.seed_report(911, ALICE, BOB, NOW).await;
    let secret = "private note mentioning carol's real name";
    h.moderation
        .resolve(
            &fresh_operator(TRIAGER),
            id(911),
            Resolution::Invalid,
            Some(secret),
        )
        .await
        .expect("resolved");
    let entry = &h.audit_for(AuditTargetKind::Report, 911).await[0];
    assert_eq!(entry.reason.as_deref(), Some(secret));
    assert!(
        !entry.summary.contains("carol"),
        "the summary does not echo the reason"
    );
}

#[tokio::test]
async fn an_action_is_recorded_against_its_account() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    h.moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Suspend {
                account_id: id(BOB),
                until: None,
            },
            Some("repeat abuse"),
        )
        .await
        .expect("suspended");
    let trail = h.audit_for(AuditTargetKind::Account, BOB).await;
    assert_eq!(trail.len(), 1);
    assert_eq!(trail[0].action, "moderation.account.suspend");
    assert_eq!(trail[0].actor_id, Some(id(SUSPENDER)));
    assert_eq!(trail[0].target_id, Some(id(BOB)));
    assert_eq!(trail[0].reason.as_deref(), Some("repeat abuse"));
}

#[tokio::test]
async fn each_takedown_is_recorded_against_its_own_target_kind() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.medium(A_MEDIUM, BOB).await;
    h.room(A_ROOM, BOB).await;
    h.bot(A_BOT, BOB, 700).await;
    h.message(A_CONVERSATION, A_MESSAGE, BOB).await;
    h.staff.grant(REMOVER, Powers::TAKEDOWN);
    for action in [
        Action::RemoveMedia {
            media_id: id(A_MEDIUM),
        },
        Action::ArchiveRoom {
            room_id: id(A_ROOM),
        },
        Action::DisableBot { bot_id: id(A_BOT) },
        Action::RemoveMessage {
            conversation_id: id(A_CONVERSATION),
            message_id: id(A_MESSAGE),
        },
    ] {
        h.moderation
            .act(&fresh_operator(REMOVER), action, None)
            .await
            .expect("done");
    }
    assert_eq!(
        h.audit_for(AuditTargetKind::Media, A_MEDIUM).await[0].action,
        "moderation.media.remove"
    );
    assert_eq!(
        h.audit_for(AuditTargetKind::Room, A_ROOM).await[0].action,
        "moderation.room.archive"
    );
    assert_eq!(
        h.audit_for(AuditTargetKind::Bot, A_BOT).await[0].action,
        "moderation.bot.disable"
    );
    assert_eq!(
        h.audit_for(AuditTargetKind::Message, A_MESSAGE).await[0].action,
        "moderation.message.remove"
    );
}

#[tokio::test]
async fn repeating_an_action_records_it_again() {
    // Actions are not deduplicated the way filings are: a second suspension of an already
    // suspended account is a real decision somebody made, and the trail keeps both so the
    // history is complete.
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    for _ in 0..2 {
        h.moderation
            .act(
                &fresh_operator(SUSPENDER),
                Action::Suspend {
                    account_id: id(BOB),
                    until: None,
                },
                None,
            )
            .await
            .expect("suspended");
    }
    assert_eq!(h.audit_for(AuditTargetKind::Account, BOB).await.len(), 2);
    assert_eq!(h.actions("suspend"), 2);
}

#[tokio::test]
async fn an_auditor_reads_an_accounts_history_through_the_service() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.staff.grant(SUSPENDER, Powers::SUSPEND);
    h.staff.grant(AUDITOR, Powers::AUDIT);
    h.moderation
        .act(
            &fresh_operator(SUSPENDER),
            Action::Suspend {
                account_id: id(BOB),
                until: None,
            },
            None,
        )
        .await
        .expect("suspended");
    let entries = h
        .moderation
        .audit(&operator(AUDITOR), AuditTargetKind::Account, id(BOB), None)
        .await
        .expect("the audit power reads the trail");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "moderation.account.suspend");
    assert_eq!(h.plain("migo_moderation_audit_reads_total"), 1);
}

// ---------------------------------------------------------------------------
// The queue is paged, and the page ceiling is the server's
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_queue_page_is_capped_however_much_is_asked_for() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.account(BOB, "bob").await;
    for n in 0..(MAX_PAGE as u128 + 10) {
        h.account(1_000 + n, &format!("reporter{n}")).await;
        h.seed_report(2_000 + n, 1_000 + n, BOB, NOW - (n as i64) * MINUTE)
            .await;
    }

    let named = h
        .moderation
        .queue(&operator(TRIAGER), Some(u16::MAX))
        .await
        .expect("triage powers open the queue");
    let unnamed = h
        .moderation
        .queue(&operator(TRIAGER), None)
        .await
        .expect("triage powers open the queue");

    // A moderator asking for everything gets one page, not everything: the ceiling belongs
    // to the server because a queue that has grown to a hundred thousand open reports is
    // exactly the queue somebody will try to read in a single request.
    assert_eq!(named.len(), MAX_PAGE as usize);
    // Asking for nothing in particular gets the smaller default, which is the size a
    // dashboard actually renders.
    assert_eq!(unnamed.len(), DEFAULT_PAGE as usize);
    assert!(unnamed.len() < named.len());
}

#[tokio::test]
async fn the_oldest_open_report_is_at_the_front_of_the_queue() {
    let h = Harness::new();
    h.staff.grant(TRIAGER, Powers::TRIAGE);
    h.account(BOB, "bob").await;
    h.account(ALICE, "alice").await;
    h.account(CAROL, "carol").await;
    h.account(MALLORY, "mallory").await;
    h.seed_report(701, ALICE, BOB, NOW - HOUR).await;
    h.seed_report(702, CAROL, BOB, NOW - 2 * HOUR).await;
    h.seed_report(703, MALLORY, BOB, NOW - 30 * MINUTE).await;

    let queue = h
        .moderation
        .queue(&operator(TRIAGER), None)
        .await
        .expect("triage powers open the queue");

    // Oldest first, because a queue ordered newest first is a queue whose tail is never
    // read: the reports that wait longest are the ones a busy team never reaches, and the
    // person who filed the one from two hours ago is still waiting.
    assert_eq!(queue.len(), 3);
    assert_eq!(queue[0].report_id, id(702));
    assert_eq!(queue[2].report_id, id(703));
}

// ---------------------------------------------------------------------------
// A report is a pointer, never a copy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_report_about_a_message_stores_a_pointer_and_not_the_conversation() {
    let h = Harness::new();
    h.account(ALICE, "alice").await;
    h.message(A_CONVERSATION, A_MESSAGE, BOB).await;

    let filed = h
        .moderation
        .file_report(
            &caller(ALICE),
            Filing::new(
                Subject::Message {
                    conversation_id: id(A_CONVERSATION),
                    message_id: id(A_MESSAGE),
                },
                Reason::Harassment,
            )
            .with_evidence(id(A_MESSAGE)),
        )
        .await
        .expect("a report about a message that exists is accepted");

    let row = h.report_row(filed.report_id).await;
    // The subject column holds the message and nothing else. A report carries one
    // subject_id, and writing the conversation there as well would put a second key in a
    // column indexed as one -- so the conversation stays in memory for the length of the
    // request, which is all the read of the message needed it for.
    assert_eq!(row.subject_kind, report_subject::MESSAGE);
    assert_eq!(row.subject_id, id(A_MESSAGE));
    assert_ne!(row.subject_id, id(A_CONVERSATION));
    // Evidence is an id. Copying ciphertext into a moderation table would undo the point
    // of encrypting it, and there is no plaintext on this server to copy instead.
    assert_eq!(row.evidence_ref, Some(id(A_MESSAGE)));
    assert_eq!(row.note, None);
    assert_eq!(row.status, report_status::OPEN);
    assert_eq!(row.resolution, None);
}

// ---------------------------------------------------------------------------
// Adaptive rate limiting, which is arithmetic and therefore has to be arguable
//
// Brief section 50 asks for rate limits that tighten around abuse. The whole of that here
// is score, then risk_of, then Risk::clamp over the tier the session would otherwise have
// -- three pure functions, no bucket of its own, and no way to move a tier upwards.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_ordinary_account_scores_clear_and_keeps_its_tier() {
    let h = Harness::new();
    h.account(BOB, "bob").await;

    let quiet = Signals {
        messages_last_minute: 4,
        recipients_last_hour: 3,
        rooms_joined_last_hour: 1,
        friend_requests_last_hour: 2,
        refusals_last_hour: 0,
        reports_against: 0,
        account_age_ms: 400 * DAY,
    };
    let assessment: Assessment = h
        .moderation
        .assess(id(BOB), quiet, ts(NOW))
        .await
        .expect("an assessment is not a privileged read");

    assert_eq!(assessment.account_id, id(BOB));
    assert_eq!(assessment.risk, Risk::Clear);
    assert!(assessment.score < ModerationConfig::default().watch_at);
    // Clear changes nothing, which is the case that has to stay cheap: almost every
    // account is this one, and a scorer that slowed everybody down to catch a few would be
    // the abuse.
    assert_eq!(
        effective_tier(&assessment, TrustTier::Established),
        TrustTier::Established
    );
    assert_eq!(h.assessments("clear"), 1);
}

#[tokio::test]
async fn a_broadcaster_is_throttled_rather_than_stopped() {
    let h = Harness::new();
    h.account(MALLORY, "mallory").await;

    let fanning_out = Signals {
        messages_last_minute: 240,
        recipients_last_hour: 300,
        rooms_joined_last_hour: 40,
        friend_requests_last_hour: 120,
        refusals_last_hour: 200,
        reports_against: 0,
        account_age_ms: 20 * MINUTE,
    };
    let assessment = h
        .moderation
        .assess(id(MALLORY), fanning_out, ts(NOW))
        .await
        .expect("an assessment is not a privileged read");

    // Two hundred messages to one friend is a conversation; two hundred to two hundred
    // strangers is not. The consequence is a smaller budget, which slows an abuser and
    // inconveniences a false positive -- the right way round for a decision made by
    // arithmetic rather than by a person.
    assert!(
        assessment.risk.index() >= Risk::Throttle.index(),
        "{assessment:?}"
    );
    assert!(assessment.score >= ModerationConfig::default().throttle_at);
    assert_ne!(
        effective_tier(&assessment, TrustTier::Established),
        TrustTier::Established
    );
    // Nobody was suspended: auto_suspend is off by default and this is why.
    assert_eq!(h.status_of(MALLORY).await, AccountStatus::Active);
}

#[tokio::test]
async fn the_score_is_never_a_way_to_earn_a_better_tier() {
    let config = ModerationConfig::default();
    let spotless = Signals::default();
    assert_eq!(risk_of(score(&spotless, &config), &config), Risk::Clear);

    // Every level, against every tier: the result is the tier it was given or something
    // smaller, never something larger. Otherwise the published scoring rules in this file
    // would be a recipe for earning trust by behaving in a pattern.
    for risk in Risk::ALL {
        for tier in [
            TrustTier::Anonymous,
            TrustTier::New,
            TrustTier::Established,
            TrustTier::Trusted,
            TrustTier::Bot,
        ] {
            let clamped = risk.clamp(tier);
            let assessment = Assessment {
                account_id: id(BOB),
                score: 0,
                risk,
            };
            assert_eq!(effective_tier(&assessment, tier), clamped);
            if risk == Risk::Clear || risk == Risk::Watch {
                assert_eq!(clamped, tier, "{risk:?} moved {tier:?}");
            }
        }
    }
    // A bot keeps its tier under throttling, because demoting a bot to New rate limits an
    // integration into uselessness for sending the volume it exists to send.
    assert_eq!(Risk::Throttle.clamp(TrustTier::Bot), TrustTier::Bot);
    assert_eq!(Risk::Restrict.clamp(TrustTier::Bot), TrustTier::Anonymous);
}

#[tokio::test]
async fn reports_from_other_people_weigh_more_than_any_rate() {
    let h = Harness::new();
    h.account(BOB, "bob").await;
    h.account(MALLORY, "mallory").await;
    for n in 0..6 {
        h.account(1_100 + n, &format!("witness{n}")).await;
        h.seed_report(2_100 + n, 1_100 + n, MALLORY, NOW - MINUTE)
            .await;
    }

    let idle = Signals {
        account_age_ms: 400 * DAY,
        ..Signals::default()
    };
    let reported = h
        .moderation
        .assess(id(MALLORY), idle, ts(NOW))
        .await
        .expect("an assessment is not a privileged read");
    let unreported = h
        .moderation
        .assess(id(BOB), idle, ts(NOW))
        .await
        .expect("an assessment is not a privileged read");

    // The caller does not supply this signal and cannot suppress it: the service counts the
    // reports itself, inside REPORT_WINDOW_MS. It is the heaviest input because every unit
    // of it came from a person who pressed a button, which no traffic counter can claim.
    assert!(reported.score > unreported.score, "{reported:?}");
    assert_eq!(unreported.score, 0);
    // Six complaints from six people, and nothing else at all, is already worth watching.
    assert_ne!(reported.risk, Risk::Clear);
}

#[tokio::test]
async fn a_report_older_than_the_window_stops_counting() {
    let h = Harness::new();
    h.account(MALLORY, "mallory").await;
    for n in 0..6 {
        h.account(1_200 + n, &format!("witness{n}")).await;
        h.seed_report(2_200 + n, 1_200 + n, MALLORY, NOW - REPORT_WINDOW_MS - DAY)
            .await;
    }

    let idle = Signals {
        account_age_ms: 400 * DAY,
        ..Signals::default()
    };
    let assessment = h
        .moderation
        .assess(id(MALLORY), idle, ts(NOW))
        .await
        .expect("an assessment is not a privileged read");

    // A grievance from last month is history, not evidence of what is happening now. A
    // window that never closes turns one bad week into a permanently smaller budget, which
    // is a punishment nobody decided to hand out and nobody can appeal.
    assert_eq!(assessment.score, 0);
    assert_eq!(assessment.risk, Risk::Clear);
}

#[tokio::test]
async fn an_assessment_is_never_a_metric_label() {
    let h = Harness::new();
    h.account(MALLORY, "mallory").await;
    let busy = Signals {
        messages_last_minute: 300,
        recipients_last_hour: 300,
        refusals_last_hour: 400,
        account_age_ms: MINUTE,
        ..Signals::default()
    };
    h.moderation
        .assess(id(MALLORY), busy, ts(NOW))
        .await
        .expect("an assessment is not a privileged read");

    // The score is published to an operator who asks why an account is throttled, and
    // suppressed from every label, because a score attached to an account id on an
    // unauthenticated scrape endpoint is a dossier.
    let rendered = h.registry.render();
    assert!(!rendered.contains(&id(MALLORY).to_text()));
    assert!(!rendered.contains("mallory"));
    assert!(rendered.contains("migo_moderation_assessments_total"));
    for risk in Risk::ALL {
        assert!(rendered.contains(risk.label()));
    }
}

// ---------------------------------------------------------------------------
// What the person on the other end is told
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suspension_produces_a_notice_and_a_takedown_does_not() {
    let h = Harness::new();
    h.staff.grant(ADMIN, Powers::SUSPEND.with(Powers::TAKEDOWN));
    h.account(ADMIN, "admin").await;
    h.account(BOB, "bob").await;
    h.medium(A_MEDIUM, BOB).await;

    let notice: Option<Notice> = h
        .moderation
        .act(
            &fresh_operator(ADMIN),
            Action::Suspend {
                account_id: id(BOB),
                until: Some(ts(NOW + 7 * DAY)),
            },
            Some("repeated harassment after a warning"),
        )
        .await
        .expect("suspend powers suspend");
    let notice = notice.expect("a suspension is something its subject must be told");
    assert_eq!(notice.audience, id(BOB));
    assert_eq!(notice.at, ts(NOW));

    // A takedown produces no notice, and that is a fact about what this crate knows rather
    // than a decision about what somebody deserves: the removal reaches the room it
    // happened in, and this crate does not know who was watching.
    let quiet = h
        .moderation
        .act(
            &fresh_operator(ADMIN),
            Action::RemoveMedia {
                media_id: id(A_MEDIUM),
            },
            None,
        )
        .await
        .expect("takedown powers remove media");
    assert!(quiet.is_none());
}

#[tokio::test]
async fn an_operators_free_text_reaches_the_audit_trail_and_nothing_else() {
    let h = Harness::new();
    h.staff.grant(ADMIN, Powers::SUSPEND);
    h.staff.grant(AUDITOR, Powers::AUDIT);
    h.account(ADMIN, "admin").await;
    h.account(BOB, "bob").await;

    const PROSE: &str = "spoke to the reporter by phone on Tuesday";
    let notice = h
        .moderation
        .act(
            &fresh_operator(ADMIN),
            Action::Suspend {
                account_id: id(BOB),
                until: None,
            },
            Some(PROSE),
        )
        .await
        .expect("suspend powers suspend")
        .expect("a suspension is something its subject must be told");

    // The reason a moderator typed is for the next moderator, not for the person it is
    // about and not for a scrape endpoint: it can name a third party, quote a private
    // message, or contain a case number from somewhere else entirely.
    assert!(!format!("{notice:?}").contains(PROSE));
    assert!(!h.registry.render().contains(PROSE));

    let trail = h
        .moderation
        .audit(&operator(AUDITOR), AuditTargetKind::Account, id(BOB), None)
        .await
        .expect("audit powers read the trail");
    assert!(trail
        .iter()
        .any(|entry| entry.reason.as_deref() == Some(PROSE)));
}
