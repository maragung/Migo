//! The moderation operator surface: reading the queue, ruling on a case, acting, and reading the
//! audit trail.
//!
//! # Why these are REST routes and not opcodes
//!
//! Filing a report and ruling on one are already opcodes — `REPORT_CREATE` and
//! `MODERATION_ACTION` — and both stay. What the socket has never had is a way to *read*: the queue
//! is a list somebody browses, the audit trail is a list somebody scrolls, and a list is what HTTP
//! is for. So the two surfaces split by shape rather than by audience. The socket keeps the acts a
//! client performs from inside the conversation it is looking at; this keeps the reading a
//! moderator does between them.
//!
//! The split is not a second implementation, and that is the point of routing
//! `POST /case/{report_id}/resolve` through [`Warden::resolve`] rather than writing a REST-shaped
//! copy of it: the opcode and this route reach the same method, so neither can be the door that
//! skips the audit row, and the powers behind both come from the same roster. A dashboard ruling on
//! the case it is already showing does not have to open a second connection to say so.
//!
//! # The powers are not in the request
//!
//! Every handler here builds an [`Operator`] with [`Powers::NONE`] and lets the service overwrite
//! it — [`Warden`]'s own note says the field is what the roster lookup produced, never something a
//! client sends, and this module is not an exception to that. What the route contributes is the
//! account, the device, the truncated network, and whether the session proved a factor recently
//! enough.
//!
//! # Freshness is asked about, not demanded
//!
//! `Identity::require_fresh` exists and is the obvious thing to call at the top of an operator
//! handler, and calling it here would be wrong. It would put the `REAUTHENTICATION_REQUIRED`
//! refusal *in front of* the power check, and an ordinary account would learn from the refusal that
//! a freshness rule exists — which is precisely the leak the service's own ordering is careful to
//! avoid ("a caller who is not staff should not learn that the freshness rule exists"). So this
//! module mirrors the gateway: `is_fresh` sets a flag on the operator, and the service decides what
//! to do about it, after it has decided whether the caller is staff at all.
//!
//! # What this surface deliberately does not do
//!
//! It names ids and never usernames. Resolving one to the other is the auth service's account
//! directory, and moderation is not a second reader of it: a queue row carries who reported whom by
//! id, and a dashboard renders what it can resolve through the surfaces it already has. The
//! alternative — giving this crate a store handle so it can join a username onto every row — would
//! make the moderation surface a general account lookup with a filter in front of it.
//!
//! # The queue is a window, not a cursor
//!
//! [`Warden::queue`] returns at most a bounded number of the longest-waiting open reports, so this
//! route offers exactly that: a limit, clamped by the crate into `1..=MAX_PAGE`, defaulting to
//! `DEFAULT_PAGE` when a client names none. There is no cursor, because the domain has none to
//! honour — a `next_cursor` invented here would be a promise this layer cannot keep. A client that
//! wants a longer view asks for a larger limit, up to the crate's own ceiling.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use migo_core::{Id, Timestamp};
use migo_moderation::{
    Action, Case, Notice, Operator, Outcome, Powers, Reason, Resolution, Roster, Warden,
};
use migo_store::model::{AuditActorKind, AuditEntry, AuditTargetKind};

use crate::extract::Authenticated;
use crate::ApiState;

/// The operator routes: one read any signed-in account may make to learn what it may do, four
/// reads and writes behind the powers they require, and no route that answers for a caller who is
/// not staff.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/moderation/whoami", get(standing))
        .route("/moderation/queue", get(queue))
        .route("/moderation/case/{report_id}", get(one_case))
        .route("/moderation/case/{report_id}/resolve", post(resolve))
        .route("/moderation/act", post(act))
        .route("/moderation/audit/{target_kind}/{target_id}", get(audit))
}

