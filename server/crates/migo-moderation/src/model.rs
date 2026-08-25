//! Types the moderation service takes and returns.
//!
//! # Why none of these are protocol structs
//!
//! Brief section 145 reserves opcodes 192 to 194 — `REPORT_CREATE`,
//! `MODERATION_ACTION`, `MODERATION_EVENT` — and leaves the block at `STATUS: SPEC`.
//! None of them is in the generated packet registry, so there is no `ReportCreate` wire
//! struct to accept and no `ModerationEvent` to publish. These types exist instead and
//! the API layer maps them, exactly as the social and media crates do for their own
//! reserved blocks. Adding three frames to the IDL from a domain crate would change the
//! protocol's golden vectors, which is not a thing one feature's author gets to do on
//! the way past.
//!
//! # The two callers
//!
//! [`Caller`] is a user filing a report. [`Operator`] is a member of staff acting on
//! one. They are separate types rather than one type with a flag, because every method
//! on the service takes exactly one of them and the compiler should be the thing that
//! notices when a user reaches an operator path — not a runtime check somebody can
//! forget to write.
//!
//! # Where the numbers come from
//!
//! `report.subject_kind` is numbered in `migo_store::model::report_subject`, beside the
//! column and the index that is keyed on it. `report.status` likewise. [`Reason`] and
//! [`Resolution`] are numbered here, because the `reason` and `resolution` columns are
//! `smallint` with no meaning attached in SQL and nothing in the store reads them: the
//! store writes the number it is given and hands it back. This crate is where they mean
//! something, so this is where they are defined.

use migo_core::{Id, Timestamp};
use migo_ratelimit::TrustTier;
use migo_store::model::{report_subject, AuditTargetKind, Report};

/// Longest reporter note accepted.
///
/// Five hundred characters. Long enough for somebody to explain what happened, short
/// enough that the field cannot be used to store a document — and a note is read by a
/// human under time pressure, so a limit that keeps it readable is a feature.
pub const MAX_NOTE_LEN: usize = 500;

/// Longest operator reason accepted.
///
/// Two hundred and fifty. An operator reason is written for the audit trail and for the
/// next operator who reads it, and both are served better by one sentence than by five.
pub const MAX_REASON_LEN: usize = 250;

/// Largest queue page any listing here will return.
pub const MAX_PAGE: u16 = 200;

/// Queue page size for a caller that named none.
pub const DEFAULT_PAGE: u16 = 50;

/// How far back [`abuse pressure`](Signals::reports_against) is counted.
///
/// Seven days. Long enough that a slow-burning pattern shows up, short enough that
/// somebody who was reported once a year ago is not still being scored for it.
pub const REPORT_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// What a report is about.
///
/// A closed enum over `report.subject_kind`, and the reason the service can decide what
/// a report *means* without a second lookup. The variants carry only ids: a report is a
/// pointer to something a moderator will go and read, never a copy of it. Brief section
/// 162 puts it in the schema comment — copying private ciphertext into a moderation
/// table would defeat the point of encrypting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// A whole account.
    User(Id),
    /// One message.
    ///
    /// Carries the conversation as well as the message, because the column cannot: a
    /// report stores one `subject_id`, and reading a message needs both halves of its
    /// key. The conversation is kept in memory for the length of the request and is not
    /// written to the report row.
    Message {
        /// Which conversation it is in.
        conversation_id: Id,
        /// Which message.
        message_id: Id,
    },
    /// A room.
    Room(Id),
    /// A media object.
    Media(Id),
    /// A bot, by `bot.bot_id`.
    Bot(Id),
}

impl Subject {
    /// The `report.subject_kind` value for this subject.
    #[must_use]
    pub const fn kind(self) -> i16 {
        match self {
            Self::User(_) => report_subject::USER,
            Self::Message { .. } => report_subject::MESSAGE,
            Self::Room(_) => report_subject::ROOM,
            Self::Media(_) => report_subject::MEDIA,
            Self::Bot(_) => report_subject::BOT,
        }
    }

    /// The `report.subject_id` value for this subject.
    #[must_use]
    pub const fn id(self) -> Id {
        match self {
            Self::User(id) | Self::Room(id) | Self::Media(id) | Self::Bot(id) => id,
            Self::Message { message_id, .. } => message_id,
        }
    }

    /// What kind of thing the audit trail should say was acted on.
    #[must_use]
    pub const fn target_kind(self) -> AuditTargetKind {
        match self {
            Self::User(_) => AuditTargetKind::Account,
            Self::Message { .. } => AuditTargetKind::Message,
            Self::Room(_) => AuditTargetKind::Room,
            Self::Media(_) => AuditTargetKind::Media,
            Self::Bot(_) => AuditTargetKind::Bot,
        }
    }

