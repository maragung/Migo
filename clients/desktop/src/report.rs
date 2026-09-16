//! The moderation vocabulary this client files reports with.
//!
//! Section 49 of the brief asks for four kinds of report — a user, a message, a room, a bot — and
//! that is exactly what [`ReportSubject`] carries. Filing one is the only thing a *client* can do
//! here, and deliberately so: reading the queue, ruling on a case and applying a takedown are staff
//! powers whose surface is the node's operator API, not this one. A client that could read the
//! queue would be a client that could read who reported whom.
//!
//! # A report is a pointer, never a copy
//!
//! The wire carries a subject kind, a subject id, a reason code and an optional note the reporter
//! wrote. It carries no message content and no attachment, and that is not a shortcut: the
//! conversations this system carries are end-to-end encrypted, so a report that quoted the
//! offending text would either be unreadable to the moderator or readable only by shipping the
//! server a plaintext it is not supposed to have. The moderator follows the pointer with their own
//! eyes.
//!
//! The corollary is [`ReportTarget::label`]: it is the reporter's own phrasing — "this message", a
//! display name, "this room" — and never quoted content, because a report surface that renders the
//! thing being reported is rendering it on a screen with no mute, no filter and no way to look
//! away.
//!
//! # Why the wire numbers and not the store's
//!
//! [`ReportSubject`] is the *wire* vocabulary. The node's own storage numbers a media object 3 and a
//! bot 4, while the wire numbers a bot 3 and has no media kind at all — so a client that copied the
//! storage numbering would file every bot report as a report about a media object, and nothing
//! would say so until a moderator opened the queue. The reason codes are no different: they are
//! stored exactly as given, so renumbering one here would silently rewrite the meaning of every
//! report already sitting in a queue.
//!
//! # Idempotent, and priced
//!
//! A second report from the same reporter about the same still-open subject does not create a
//! second row and does not fail — the node recognises it and answers success, because the usual
//! cause is a client whose first answer was lost and telling it the report failed would be a lie
//! about a report that is in fact sitting in the queue. Reporting yourself is refused as a client
//! bug rather than queued for a human to read, which is why no surface here offers the door on the
//! account's own messages or its own profile card.
//!
//! Filing is priced — the registry charges `REPORT_CREATE` 20 — so a surface offers the report once
//! per gesture rather than retrying a refused call in a loop.
//!
//! # Why the reply is a bare acknowledgement
//!
//! `REPORT_CREATE` answers with `Acknowledged`, not with the report's id. The node knows more than
//! it says — whether the filing was a duplicate, and which row it landed on — and keeping the reply
//! bare is deliberate: a reporter has no use for a case id (they cannot read the case), and echoing
//! one back would invite this client to present it as a receipt the reporter could chase. The one
//! thing a reporter legitimately needs — that the report arrived — is what the ack carries.

use migo_core::Id;

/// What is being reported, as the wire numbers it.
///
/// The values are the `subject_kind` field of `REPORT_CREATE` and are load-bearing: they are what
/// the node maps to its own storage vocabulary, so they are never renumbered or reused.
/// [`ReportSubject::Message`] is a message id and nothing more — the conversation it sits in is not
/// on the wire, and a report row stores exactly one id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportSubject {
    /// A whole account, by account id.
    User,
    /// One message, by message id.
    Message,
    /// A room, by room id.
    Room,
    /// A bot, by `bot.bot_id` rather than by the account it signs in as.
    ///
    /// Carried because the numbering is the wire's and a client that skipped the kind would
    /// leave a hole where the node reads one — but nothing here constructs it: this client
    /// has no bot surface, so there is no bot on any screen for a report to point at. The
    /// kind exists for the clients that grow one, and the value is pinned by a test either
    /// way.
    #[allow(dead_code)]
    Bot,
}

impl ReportSubject {
    /// The value `REPORT_CREATE.subject_kind` carries.
    pub const fn wire(self) -> u32 {
        match self {
            Self::User => 0,
            Self::Message => 1,
            Self::Room => 2,
            Self::Bot => 3,
        }
    }
}