/// The operator behind one request, built from the session and the request facts.
///
/// The powers are [`Powers::NONE`] because they are not this layer's to decide; see the module
/// note. Everything else here is what the audit row will say about who acted and from where.
fn operator_for(auth: &Authenticated, now: Timestamp) -> Operator {
    let identity = &auth.identity;
    let mut operator = Operator::new(
        identity.account_id(),
        identity.device_id(),
        Powers::NONE,
        now,
    );
    // A flag, not a refusal: the service owns the ordering. See the module note.
    if identity.is_fresh(now) {
        operator = operator.reauthenticated();
    }
    if let Some(ip) = auth.facts.ip {
        // The truncated network class and never the address, which is the same value the gateway
        // stamps onto its own audit rows — one function, so the two doors cannot disagree about
        // what "the same network" means.
        operator = operator.from_network(migo_ratelimit::scope::network(ip));
    }
    if let Some(request_id) = &auth.facts.request_id {
        operator = operator.with_request_id(request_id.clone());
    }
    operator
}

/// `GET /v1/moderation/whoami` — what the caller may do.
///
/// Never fails on standing, exactly like `/v1/admins/whoami`: an ordinary account gets an empty
/// power list, which is the answer and not an error. This is the route a dashboard calls before it
/// renders anything, so that an account with no powers is shown no operator surface at all rather
/// than a set of buttons that refuse.
///
/// The directory answers this, not the service: the question is what the roster says about one
/// account, and asking the service to ask the roster on this layer's behalf would buy nothing.
/// Nothing is *done* on the strength of the answer — every route below goes through the warden,
/// which resolves the same powers itself and refuses on its own authority.
async fn standing(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<StandingResponse>, crate::ApiError> {
    let account_id = auth.identity.account_id();
    let powers = state.roster().powers(account_id).await?;
    Ok(Json(StandingResponse::of(account_id, powers)))
}

/// `GET /v1/moderation/queue` — the open reports, longest-waiting first. Requires
/// [`Powers::TRIAGE`].
async fn queue(
    State(state): State<ApiState>,
    auth: Authenticated,
    Query(query): Query<LimitQuery>,
) -> Result<Json<QueueResponse>, crate::ApiError> {
    let now = state.now();
    let operator = operator_for(&auth, now);
    let cases = state.warden().queue(&operator, query.limit).await?;
    Ok(Json(QueueResponse {
        cases: cases.iter().map(CaseDto::of).collect(),
    }))
}

/// `GET /v1/moderation/case/{report_id}` — one report, open or settled.
///
/// Requires [`Powers::TRIAGE`]. A report that does not exist is `NOT_FOUND` from the service, not
/// an empty case: this route has no way to tell "no such report" from "not for you" and does not
/// pretend to.
async fn one_case(
    State(state): State<ApiState>,
    auth: Authenticated,
    Path(report_id): Path<Id>,
) -> Result<Json<CaseDto>, crate::ApiError> {
    let now = state.now();
    let operator = operator_for(&auth, now);
    let case = state.warden().report(&operator, report_id).await?;
    Ok(Json(CaseDto::of(&case)))
}

/// `POST /v1/moderation/case/{report_id}/resolve` — rule on one report.
///
/// Requires [`Powers::TRIAGE`] and a fresh session. The body carries the decision and, optionally,
/// the operator's own words; the reason is vetted and stored by the service, and the response is
/// the report as the store now holds it rather than what this layer intended to write.
async fn resolve(
    State(state): State<ApiState>,
    auth: Authenticated,
    Path(report_id): Path<Id>,
    Json(body): Json<ResolveBody>,
) -> Result<Json<CaseDto>, crate::ApiError> {
    let now = state.now();
    let operator = operator_for(&auth, now);
    let resolution = Resolution::of_i16(body.resolution);
    let case = state
        .warden()
        .resolve(&operator, report_id, resolution, body.reason.as_deref())
        .await?;
    Ok(Json(CaseDto::of(&case)))
}

/// `POST /v1/moderation/act` — take an action against a subject.
///
/// Requires whatever power [`Action::requires`] names — triage to warn, takedown to remove
/// content, suspend to close an account — and a fresh session. The response echoes the action's own
/// stable name, the same string the audit row now carries, so a dashboard can show what was
/// recorded without keeping its own copy of the mapping.
async fn act(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<ActionBody>,
) -> Result<Json<ActResponse>, crate::ApiError> {
    let now = state.now();
    let operator = operator_for(&auth, now);
    let (action, reason) = body.into_parts();
    let name = action.name();
    let notice = state
        .warden()
        .act(&operator, action, reason.as_deref())
        .await?;
    Ok(Json(ActResponse {
        action: name,
        notice: notice.map(NoticeDto::of),
    }))
}

/// `GET /v1/moderation/audit/{target_kind}/{target_id}` — the trail for one target, newest first.
///
/// Requires [`Powers::AUDIT`]. This is also the warning history: a warning is an audit entry and
/// nothing else, so the trail over an account is what the dashboard shows under "warnings".
async fn audit(
    State(state): State<ApiState>,
    auth: Authenticated,
    Path((target_kind, target_id)): Path<(String, Id)>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<AuditResponse>, crate::ApiError> {
    let kind = target_kind_named(&target_kind).ok_or_else(|| {
        crate::ApiError::from(migo_protocol::fault::validation(
            "target_kind",
            "unknown audit target kind",
        ))
    })?;
    let now = state.now();
    let operator = operator_for(&auth, now);
    let entries = state
        .warden()
        .audit(&operator, kind, target_id, query.limit)
        .await?;
    Ok(Json(AuditResponse {
        entries: entries.iter().map(AuditDto::of).collect(),
    }))
}

/// The `?limit=` these listings accept, or none for the crate's own default.
///
/// A hint rather than a contract: [`Warden::queue`] and [`Warden::audit`] clamp whatever arrives
/// into the crate's own ceiling, so an absurd value costs a client nothing to send and gains them
/// nothing. A value that is not a number at all is refused by the extractor before a handler runs.
#[derive(Deserialize)]
struct LimitQuery {
    /// The requested page size; `None` lets the crate choose.
    #[serde(default)]
    limit: Option<u16>,
}

/// What the caller may do, in the words a dashboard gates its buttons on.
#[derive(Serialize)]
struct StandingResponse {
    /// The account these powers belong to — the caller's own, always.
    account_id: Id,
    /// The raw bitmask, for a client that would rather compare numbers.
    bits: u32,
    /// The powers, by name, so a client needs no copy of the bit table.
    powers: Vec<&'static str>,
    /// Whether the caller holds any power at all.
    staff: bool,
}

impl StandingResponse {
    /// Projects one resolved power set.
    fn of(account_id: Id, powers: Powers) -> Self {
        Self {
            account_id,
            bits: powers.bits(),
            powers: power_names(powers),
            staff: !powers.is_empty(),
        }
    }
}

/// The names of the powers present, in the order the crate declares them.
///
/// The list is written here rather than derived from the bit constants because a name is a wire
/// word and a constant name is not: renaming `Powers::TAKEDOWN` in Rust must not rename the string
/// a dashboard compares against, and a test below pins the two in step either way.
fn power_names(powers: Powers) -> Vec<&'static str> {
    let mut names = Vec::new();
    if powers.contains(Powers::TRIAGE) {
        names.push("triage");
    }
    if powers.contains(Powers::TAKEDOWN) {
        names.push("takedown");
    }
    if powers.contains(Powers::SUSPEND) {
        names.push("suspend");
    }
    if powers.contains(Powers::AUDIT) {
        names.push("audit");
    }
    names
}

/// The open reports, as one page of the queue.
#[derive(Serialize)]
struct QueueResponse {
    /// The cases, longest-waiting first — the order the crate returns them in, because a report
    /// that has waited longest is the one most likely to matter.
    cases: Vec<CaseDto>,
}

/// One report, as the operator surface states it.
///
/// Every numeric code travels with its name, and for the subject kind that is not a convenience:
/// the store numbers a media object 3 and a bot 4, while the *wire* numbers a bot 3 and has no
/// media kind at all, so a client that assumed the wire's numbers would read every bot report as a
/// report about a media object. The queue is a store-side view, so these are the store's numbers,
/// and a test below pins them.
#[derive(Serialize)]
struct CaseDto {
    /// The case id, which is the report id.
    report_id: Id,
    /// Who filed it.
    reporter_id: Id,
    /// What it is about, as `report.subject_kind`.
    subject_kind: i16,
    /// That kind's name.
    subject_kind_name: &'static str,
    /// Which one.
    subject_id: Id,
    /// The room it happened in, when it happened in one.
    room_id: Option<Id>,
    /// Why, as the reporter's code.
    reason: i16,
    /// That reason's name.
    reason_name: &'static str,
    /// The reporter's own words, if they wrote any.
    note: Option<String>,
    /// A pointer to evidence, when the filing carried one.
    evidence_ref: Option<Id>,
    /// `report.status`.
    status: i16,
    /// Whether the case is still waiting for a decision.
    open: bool,
    /// When it was filed.
    created_at: Timestamp,
    /// When it was closed.
    resolved_at: Option<Timestamp>,
    /// Who closed it.
    resolved_by: Option<Id>,
    /// What it came to, when it has come to something.
    resolution: Option<i16>,
    /// That resolution's name.
    resolution_name: Option<&'static str>,
}

impl CaseDto {
    /// Projects one case.
    fn of(case: &Case) -> Self {
        Self {
            report_id: case.report_id,
            reporter_id: case.reporter_id,
            subject_kind: case.subject_kind,
            subject_kind_name: subject_kind_name(case.subject_kind),
            subject_id: case.subject_id,
            room_id: case.room_id,
            reason: case.reason.to_i16(),
            reason_name: case.reason.label(),
            note: case.note.clone(),
            evidence_ref: case.evidence_ref,
            status: case.status,
            open: case.is_open(),
            created_at: case.created_at,
            resolved_at: case.resolved_at,
            resolved_by: case.resolved_by,
            resolution: case.resolution.map(Resolution::to_i16),
            resolution_name: case.resolution.map(Resolution::label),
        }
    }
}

/// The names of the `report.subject_kind` values, and the one place they are written down.
///
/// The store's numbering, not the wire's: media is 3 and a bot is 4 here, where the wire puts a bot
/// at 3 and has no media kind. See [`CaseDto`].
const SUBJECT_KINDS: [(i16, &str); 5] = [
    (migo_store::model::report_subject::USER, "user"),
    (migo_store::model::report_subject::MESSAGE, "message"),
    (migo_store::model::report_subject::ROOM, "room"),
    (migo_store::model::report_subject::MEDIA, "media"),
    (migo_store::model::report_subject::BOT, "bot"),
];

/// The name of one stored subject kind.
///
/// An unknown code reads as `"unknown"` rather than being refused or guessed: a report row written
/// by a newer build must still be listable by this one, and mislabelling what somebody reported is
/// worse than admitting this build does not know.
fn subject_kind_name(kind: i16) -> &'static str {
    SUBJECT_KINDS
        .iter()
        .find(|(code, _)| *code == kind)
        .map_or("unknown", |(_, name)| *name)
}