    /// A short, stable label for a metric.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Message { .. } => "message",
            Self::Room(_) => "room",
            Self::Media(_) => "media",
            Self::Bot(_) => "bot",
        }
    }

    /// Whether either id in this subject is missing.
    #[must_use]
    pub fn is_nil(self) -> bool {
        match self {
            Self::User(id) | Self::Room(id) | Self::Media(id) | Self::Bot(id) => id.is_nil(),
            Self::Message {
                conversation_id,
                message_id,
            } => conversation_id.is_nil() || message_id.is_nil(),
        }
    }
}

/// Why somebody is reporting something.
///
/// The numbers are a wire contract: they go into `report.reason` and a client renders
/// them back into a sentence in the user's own language. Appending is safe; renumbering
/// is not, and would relabel every report already filed.
///
/// The list is the union of brief section 49's automated-detection categories — spam,
/// flood, scam, malicious links, abusive behaviour, bot abuse — and the categories a
/// person actually reaches for when they press the report button. Both, because the
/// same column carries a report filed by a human and one filed by the detector, and two
/// numbering schemes over one column is how a dashboard ends up counting spam twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reason {
    /// Unsolicited bulk content.
    #[default]
    Spam = 0,
    /// Volume rather than content: the same thing, very fast.
    Flood = 1,
    /// An attempt to obtain money or credentials by deception.
    Scam = 2,
    /// A link to malware, phishing, or a credential harvester.
    MaliciousLink = 3,
    /// Harassment, threats, or targeted abuse of a person.
    Harassment = 4,
    /// Hateful content aimed at a group.
    HateSpeech = 5,
    /// Sexual content where it does not belong.
    SexualContent = 6,
    /// Graphic violence.
    Violence = 7,
    /// Self-harm or suicide content.
    ///
    /// Routed like any other report and prioritised like none of them. This crate can
    /// only carry the code; a deployment that does not put a human on this queue within
    /// minutes has a product problem that no amount of software here will fix.
    SelfHarm = 8,
    /// Somebody pretending to be somebody else.
    Impersonation = 9,
    /// Child sexual abuse material.
    ///
    /// Kept as its own code and never folded into [`Reason::SexualContent`], because
    /// the legal obligations attached to it are not the same and an operator must be
    /// able to filter the queue for exactly this.
    ChildSafety = 10,
    /// A bot misbehaving: spamming, ignoring its scopes, or acting for somebody else.
    BotAbuse = 11,
    /// None of the above.
    ///
    /// Last, and deliberately unhelpful to a triager, which is the point: a report that
    /// lands here is one the reporter could not classify, and the note is the only thing
    /// that will explain it.
    Other = 12,
}

impl Reason {
    /// Every reason, in numeric order.
    pub const ALL: [Self; 13] = [
        Self::Spam,
        Self::Flood,
        Self::Scam,
        Self::MaliciousLink,
        Self::Harassment,
        Self::HateSpeech,
        Self::SexualContent,
        Self::Violence,
        Self::SelfHarm,
        Self::Impersonation,
        Self::ChildSafety,
        Self::BotAbuse,
        Self::Other,
    ];

    /// The stored value.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Reads a stored value.
    ///
    /// An unknown number becomes [`Reason::Other`] rather than an error. A row written
    /// by a newer build must still be readable by an older one, and a moderator seeing
    /// "other" for a category their binary does not know is strictly better than a
    /// queue that will not load.
    #[must_use]
    pub fn of_i16(value: i16) -> Self {
        Self::ALL
            .into_iter()
            .find(|reason| reason.to_i16() == value)
            .unwrap_or(Self::Other)
    }

    /// A short, stable label for a metric.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Flood => "flood",
            Self::Scam => "scam",
            Self::MaliciousLink => "malicious_link",
            Self::Harassment => "harassment",
            Self::HateSpeech => "hate_speech",
            Self::SexualContent => "sexual_content",
            Self::Violence => "violence",
            Self::SelfHarm => "self_harm",
            Self::Impersonation => "impersonation",
            Self::ChildSafety => "child_safety",
            Self::BotAbuse => "bot_abuse",
            Self::Other => "other",
        }
    }

    /// Position in [`Reason::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// What a moderator decided a report came to.