/// Why something is being reported.
///
/// The codes mirror the node's own reason vocabulary and are stored as given, so they are never
/// renumbered.
///
/// The four that exist for a legal reason rather than a product one — [`ReportReason::ChildSafety`]
/// above all, kept separate from [`ReportReason::SexualContent`] because the obligations attached
/// to it are not the same and an operator must be able to filter the queue for exactly it — are the
/// reason this is a code and not a free-text field.
///
/// [`ReportReason::SelfHarm`] is routed like any other report and prioritised like none of them:
/// this client carries the code, and what a deployment does with it afterwards is a staffing
/// question no amount of client code answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportReason {
    /// Unsolicited bulk content.
    Spam,
    /// Volume rather than content: the same thing, very fast.
    ///
    /// One of the four codes no surface here offers; see the note on [`Self::ChildSafety`].
    #[allow(dead_code)]
    Flood,
    /// An attempt to obtain money or credentials by deception.
    Scam,
    /// A link to malware, phishing, or a credential harvester.
    MaliciousLink,
    /// Harassment, threats, or targeted abuse of a person.
    Harassment,
    /// Hateful content aimed at a group.
    HateSpeech,
    /// Sexual content where it does not belong.
    SexualContent,
    /// Graphic violence.
    Violence,
    /// Self-harm or suicide content.
    ///
    /// One of the four codes no surface here offers; see the note on [`Self::ChildSafety`].
    #[allow(dead_code)]
    SelfHarm,
    /// Somebody pretending to be somebody else.
    Impersonation,
    /// Child sexual abuse material. Kept its own code; see the enum's own note.
    ///
    /// One of the four codes the sheet deliberately does not list, alongside
    /// [`Self::SelfHarm`], [`Self::Flood`] and [`Self::BotAbuse`] — three that ask the
    /// reporter to draw a line the queue's own prioritisation should draw, and one that is a
    /// judgement about volume rather than about content, which a person reading one message
    /// cannot make. Nothing in this client constructs them: they are carried so the numbering
    /// is the wire's, and the values are pinned by a test either way. A client that grows a
    /// surface knowing which it means — a bot screen, a support form — finds the code here.
    #[allow(dead_code)]
    ChildSafety,
    /// A bot misbehaving: a broken integration rather than an abusive person.
    ///
    /// One of the four codes no surface here offers; see the note on [`Self::ChildSafety`].
    #[allow(dead_code)]
    BotAbuse,
    /// None of the above.
    Other,
}

impl ReportReason {
    /// The value `REPORT_CREATE.reason` carries.
    pub const fn wire(self) -> u32 {
        match self {
            Self::Spam => 0,
            Self::Flood => 1,
            Self::Scam => 2,
            Self::MaliciousLink => 3,
            Self::Harassment => 4,
            Self::HateSpeech => 5,
            Self::SexualContent => 6,
            Self::Violence => 7,
            Self::SelfHarm => 8,
            Self::Impersonation => 9,
            Self::ChildSafety => 10,
            Self::BotAbuse => 11,
            Self::Other => 12,
        }
    }
}

/// The longest note the node accepts, in characters.
///
/// Mirrors the warden's own ceiling. The sheet checks it before the frame is composed so an
/// over-long note fails without spending the frame or the report's cost on a call that can only be
/// refused — a refusal that arrives after the reporter typed the whole thing has already cost them
/// the typing.
pub const REPORT_NOTE_MAX_LEN: usize = 500;

/// One reason as the sheet lists it: the code, and the two lines of words that explain it.
pub struct ReportReasonOption {
    /// The code the choice sends.
    pub reason: ReportReason,
    /// The menu item, in the web client's own words.
    pub label: &'static str,
    /// The line under it, saying what the code means in practice.
    pub hint: &'static str,
}

/// The reasons offered, in the order the sheet lists them.
///
/// A subset of [`ReportReason`]: the codes a person can actually judge for themselves. The rest —
/// [`ReportReason::ChildSafety`], [`ReportReason::SelfHarm`], [`ReportReason::BotAbuse`] — are
/// reachable only through a surface that already knows which it means, and are deliberately not a
/// menu item, because a reporter asked to choose between "child safety" and "sexual content" in a
/// list is being asked to draw a legal line the queue's own prioritisation should draw instead.
///
/// [`ReportReason::Other`] is last and is the only catch-all: a menu that put it first would collect
/// every report from every reporter who reads the list top-down.
///
/// The words are the web client's and the Android sheet's, deliberately identical, because the
/// reason a reporter picks has to mean the same thing on every device they might pick it from.
pub const REPORT_REASONS: [ReportReasonOption; 9] = [
    ReportReasonOption {
        reason: ReportReason::Spam,
        label: "Spam",
        hint: "Unwanted bulk messages or invites.",
    },
    ReportReasonOption {
        reason: ReportReason::Scam,
        label: "Scam or fraud",
        hint: "Trying to get money or details by deception.",
    },
    ReportReasonOption {
        reason: ReportReason::MaliciousLink,
        label: "Malicious link",
        hint: "A link to malware, phishing, or a page that steals sign-ins.",
    },
    ReportReasonOption {
        reason: ReportReason::Harassment,
        label: "Harassment",
        hint: "Threats or targeted abuse of a person.",
    },
    ReportReasonOption {
        reason: ReportReason::HateSpeech,
        label: "Hate speech",
        hint: "Hateful content aimed at a group.",
    },
    ReportReasonOption {
        reason: ReportReason::SexualContent,
        label: "Sexual content",
        hint: "Sexual content where it does not belong.",
    },
    ReportReasonOption {
        reason: ReportReason::Violence,
        label: "Violence",
        hint: "Graphic violence.",
    },
    ReportReasonOption {
        reason: ReportReason::Impersonation,
        label: "Impersonation",
        hint: "Pretending to be somebody else.",
    },
    ReportReasonOption {
        reason: ReportReason::Other,
        label: "Something else",
        hint: "None of the above.",
    },
];