/// The body of a ruling: the decision, and the operator's own words if they wrote any.
#[derive(Deserialize)]
struct ResolveBody {
    /// The decision, as `report.resolution`.
    resolution: i16,
    /// Free text the operator wrote, stored on the audit row. Optional because most rulings need
    /// no sentence, and an empty string here would be a sentence nobody wrote.
    #[serde(default)]
    reason: Option<String>,
}

/// The body of an action, as a tagged object: `{"action": "suspend", "account_id": "...", ...}`.
///
/// The tag is the word the dashboard's button says, and a test below pins each one to
/// [`Action::label`] — the metric's own name for the same act — so the request vocabulary, the
/// button and the metric series cannot drift into three words for one thing. The audit row will
/// say a fourth, the dotted [`Action::name`], and that is deliberate: a metric label is short
/// because it is a series name, and an audit action is dotted because it is a compliance record.
///
/// Every variant carries `reason`, so an operator can say why for any action the surface offers.
/// It is a field on each variant rather than one field beside a flattened action because a tagged
/// body *is* the action's own fields: a takedown's reason belongs to the takedown.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ActionBody {
    /// Tell somebody their behaviour was looked at and found wanting.
    Warn {
        /// Who is being warned.
        account_id: Id,
        /// Free text the operator wrote, stored on the audit row.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Suspend an account, optionally until a date.
    Suspend {
        /// Who is being suspended.
        account_id: Id,
        /// When it lifts by itself; absent or null means indefinite.
        #[serde(default)]
        until: Option<Timestamp>,
        /// Free text the operator wrote, stored on the audit row.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Return an account to normal.
    Reinstate {
        /// Who is being reinstated.
        account_id: Id,
        /// Free text the operator wrote, stored on the audit row.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Remove one message.
    RemoveMessage {
        /// Which conversation.
        conversation_id: Id,
        /// Which message.
        message_id: Id,
        /// Free text the operator wrote, stored on the audit row.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Take a media object down.
    RemoveMedia {
        /// Which object.
        media_id: Id,
        /// Free text the operator wrote, stored on the audit row.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Archive a room.
    ArchiveRoom {
        /// Which room.
        room_id: Id,
        /// Free text the operator wrote, stored on the audit row.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Disable a bot.
    DisableBot {
        /// Which bot.
        bot_id: Id,
        /// Free text the operator wrote, stored on the audit row.
        #[serde(default)]
        reason: Option<String>,
    },
}

impl ActionBody {
    /// The domain action this body names, and the operator's words for it.
    fn into_parts(self) -> (Action, Option<String>) {
        match self {
            Self::Warn { account_id, reason } => (Action::Warn { account_id }, reason),
            Self::Suspend {
                account_id,
                until,
                reason,
            } => (Action::Suspend { account_id, until }, reason),
            Self::Reinstate { account_id, reason } => (Action::Reinstate { account_id }, reason),
            Self::RemoveMessage {
                conversation_id,
                message_id,
                reason,
            } => (
                Action::RemoveMessage {
                    conversation_id,
                    message_id,
                },
                reason,
            ),
            Self::RemoveMedia { media_id, reason } => (Action::RemoveMedia { media_id }, reason),
            Self::ArchiveRoom { room_id, reason } => (Action::ArchiveRoom { room_id }, reason),
            Self::DisableBot { bot_id, reason } => (Action::DisableBot { bot_id }, reason),
        }
    }
}

/// What an action did, as the surface reports it.
#[derive(Serialize)]
struct ActResponse {
    /// The action's own stable name — the string the audit row now carries.
    action: &'static str,
    /// The notice to deliver, `None` for every content takedown.
    notice: Option<NoticeDto>,
}

/// One moderation notice.
///
/// Carried because an operator acting on an account should be able to see what that account will be
/// told; `None` for a takedown is a fact about what the crate knows rather than a decision, and a
/// dashboard that showed "no notice" for a message removal would be reporting the truth.
#[derive(Serialize)]
struct NoticeDto {
    /// The account this reaches, across every device it has.
    audience: Id,
    /// What was done, by name.
    outcome: &'static str,
    /// When a suspension lifts, when it does.
    until: Option<Timestamp>,
    /// Why, as a code, when the action was tied to a report.
    reason: Option<i16>,
    /// That reason's name.
    reason_name: Option<&'static str>,
    /// When it was done.
    at: Timestamp,
}

impl NoticeDto {
    /// Projects one notice.
    fn of(notice: Notice) -> Self {
        let until = match notice.outcome {
            Outcome::Suspended { until } => until,
            Outcome::Warned | Outcome::Reinstated => None,
        };
        Self {
            audience: notice.audience,
            outcome: notice.outcome.label(),
            until,
            reason: notice.reason.map(Reason::to_i16),
            reason_name: notice.reason.map(Reason::label),
            at: notice.at,
        }
    }
}

/// The audit trail for one target.
#[derive(Serialize)]
struct AuditResponse {
    /// The entries, newest first.
    entries: Vec<AuditDto>,
}

/// One audit entry, as the surface states it.
#[derive(Serialize)]
struct AuditDto {
    /// The entry's own id.
    audit_id: Id,
    /// Who acted, when an account did.
    actor_id: Option<Id>,
    /// What kind of actor it was.
    actor_kind: i16,
    /// That kind's name.
    actor_kind_name: &'static str,
    /// The stable dotted action name.
    action: String,
    /// What kind of thing was acted on.
    target_kind: i16,
    /// That kind's name.
    target_kind_name: &'static str,
    /// Which one, when the row names one.
    target_id: Option<Id>,
    /// One line describing what happened.
    summary: String,
    /// The operator's own words, when they wrote any.
    reason: Option<String>,
    /// The correlation id, so an entry can be joined against a trace.
    request_id: Option<String>,
    /// The truncated network class the actor came from.
    ip_class: Option<String>,
    /// When it happened.
    created_at: Timestamp,
}

impl AuditDto {
    /// Projects one entry.
    fn of(entry: &AuditEntry) -> Self {
        let kind = AuditTargetKind::from_i16(entry.target_kind);
        Self {
            audit_id: entry.audit_id,
            actor_id: entry.actor_id,
            actor_kind: entry.actor_kind,
            actor_kind_name: actor_kind_name(entry.actor_kind),
            action: entry.action.clone(),
            target_kind: entry.target_kind,
            target_kind_name: kind.map_or("unknown", target_kind_name),
            target_id: entry.target_id,
            summary: entry.summary.clone(),
            reason: entry.reason.clone(),
            request_id: entry.request_id.clone(),
            ip_class: entry.ip_class.clone(),
            created_at: entry.created_at,
        }
    }
}

/// The names of the audit target kinds, and the one place they are written down.
///
/// One table read from both directions — [`target_kind_name`] for the JSON,
/// [`target_kind_named`] for the path — so a kind can never be spelled one way on the way out and
/// another on the way back in, which would leave a dashboard unable to ask about a row it had just
/// been shown.
const AUDIT_TARGET_KINDS: [(AuditTargetKind, &str); 15] = [
    (AuditTargetKind::Account, "account"),
    (AuditTargetKind::Device, "device"),
    (AuditTargetKind::Session, "session"),
    (AuditTargetKind::Conversation, "conversation"),
    (AuditTargetKind::Message, "message"),
    (AuditTargetKind::Room, "room"),
    (AuditTargetKind::RoomMember, "room_member"),
    (AuditTargetKind::Media, "media"),
    (AuditTargetKind::Report, "report"),
    (AuditTargetKind::LedgerAccount, "ledger_account"),
    (AuditTargetKind::Transaction, "transaction"),
    (AuditTargetKind::Bot, "bot"),
    (AuditTargetKind::Node, "node"),
    (AuditTargetKind::IdentityKey, "identity_key"),
    (AuditTargetKind::Wallet, "wallet"),
];

/// The name of one audit target kind.
fn target_kind_name(kind: AuditTargetKind) -> &'static str {
    AUDIT_TARGET_KINDS
        .iter()
        .find(|(candidate, _)| *candidate == kind)
        .map_or("unknown", |(_, name)| *name)
}

/// The kind one of those names refers to, or `None` for a name this build does not know.
fn target_kind_named(name: &str) -> Option<AuditTargetKind> {
    AUDIT_TARGET_KINDS
        .iter()
        .find(|(_, candidate)| *candidate == name)
        .map(|(kind, _)| *kind)
}

/// The name of one audit actor kind.
fn actor_kind_name(kind: i16) -> &'static str {
    match AuditActorKind::from_i16(kind) {
        Some(AuditActorKind::User) => "user",
        Some(AuditActorKind::System) => "system",
        Some(AuditActorKind::Bot) => "bot",
        Some(AuditActorKind::Operator) => "operator",
        None => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store's numbering, which is not the wire's. A client that read a bot at 3 — where the
    /// *wire* puts it — would show a moderator a media report for every misbehaving integration,
    /// and the numbers would look entirely plausible while it did.
    #[test]
    fn the_subject_kind_names_are_the_stores_numbers_and_media_is_not_a_bot() {
        assert_eq!("user", subject_kind_name(0));
        assert_eq!("message", subject_kind_name(1));
        assert_eq!("room", subject_kind_name(2));
        assert_eq!("media", subject_kind_name(3));
        assert_eq!("bot", subject_kind_name(4));
        // A code from a newer build is admitted to be unknown rather than mislabelled.
        assert_eq!("unknown", subject_kind_name(5));
        assert_eq!("unknown", subject_kind_name(-1));
    }

    /// The action words a dashboard sends are the words the metric series already uses, pinned so
    /// that a rename in either place is caught here rather than by an operator whose ruling was
    /// refused as an unknown action.
    #[test]
    fn every_action_word_is_the_actions_own_label() {
        let pairs = [
            (
                "warn",
                Action::Warn {
                    account_id: Id::NIL,
                },
            ),
            (
                "suspend",
                Action::Suspend {
                    account_id: Id::NIL,
                    until: None,
                },
            ),
            (
                "reinstate",
                Action::Reinstate {
                    account_id: Id::NIL,
                },
            ),
            (
                "remove_message",
                Action::RemoveMessage {
                    conversation_id: Id::NIL,
                    message_id: Id::NIL,
                },
            ),
            ("remove_media", Action::RemoveMedia { media_id: Id::NIL }),
            ("archive_room", Action::ArchiveRoom { room_id: Id::NIL }),
            ("disable_bot", Action::DisableBot { bot_id: Id::NIL }),
        ];
        for (word, action) in pairs {
            assert_eq!(word, action.label(), "the word for {word} has drifted");
        }
    }

    /// Every audit target kind survives the round trip through its own name. One kind missing from
    /// the table would be a `target_kind_name` of "unknown" on every row of that kind, and a
    /// dashboard that could not ask for the trail it was just shown.
    #[test]
    fn every_audit_target_kind_round_trips_through_its_name() {
        assert_eq!(15, AUDIT_TARGET_KINDS.len());
        for code in 0..15 {
            let Some(kind) = AuditTargetKind::from_i16(code) else {
                panic!("the store defines 15 kinds and {code} is one of them");
            };
            let name = target_kind_name(kind);
            assert_ne!("unknown", name, "kind {code} has no name");
            assert_eq!(Some(kind), target_kind_named(name));
        }
        assert_eq!(None, target_kind_named("nonsense"));
    }

    /// The power names are the four the crate declares, and an empty set names none of them — the
    /// answer an ordinary account gets.
    #[test]
    fn the_power_names_are_the_four_the_crate_declares() {
        assert_eq!(
            vec!["triage", "takedown", "suspend", "audit"],
            power_names(Powers::ALL)
        );
        assert_eq!(
            vec!["triage", "takedown"],
            power_names(Powers::TRIAGE.with(Powers::TAKEDOWN))
        );
        assert!(power_names(Powers::NONE).is_empty());
    }

    /// The actor kind names cover the four the store defines and admit to anything else.
    #[test]
    fn the_actor_kind_names_cover_the_stores_four() {
        assert_eq!("user", actor_kind_name(0));
        assert_eq!("system", actor_kind_name(1));
        assert_eq!("bot", actor_kind_name(2));
        assert_eq!("operator", actor_kind_name(3));
        assert_eq!("unknown", actor_kind_name(4));
    }

    /// The tag serde reads, so the shapes in this module's docs and the shapes a dashboard sends
    /// are the same shapes. A tagged enum that silently accepted `{"action": "suspend"}` without
    /// its id would suspend nobody and answer as though it had.
    #[test]
    fn an_action_body_parses_from_the_tagged_form() {
        let account = Id::from_bytes([3u8; 16]);
        let body: ActionBody = serde_json::from_value(serde_json::json!({
            "action": "suspend",
            "account_id": account,
        }))
        .expect("a suspend names an account and nothing else");
        assert_eq!(
            (
                Action::Suspend {
                    account_id: account,
                    until: None
                },
                None
            ),
            body.into_parts()
        );

        // The operator's own words ride along with the action they explain.
        let with_reason: ActionBody = serde_json::from_value(serde_json::json!({
            "action": "remove_message",
            "conversation_id": account,
            "message_id": account,
            "reason": "doxxing",
        }))
        .expect("a takedown names both halves of the key");
        assert_eq!(
            (
                Action::RemoveMessage {
                    conversation_id: account,
                    message_id: account
                },
                Some("doxxing".to_owned())
            ),
            with_reason.into_parts()
        );

        let missing_id =
            serde_json::from_value::<ActionBody>(serde_json::json!({"action": "warn"}));
        assert!(missing_id.is_err(), "a warn without a target is not a warn");

        let unknown = serde_json::from_value::<ActionBody>(serde_json::json!({"action": "banish"}));
        assert!(
            unknown.is_err(),
            "an unknown action is refused, not ignored"
        );
    }
}