///
/// Written to `report.resolution`. [`Resolution::status`] maps each one onto the two
/// values `report.status` can take when a report closes, so the pair can never
/// disagree — an "actioned" report whose resolution says nothing was done is a
/// contradiction the queue would then show forever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resolution {
    /// Looked at; nothing was wrong.
    #[default]
    NoAction = 0,
    /// The subject was warned.
    Warned = 1,
    /// The reported content was removed.
    ContentRemoved = 2,
    /// The subject was suspended.
    Suspended = 3,
    /// The room was archived.
    RoomArchived = 4,
    /// Handed to somebody with more authority, or to somebody outside this system.
    ///
    /// Leaves the report *open*. An escalation that closed the report would lose the
    /// only record that anybody is still waiting for an answer.
    Escalated = 5,
    /// Filed in bad faith, or about nothing.
    ///
    /// Distinct from [`Resolution::NoAction`] because the two say different things about
    /// the *reporter*, and the difference is what makes a report-button abuser visible.
    Invalid = 6,
    /// A duplicate of a report already in the queue.
    Duplicate = 7,
}

impl Resolution {
    /// Every resolution, in numeric order.
    pub const ALL: [Self; 8] = [
        Self::NoAction,
        Self::Warned,
        Self::ContentRemoved,
        Self::Suspended,
        Self::RoomArchived,
        Self::Escalated,
        Self::Invalid,
        Self::Duplicate,
    ];

    /// The stored value.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Reads a stored value. An unknown number reads as [`Resolution::NoAction`].
    #[must_use]
    pub fn of_i16(value: i16) -> Self {
        Self::ALL
            .into_iter()
            .find(|resolution| resolution.to_i16() == value)
            .unwrap_or(Self::NoAction)
    }

    /// The `report.status` this resolution closes the report with.
    ///
    /// `None` for [`Resolution::Escalated`], which does not close it.
    #[must_use]
    pub const fn status(self) -> Option<i16> {
        use migo_store::model::report_status;
        match self {
            Self::Escalated => None,
            Self::NoAction | Self::Invalid | Self::Duplicate => Some(report_status::DISMISSED),
            Self::Warned | Self::ContentRemoved | Self::Suspended | Self::RoomArchived => {
                Some(report_status::ACTIONED)
            }
        }
    }

    /// A short, stable label for a metric.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoAction => "no_action",
            Self::Warned => "warned",
            Self::ContentRemoved => "content_removed",
            Self::Suspended => "suspended",
            Self::RoomArchived => "room_archived",
            Self::Escalated => "escalated",
            Self::Invalid => "invalid",
            Self::Duplicate => "duplicate",
        }
    }

    /// Position in [`Resolution::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// What a member of staff is allowed to do.
///
/// A bitmask and not a rank, because the four powers are genuinely independent and a
/// ladder would force a deployment to grant the ones it did not want in order to reach
/// the one it did. Brief section 41 says a default identity gets the minimum permission;
/// [`Powers::NONE`] is that default here, and it is also what a bug that forgets to look
/// anybody up produces.
///
/// Not a role table. There is no global role column on `account` — `docs/04-data-model.md`
/// gives roles to room members and nobody else — so who is staff is a question this
/// crate asks and does not answer. See `crate::traits::Roster`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Powers(u32);

impl Powers {
    /// No powers at all. What an ordinary account has.
    pub const NONE: Self = Self(0);

    /// Read the queue, read one report, and resolve it.
    pub const TRIAGE: Self = Self(1 << 0);

    /// Remove content: a message, a media object, a room.
    pub const TAKEDOWN: Self = Self(1 << 1);

    /// Suspend and reinstate accounts.
    pub const SUSPEND: Self = Self(1 << 2);

    /// Read the audit trail.
    ///
    /// Separate from the rest, and the one power worth giving to somebody who cannot
    /// act: an auditor reviewing what moderators did should not need the ability to do
    /// it themselves.
    pub const AUDIT: Self = Self(1 << 3);

    /// Every power. For a single-operator deployment and for tests.
    pub const ALL: Self = Self(0b1111);

    /// Builds from a raw bitmask.
    ///
    /// Bits above the four defined ones are dropped rather than kept, so a value from a
    /// newer build cannot grant a power this one has never heard of.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// The raw bitmask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether every power in `other` is present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether there are no powers at all.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The union of two sets.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Somebody filing a report.
///
/// No `powers` field and no `reauthenticated` flag. Filing a report is something every
/// account may do and nothing that needs a second factor — a report is a request for a
/// human to look at something, and making it harder to file is how abuse goes
/// unreported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
    /// The reporter's network class, already truncated.
    ///
    /// A class and never an address. Brief section 174 forbids a full IP in a log, and
    /// the audit table is a log that is kept for longer than most: `migo_ratelimit`'s
    /// `network` function is what produces the value this field wants, and it is the
    /// gateway's job to call it, because the gateway is the only layer that has ever
    /// seen the address.
    pub ip_class: Option<String>,
}