/// What a report points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportTarget {
    /// Which of the four things is being reported.
    pub kind: ReportSubject,
    /// The id of that thing, in the vocabulary [`ReportSubject`] documents.
    pub id: Id,
    /// The reporter's own phrasing for it — "this message", a display name, "this room".
    ///
    /// Never quoted content: see this module's note on the pointer and the copy. The sheet's title
    /// and its acknowledgement sentence are both built from this string, so it has to read as the
    /// object of "Report".
    pub label: String,
}

impl ReportTarget {
    /// One message, labelled for the sheet that is about to name it.
    pub fn message(id: Id) -> Self {
        Self {
            kind: ReportSubject::Message,
            id,
            label: "this message".to_owned(),
        }
    }

    /// One account, labelled by the display name the surface already drew.
    pub fn user(id: Id, label: impl Into<String>) -> Self {
        Self {
            kind: ReportSubject::User,
            id,
            label: label.into(),
        }
    }

    /// One room, labelled by its name when the surface knows one.
    pub fn room(id: Id, label: impl Into<String>) -> Self {
        Self {
            kind: ReportSubject::Room,
            id,
            label: label.into(),
        }
    }

    /// One bot, by `bot.bot_id`.
    ///
    /// A bot is reported as a bot and not as its owner's account, because the two are different
    /// problems with different remedies: a moderator reading the queue needs to tell "this
    /// integration is broken" from "this person is abusive" before deciding anything.
    ///
    /// Nothing on this client calls it: a bot target belongs to a screen that shows bots, and
    /// this one has none. It is here so the vocabulary is whole for the client that grows one.
    #[allow(dead_code)]
    pub fn bot(id: Id, label: impl Into<String>) -> Self {
        Self {
            kind: ReportSubject::Bot,
            id,
            label: label.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire numbering is what the node reads, so it is pinned here rather than left to the
    /// order the variants happen to be written in. A bot numbered 4 — the store's number — would
    /// archive a report about a bot as a report about a media object, and nothing would say so.
    #[test]
    fn the_subject_kinds_are_the_wires_own_numbers() {
        assert_eq!(0, ReportSubject::User.wire());
        assert_eq!(1, ReportSubject::Message.wire());
        assert_eq!(2, ReportSubject::Room.wire());
        assert_eq!(3, ReportSubject::Bot.wire());
    }

    /// All thirteen codes, in order, pinned for the same reason: they are stored as given, so a
    /// gap or a reordering here rewrites the meaning of every report already in a queue.
    #[test]
    fn the_reason_codes_are_dense_and_in_the_order_the_warden_stores_them() {
        let codes: Vec<u32> = [
            ReportReason::Spam,
            ReportReason::Flood,
            ReportReason::Scam,
            ReportReason::MaliciousLink,
            ReportReason::Harassment,
            ReportReason::HateSpeech,
            ReportReason::SexualContent,
            ReportReason::Violence,
            ReportReason::SelfHarm,
            ReportReason::Impersonation,
            ReportReason::ChildSafety,
            ReportReason::BotAbuse,
            ReportReason::Other,
        ]
        .iter()
        .map(|reason| reason.wire())
        .collect();
        assert_eq!((0..13).collect::<Vec<u32>>(), codes);
    }

    /// The menu is the codes a person can judge for themselves, and the three that are not on it
    /// are absent on purpose — a reporter choosing between "child safety" and "sexual content" in a
    /// list is being asked to make a legal distinction the queue should make instead.
    #[test]
    fn the_menu_omits_the_three_codes_a_reporter_cannot_judge() {
        let offered: Vec<ReportReason> = REPORT_REASONS.iter().map(|entry| entry.reason).collect();
        assert!(!offered.contains(&ReportReason::ChildSafety));
        assert!(!offered.contains(&ReportReason::SelfHarm));
        assert!(!offered.contains(&ReportReason::BotAbuse));
        assert_eq!(9, offered.len());
        // The catch-all is last: a list read top-down must not meet "Something else" first.
        assert_eq!(Some(&ReportReason::Other), offered.last());
    }

    /// Every menu entry carries words, because a row with an empty label is a button nobody can
    /// decide to press.
    #[test]
    fn every_offered_reason_has_a_label_and_a_hint() {
        for entry in &REPORT_REASONS {
            assert!(!entry.label.is_empty(), "{:?} has no label", entry.reason);
            assert!(!entry.hint.is_empty(), "{:?} has no hint", entry.reason);
        }
    }

    /// The message door names the conversation nowhere, because the wire has nowhere to put it: a
    /// report row holds one id, and the label is the reporter's phrasing rather than a key.
    #[test]
    fn a_message_target_carries_one_id_and_a_label_that_quotes_nothing() {
        let id = Id::from_bytes([7u8; 16]);
        let target = ReportTarget::message(id);
        assert_eq!(ReportSubject::Message, target.kind);
        assert_eq!(id, target.id);
        assert_eq!("this message", target.label);
    }
}