impl Caller {
    /// A caller at `now`.
    #[must_use]
    pub fn new(account_id: Id, device_id: Id, tier: TrustTier, now: Timestamp) -> Self {
        Self {
            account_id,
            device_id,
            tier,
            now,
            request_id: None,
            ip_class: None,
        }
    }

    /// Sets the correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Sets the truncated network class.
    #[must_use]
    pub fn from_network(mut self, ip_class: impl Into<String>) -> Self {
        self.ip_class = Some(ip_class.into());
        self
    }
}

/// A member of staff acting on a report.
///
/// # Why the powers are on the struct and not looked up here
///
/// They are looked up — by the service, through `crate::traits::Roster`, on every call.
/// This field is what that lookup produced, and it exists so that the service can hand
/// a resolved operator to its own internals without asking twice in one request.
///
/// It is deliberately *not* something a client sends. An `Operator` is minted by the
/// composition root from an authenticated session, in the same way `tier` and
/// `reauthenticated` are, and `crate::traits::Warden`'s methods take an account id and
/// resolve the powers themselves for exactly this reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operator {
    /// The staff account.
    pub account_id: Id,
    /// The connection the request arrived on.
    pub device_id: Id,
    /// What this account may do, as resolved for this request.
    pub powers: Powers,
    /// Whether this session proved a factor recently.
    ///
    /// Every action requires it; no read does. A stolen operator session is the worst
    /// credential in this system — it can suspend accounts and delete other people's
    /// content — and the brief's own precedent for the pattern is section 85, which
    /// requires a fresh factor before a room changes hands. If it is worth asking for
    /// before somebody gives away a room, it is worth asking for before somebody
    /// suspends a stranger.
    pub reauthenticated: bool,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id.
    pub request_id: Option<String>,
    /// The operator's network class, already truncated.
    pub ip_class: Option<String>,
}

impl Operator {
    /// An operator who has not proved a second factor recently.
    ///
    /// The default, because the failure of forgetting to set the flag should be a
    /// refused suspension rather than an accepted one.
    #[must_use]
    pub fn new(account_id: Id, device_id: Id, powers: Powers, now: Timestamp) -> Self {
        Self {
            account_id,
            device_id,
            powers,
            reauthenticated: false,
            now,
            request_id: None,
            ip_class: None,
        }
    }

    /// Marks the session as having proved a factor within the gateway's window.
    #[must_use]
    pub fn reauthenticated(mut self) -> Self {
        self.reauthenticated = true;
        self
    }

    /// Sets the correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Sets the truncated network class.
    #[must_use]
    pub fn from_network(mut self, ip_class: impl Into<String>) -> Self {
        self.ip_class = Some(ip_class.into());
        self
    }
}

/// A report somebody is filing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filing {
    /// What is being reported.
    pub subject: Subject,
    /// Why.
    pub reason: Reason,
    /// The reporter's own words, if they wrote any.
    pub note: Option<String>,
    /// The room this happened in, when it happened in one.
    ///
    /// Context for the triager, and the thing that lets a room's own moderators be
    /// shown their own reports without a scan of every report ever filed.
    pub room_id: Option<Id>,
    /// A pointer to evidence.
    ///
    /// An id, never a copy. The schema comment on the column says it and means it:
    /// copying private ciphertext into a moderation table would defeat the point of
    /// encrypting it, and copying plaintext there is not possible because the server
    /// does not have any.
    pub evidence_ref: Option<Id>,
}

impl Filing {
    /// A report about `subject` for `reason`.
    #[must_use]
    pub const fn new(subject: Subject, reason: Reason) -> Self {
        Self {
            subject,
            reason,
            note: None,
            room_id: None,
            evidence_ref: None,
        }
    }

    /// Attaches the reporter's note.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Records the room this happened in.
    #[must_use]
    pub const fn in_room(mut self, room_id: Id) -> Self {
        self.room_id = Some(room_id);
        self
    }

    /// Points at evidence.
    #[must_use]
    pub const fn with_evidence(mut self, evidence_ref: Id) -> Self {
        self.evidence_ref = Some(evidence_ref);
        self
    }
}

/// What filing a report did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filed {
    /// The report.
    pub report_id: Id,
    /// Whether this reporter already had an open report about this subject.
    ///
    /// `true` means nothing new was written and `report_id` names the report that was
    /// already there. Brief section 153 makes this an outcome rather than an error: the
    /// client that filed twice is usually a client whose first answer was lost, and
    /// telling it the report failed would be a lie about a report sitting in the queue.
    pub duplicate: bool,
}

/// A report, as a triager sees it.
///
/// A projection of the stored row rather than the row itself, so that `reason` and
/// `resolution` arrive as enums instead of as numbers a caller has to look up, and so
/// that adding a column to the table is not automatically a change to this crate's
/// public API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    /// Primary key.
    pub report_id: Id,
    /// Who filed it.
    pub reporter_id: Id,
    /// What it is about.
    pub subject_kind: i16,
    /// Which one.
    pub subject_id: Id,
    /// Room context.
    pub room_id: Option<Id>,
    /// Why.
    pub reason: Reason,
    /// The reporter's words.
    pub note: Option<String>,
    /// A pointer to evidence.
    pub evidence_ref: Option<Id>,
    /// `report.status`.
    pub status: i16,
    /// When it was filed.
    pub created_at: Timestamp,
    /// When it was closed.
    pub resolved_at: Option<Timestamp>,
    /// Who closed it.
    pub resolved_by: Option<Id>,
    /// What it came to.
    pub resolution: Option<Resolution>,
}

impl Case {
    /// Projects a stored row.
    #[must_use]
    pub fn of(row: Report) -> Self {
        Self {
            report_id: row.report_id,
            reporter_id: row.reporter_id,
            subject_kind: row.subject_kind,
            subject_id: row.subject_id,
            room_id: row.room_id,
            reason: Reason::of_i16(row.reason),
            note: row.note,
            evidence_ref: row.evidence_ref,
            status: row.status,
            created_at: row.created_at,
            resolved_at: row.resolved_at,
            resolved_by: row.resolved_by,
            resolution: row.resolution.map(Resolution::of_i16),
        }
    }

    /// Whether this report is still waiting for a decision.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.status == migo_store::model::report_status::OPEN
    }
}

/// Something a member of staff is doing about a subject.
///
/// # What is here, and what is somewhere else
///
/// Every variant maps onto a method the store already has, which is what keeps this
/// crate from needing a table of its own. A warning is an audit row and nothing else; a
/// suspension is `account.status`; a takedown is the tombstone or scan column that the
/// serving path already reads.
///
/// **Room bans and room mutes are not here.** They belong to `migo_rooms`, which owns
/// `set_room_sanction` and the permission model that decides who may call it. A second
/// crate writing that column would be two owners of one rule, and the first time they
/// disagreed a ban would be lifted by the code that did not know about it.
///
/// **Disabling a bot lands on `bot.disabled_at`, through the store's `BotStore` trait.**
/// `DisableBot` is a takedown taken against the bot itself rather than its owner's
/// account: the bot's token stops authenticating at once, while the row and its backing
/// account survive so the bot can be re-enabled and its history stays readable. It writes
/// the same column as the owner's own pause control in `migo-bots`, but the two are
/// different authorities — a moderator's takedown and an owner's switch — reaching one bit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Tells somebody their behaviour was looked at and found wanting.
    ///
    /// Writes an audit row and notifies the account. There is no warnings table and
    /// there does not need to be one: brief section 49's dashboard lists warnings beside
    /// audit logs, and a warning *is* an audit entry — `audit_for_target` on the account
    /// is the warning history, already indexed, already ordered newest first.
    Warn {
        /// Who is being warned.
        account_id: Id,
    },
    /// Suspends an account, optionally until a date.
    ///
    /// `until: None` is indefinite, which is the shape `account.suspended_until` already
    /// has. An indefinite suspension is a decision to make deliberately, so the API
    /// makes it explicit rather than defaulting to it.
    Suspend {
        /// Who is being suspended.
        account_id: Id,
        /// When it lifts by itself.
        until: Option<Timestamp>,
    },
    /// Returns an account to normal.
    ///
    /// Clears the expiry as well as the status, because a reinstatement that left
    /// `suspended_until` set would leave a date in the row that nothing reads and every
    /// future reader misinterprets.
    Reinstate {
        /// Who is being reinstated.
        account_id: Id,
    },
    /// Removes one message.
    ///
    /// Both halves of the key, because that is what the store needs and because a report
    /// row cannot hold both. The tombstone is what the messaging layer already
    /// understands; nothing here reads the envelope, and for an end-to-end conversation
    /// nothing could.
    RemoveMessage {
        /// Which conversation.
        conversation_id: Id,
        /// Which message.
        message_id: Id,
    },
    /// Takes a media object down.
    ///
    /// Marks it rejected and then tombstones it, in that order. The order is the whole
    /// point: the scan column is what the media crate consults before it signs a
    /// download URL, so marking it first means a crash between the two steps leaves an
    /// object that is unservable rather than one that is servable and gone.
    RemoveMedia {
        /// Which object.
        media_id: Id,
    },
    /// Archives a room.
    ///
    /// Not a delete. Links and history keep resolving, which is what the store's own
    /// comment on `archive_room` promises, and what makes the action reviewable
    /// afterwards instead of unfalsifiable.
    ArchiveRoom {
        /// Which room.
        room_id: Id,
    },
    /// Disables a bot.
    ///
    /// A takedown against the bot rather than its owner: `bot.disabled_at` is set, the
    /// bot's token stops authenticating at once, and the row and backing account survive
    /// so the bot can be re-enabled and its history stays readable. This is the
    /// moderator's counterpart to the owner's own pause in `migo-bots`; both land on the
    /// same column.
    DisableBot {
        /// Which bot.
        bot_id: Id,
    },
}

impl Action {
    /// The power required to take this action.
    #[must_use]
    pub const fn requires(&self) -> Powers {
        match self {
            // A warning changes nothing but the record, so it rides with triage.
            Self::Warn { .. } => Powers::TRIAGE,
            Self::Suspend { .. } | Self::Reinstate { .. } => Powers::SUSPEND,
            Self::RemoveMessage { .. }
            | Self::RemoveMedia { .. }
            | Self::ArchiveRoom { .. }
            | Self::DisableBot { .. } => Powers::TAKEDOWN,
        }
    }

    /// The stable action name written to `audit_entry.action`.
    ///
    /// Dotted, lowercase, and never reworded once shipped: these strings are what an
    /// operator greps for and what a compliance export groups by, so they are part of
    /// the contract in the same way an error code is.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Warn { .. } => "moderation.account.warn",
            Self::Suspend { .. } => "moderation.account.suspend",
            Self::Reinstate { .. } => "moderation.account.reinstate",
            Self::RemoveMessage { .. } => "moderation.message.remove",
            Self::RemoveMedia { .. } => "moderation.media.remove",
            Self::ArchiveRoom { .. } => "moderation.room.archive",
            Self::DisableBot { .. } => "moderation.bot.disable",
        }
    }

    /// A short, stable label for a metric.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Warn { .. } => "warn",
            Self::Suspend { .. } => "suspend",
            Self::Reinstate { .. } => "reinstate",
            Self::RemoveMessage { .. } => "remove_message",
            Self::RemoveMedia { .. } => "remove_media",
            Self::ArchiveRoom { .. } => "archive_room",
            Self::DisableBot { .. } => "disable_bot",
        }
    }

    /// Position in the metric's label set.
    #[must_use]
    pub const fn index(&self) -> usize {
        match self {
            Self::Warn { .. } => 0,
            Self::Suspend { .. } => 1,
            Self::Reinstate { .. } => 2,
            Self::RemoveMessage { .. } => 3,
            Self::RemoveMedia { .. } => 4,
            Self::ArchiveRoom { .. } => 5,
            Self::DisableBot { .. } => 6,
        }
    }

    /// What kind of thing the audit trail should say was acted on.
    #[must_use]
    pub const fn target_kind(&self) -> AuditTargetKind {
        match self {
            Self::Warn { .. } | Self::Suspend { .. } | Self::Reinstate { .. } => {
                AuditTargetKind::Account
            }
            Self::RemoveMessage { .. } => AuditTargetKind::Message,
            Self::RemoveMedia { .. } => AuditTargetKind::Media,
            Self::ArchiveRoom { .. } => AuditTargetKind::Room,
            Self::DisableBot { .. } => AuditTargetKind::Bot,
        }
    }

    /// Which row the audit entry points at.
    #[must_use]
    pub const fn target_id(&self) -> Id {
        match self {
            Self::Warn { account_id }
            | Self::Suspend { account_id, .. }
            | Self::Reinstate { account_id } => *account_id,
            Self::RemoveMessage { message_id, .. } => *message_id,
            Self::RemoveMedia { media_id } => *media_id,
            Self::ArchiveRoom { room_id } => *room_id,
            Self::DisableBot { bot_id } => *bot_id,
        }
    }

    /// The account this action is about, when it is about an account.
    ///
    /// `None` for a takedown: removing a message tells this crate nothing about who
    /// wrote it, and guessing would be a notification sent to the wrong person.
    #[must_use]
    pub const fn subject_account(&self) -> Option<Id> {
        match self {
            Self::Warn { account_id }
            | Self::Suspend { account_id, .. }
            | Self::Reinstate { account_id } => Some(*account_id),
            _ => None,
        }
    }
}

/// Metadata a deployment has counted about one account.
///
/// # Why every field is a number and none of them is content
///
/// Brief section 49 asks for automated detection of spam, flood, scam, malicious links,
/// abusive behaviour, and bot abuse. Three of those six cannot be detected on this
/// server at all: a scam, a malicious link, and abusive behaviour live inside a message
/// body, and for a private conversation the server holds ciphertext. Section 122 says so
/// outright — validation of content moves to the client.
///
/// What is left is rate and shape, and that is what this struct is. Nothing here is
/// derived from a message body, so nothing here becomes wrong when the body is
/// encrypted, and nothing here would tempt a future author into reading one.
///
/// # Where the numbers come from
///
/// The caller. Every field is something the gateway already counts to charge the rate
/// limiter, and a scorer that read them out of its own store would be a second set of
/// counters that disagrees with the first. The one field this crate fills in itself is
/// [`Signals::reports_against`], because that one is a query against a table it owns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    /// Messages sent in the last minute.
    pub messages_last_minute: u32,
    /// Distinct conversations written to in the last hour.
    ///
    /// The fan-out signal, and the one that separates a chatty person from a broadcaster.
    /// Somebody sending two hundred messages to one friend is having a conversation;
    /// somebody sending two hundred messages to two hundred strangers is not.
    pub recipients_last_hour: u32,
    /// Rooms joined in the last hour.
    pub rooms_joined_last_hour: u32,
    /// Friend requests sent in the last hour.
    pub friend_requests_last_hour: u32,
    /// Requests the rate limiter refused in the last hour.
    ///
    /// Being refused is not misconduct. Being refused four hundred times is a client
    /// that is not backing off, which is either a bug worth finding or a script.
    pub refusals_last_hour: u32,
    /// How many reports were filed about this account in [`REPORT_WINDOW_MS`].
    ///
    /// Filled in by the service, not by the caller.
    pub reports_against: u32,
    /// How old the account is, in milliseconds.
    ///
    /// Not a suspicion by itself. It is a multiplier: an account that has existed for
    /// two years and suddenly sends four hundred messages a minute has probably been
    /// stolen, and an account that does it twenty minutes after signing up was made for
    /// it.
    pub account_age_ms: i64,
}

/// How much of a concern an account is.
///
/// Four levels and not a raw number, because the number is a policy artefact that will
/// be retuned and the level is what the rest of the system branches on. A caller that
/// switched on the score would have the thresholds copied into it, and then a retune
/// here would silently stop matching the behaviour there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Risk {
    /// Nothing unusual.
    #[default]
    Clear = 0,
    /// Worth counting, not worth acting on.
    Watch = 1,
    /// Give this account a smaller budget.
    ///
    /// This is brief section 50's adaptive rate limiting, and it is the only automated
    /// consequence this crate will apply by itself. A smaller budget slows an abuser and
    /// inconveniences a false positive, which is the right way round for a decision made
    /// by arithmetic.
    Throttle = 2,
    /// A human should look at this account now.
    ///
    /// Still not an automatic suspension unless a deployment turns one on. See
    /// [`ModerationConfig::auto_suspend`].
    Restrict = 3,
}

impl Risk {
    /// Every level, in numeric order.
    pub const ALL: [Self; 4] = [Self::Clear, Self::Watch, Self::Throttle, Self::Restrict];

    /// A short, stable label for a metric.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Watch => "watch",
            Self::Throttle => "throttle",
            Self::Restrict => "restrict",
        }
    }

    /// Position in [`Risk::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The tier this level allows, given the tier the session would otherwise have.
    ///
    /// Only ever downwards. A scorer that could *raise* a tier would be a way to earn
    /// trust by behaving in a pattern, and the patterns are published in this file.
    #[must_use]
    pub const fn clamp(self, tier: TrustTier) -> TrustTier {
        match self {
            Self::Clear | Self::Watch => tier,
            // A bot keeps its tier under throttling: bot budgets are set by brief
            // section 70 for machine traffic, and demoting a bot to `New` would rate
            // limit an integration into uselessness for sending the volume it exists to
            // send. A bot that is genuinely abusive is a `Restrict`, handled below.
            Self::Throttle => match tier {
                TrustTier::Bot => TrustTier::Bot,
                TrustTier::Trusted | TrustTier::Established => TrustTier::New,
                other => other,
            },
            Self::Restrict => TrustTier::Anonymous,
        }
    }
}

/// What the scorer concluded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Assessment {
    /// The account looked at.
    pub account_id: Id,
    /// The weighted score, zero upwards.
    ///
    /// Published because an operator asking "why is this account throttled" deserves an
    /// answer, and suppressed from every metric label because a score attached to an
    /// account id is a dossier.
    pub score: u32,
    /// What to do about it.
    pub risk: Risk,
}

/// What the service needs that only deployment knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModerationConfig {
    /// Queue page size when a caller names none.
    pub page: u16,
    /// Score at or above which an account is watched.
    pub watch_at: u32,
    /// Score at or above which an account is throttled.
    pub throttle_at: u32,
    /// Score at or above which a human is wanted.
    pub restrict_at: u32,
    /// Whether [`Risk::Restrict`] suspends the account without asking anybody.
    ///
    /// `false`, and the default matters more than the flag. An automated system that
    /// suspends accounts on a metadata score will suspend somebody's account on a
    /// metadata score — a person on a shared connection during a group chat, a client
    /// with a retry bug, somebody who was mass-reported for winning an argument. The
    /// safe default is to make the queue loud, not to make the ban automatic.
    ///
    /// When it is on, the suspension is written with `AuditActorKind::System` and a
    /// fixed duration, never indefinitely: an automatic decision that never expires is
    /// an automatic decision nobody will ever revisit.
    pub auto_suspend: bool,
    /// How long an automatic suspension lasts.
    pub auto_suspend_ms: i64,
}

impl Default for ModerationConfig {
    fn default() -> Self {
        Self {
            page: DEFAULT_PAGE,
            watch_at: 30,
            throttle_at: 60,
            restrict_at: 100,
            auto_suspend: false,
            auto_suspend_ms: 24 * 60 * 60 * 1_000,
        }
    }
}

/// Scores one account's metadata.
///
/// A free function, pure, and deliberately readable end to end: this is the arithmetic
/// that decides whether somebody's app gets slower, and it should be possible to argue
/// with it by reading it rather than by instrumenting it.
///
/// The weights say, in order: sending faster than a person can type is the strongest
/// single signal; writing to many strangers at once is the next; other people
/// complaining is worth more than anything the server noticed by itself, because it is
/// the only signal here that comes from a human; and being refused repeatedly without
/// backing off is a client that is not behaving like a client.
///
/// A young account doubles the total. Not because new accounts are suspicious, but
/// because the cost of being wrong is not symmetric: a throttled two-year-old account
/// belongs to somebody with a history worth protecting, and a throttled twenty-minute-old
/// account costs an attacker the twenty minutes.
#[must_use]
pub fn score(signals: &Signals, config: &ModerationConfig) -> u32 {
    /// Messages a minute past which a human is not typing them.
    const TYPING_CEILING: u32 = 30;
    /// Distinct conversations an hour past which this is a broadcast.
    const FANOUT_CEILING: u32 = 40;
    /// Rooms an hour past which this is a crawler.
    const JOIN_CEILING: u32 = 20;
    /// Friend requests an hour past which this is an address-book scrape.
    const REQUEST_CEILING: u32 = 30;
    /// Refusals an hour past which the client is not backing off.
    const REFUSAL_CEILING: u32 = 50;
    /// Below this age an account is young. Twenty-four hours.
    const YOUNG_MS: i64 = 24 * 60 * 60 * 1_000;

    let over = |value: u32, ceiling: u32, weight: u32| -> u32 {
        value.saturating_sub(ceiling).saturating_mul(weight)
    };

    let raw = over(signals.messages_last_minute, TYPING_CEILING, 3)
        .saturating_add(over(signals.recipients_last_hour, FANOUT_CEILING, 2))
        .saturating_add(over(signals.rooms_joined_last_hour, JOIN_CEILING, 2))
        .saturating_add(over(signals.friend_requests_last_hour, REQUEST_CEILING, 2))
        .saturating_add(over(signals.refusals_last_hour, REFUSAL_CEILING, 1))
        // No ceiling, and the heaviest weight in the function. Every unit of this came
        // from a person pressing a button, which is a different kind of evidence from a
        // counter the server kept about itself.
        .saturating_add(signals.reports_against.saturating_mul(15));

    let scored = if signals.account_age_ms < YOUNG_MS {
        raw.saturating_mul(2)
    } else {
        raw
    };
    // Clamped to just past the top threshold. The number is for a human to read, and
    // "one hundred and four" and "nine million" mean the same thing to the decision.
    scored.min(config.restrict_at.saturating_mul(2))
}

/// Turns a score into a level.
#[must_use]
pub fn risk_of(score: u32, config: &ModerationConfig) -> Risk {
    // Descending, so a misconfiguration where the thresholds are out of order produces
    // the stricter answer rather than the looser one.
    if score >= config.restrict_at {
        Risk::Restrict
    } else if score >= config.throttle_at {
        Risk::Throttle
    } else if score >= config.watch_at {
        Risk::Watch
    } else {
        Risk::Clear
    }
}

/// Whether a note or reason is short enough and says anything.
#[must_use]
pub fn text_is_usable(text: &str, max: usize) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= max
}
