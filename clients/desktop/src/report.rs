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
    /// The two are different ids and different problems: a bot report names the integration, a
    /// user report names the person who owns it, and a moderator deciding what to do needs to
    /// tell them apart before deciding anything. A surface that holds both ids picks between
    /// them through [`person_target`], which is the only place in this client that makes the
    /// choice.
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
    /// One of the three codes no surface here offers; see the note on [`Self::ChildSafety`].
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
    /// One of the three codes no surface here offers; see the note on [`Self::ChildSafety`].
    #[allow(dead_code)]
    SelfHarm,
    /// Somebody pretending to be somebody else.
    Impersonation,
    /// Child sexual abuse material. Kept its own code; see the enum's own note.
    ///
    /// One of the three codes the sheet deliberately does not list, alongside [`Self::SelfHarm`]
    /// and [`Self::Flood`] — two that ask the reporter to draw a line the queue's own
    /// prioritisation should draw, and one that is a judgement about volume rather than about
    /// content, which a person reading one message cannot make. Nothing in this client constructs
    /// them: they are carried so the numbering is the wire's, and the values are pinned by a test
    /// either way. A client that grows a surface knowing which it means — a support form, say —
    /// finds the code here.
    #[allow(dead_code)]
    ChildSafety,
    /// A bot misbehaving: a broken integration rather than an abusive person.
    ///
    /// The fourth code that is not a menu item, and the one that is nevertheless reachable: a bot
    /// is named on the wire, this client marks one wherever it draws one, and the marking already
    /// answered the question a reason list would otherwise ask. So [`reasons_for`] puts this row
    /// in front of the nine for a [`ReportSubject::Bot`] and [`opening_reason`] opens the sheet on
    /// it, while every other subject is offered the nine alone.
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
/// [`ReportReason::ChildSafety`], [`ReportReason::SelfHarm`] — are reachable only through a
/// surface that already knows which it means, and are deliberately not a menu item, because a
/// reporter asked to choose between "child safety" and "sexual content" in a list is being asked
/// to draw a legal line the queue's own prioritisation should draw instead.
///
/// The fourth withheld code, [`ReportReason::BotAbuse`], is the one exception and is not in this
/// list either: see [`BOT_REPORT_REASON`] and [`reasons_for`] for where it is offered instead.
///
/// [`ReportReason::Other`] is last and is the only catch-all: a menu that put it first would collect
/// every report from every reporter who reads the list top-down.
///
/// The words are the web client's and the Android sheet's, deliberately identical, because the
/// reason a reporter picks has to mean the same thing on every device they might pick it from.
///
/// A `static` rather than a `const`, and for one reason: [`reasons_for`] hands out references
/// into this list, and a reference into a `const` is a reference into a temporary the compiler
/// may or may not promote — a `static` is one value at one address for the life of the program,
/// so the borrow is `'static` by construction rather than by the promotion rules.
pub static REPORT_REASONS: [ReportReasonOption; 9] = [
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

/// The bot-abuse row, which is offered to exactly one subject and to no other.
///
/// The words are the web dialog's and the Android sheet's, deliberately identical, because the
/// reason a reporter picks has to mean the same thing on every device they might pick it from.
///
/// It is not in [`REPORT_REASONS`] because the rule that keeps it out of the generic menu is not
/// "bot abuse is never offered" but "bot abuse is offered exactly where the surface already knows
/// the subject is a bot" — and a rule stated that way is a rule about the *subject*, which is what
/// [`reasons_for`] implements. Keeping it in the array and filtering it out per subject would put
/// the nine and the ten one boolean apart in the same list, where a caller that forgot the filter
/// would offer it to everybody and nothing would look wrong.
pub const BOT_REPORT_REASON: ReportReasonOption = ReportReasonOption {
    reason: ReportReason::BotAbuse,
    label: "Bot misbehaving",
    hint: "A bot that is broken, spammy, or abusive — a bad integration, not a bad person.",
};

/// The reason rows a report about `subject` offers, in the order the sheet draws them.
///
/// The nine every subject gets, and one row in front of them for a bot. Nothing is swapped out and
/// nothing is lost: a reporter who knows a bot is doing something the bot row does not describe can
/// still pick any of the nine, which is what makes the bot row an opening position rather than a
/// verdict.
///
/// This mirrors the web dialog's `personSubject` and the Android sheet's `reportReasons`, and the
/// difference between the three clients is worth stating because it is deliberate. The web dialog
/// can open on a code its menu does not draw — it shows no radio checked while Send stays live —
/// and this sheet cannot: `None` is what keeps Send dark here, and a Send that is live over a
/// choice the reporter cannot see is a report filed under a reason nobody picked. So the row the
/// sheet opens on has to be a row it drew, and for a bot that means this function has to add one.
pub fn reasons_for(subject: ReportSubject) -> Vec<&'static ReportReasonOption> {
    let mut rows: Vec<&'static ReportReasonOption> = Vec::with_capacity(REPORT_REASONS.len() + 1);
    if subject == ReportSubject::Bot {
        rows.push(&BOT_REPORT_REASON);
    }
    rows.extend(REPORT_REASONS.iter());
    rows
}

/// The reason a sheet on `target` opens with, or `None` while the reporter is still reading.
///
/// `None` is the ordinary answer and the one every subject but a bot gets: the sheet's Send stays
/// dark until a row is picked, because a list drawn with a row already chosen is a list nobody
/// reads. A bot is the exception, and only because the marking already answered the question this
/// list would otherwise ask — the surface knew the account was a bot before the reporter opened
/// anything, which is exactly the knowledge that makes opening on a code honest rather than a guess.
pub const fn opening_reason(target: &ReportTarget) -> Option<ReportReason> {
    if matches!(target.kind, ReportSubject::Bot) {
        Some(ReportReason::BotAbuse)
    } else {
        None
    }
}

/// The report an account row points at, given the account and the bot it may speak as.
///
/// An account that speaks as a bot is reported as the bot and not as the account behind it, because
/// `bot.bot_id` is a different id from the account id: a report filed under the account id would
/// reach a moderator as a report about a bot that names something which is not one, and nothing on
/// the wire would look wrong while it did. Every person-reporting door in this client goes through
/// here for that reason — the rule is one branch, and it belongs in one place rather than in each
/// row that happens to have a bot in it.
pub fn person_target(account_id: Id, bot_id: Option<Id>, label: impl Into<String>) -> ReportTarget {
    match bot_id {
        Some(bot) => ReportTarget::bot(bot, label),
        None => ReportTarget::user(account_id, label),
    }
}

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
    /// Reached through [`person_target`] rather than called directly by the surfaces that report a
    /// person: a row holding both ids has to pick one, and a pick made at each row is a pick that
    /// can be made wrongly at one of them without anything saying so.
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
    ///
    /// `BotAbuse` is absent from this list too, and that is a different fact about it rather than
    /// the same one: it is offered, to a bot subject and to nothing else. The two claims are
    /// asserted separately so neither can quietly become the other.
    #[test]
    fn the_menu_omits_the_three_codes_a_reporter_cannot_judge() {
        let offered: Vec<ReportReason> = REPORT_REASONS.iter().map(|entry| entry.reason).collect();
        assert!(!offered.contains(&ReportReason::ChildSafety));
        assert!(!offered.contains(&ReportReason::SelfHarm));
        assert!(!offered.contains(&ReportReason::Flood));
        assert!(!offered.contains(&ReportReason::BotAbuse));
        assert_eq!(9, offered.len());
        // The catch-all is last: a list read top-down must not meet "Something else" first.
        assert_eq!(Some(&ReportReason::Other), offered.last());
        // And "the generic menu" is not "every code": Flood is a fourth code no subject is
        // offered, which the assertion above would pass just as well if it were missing from the
        // enum entirely.
        assert!(!reasons_for(ReportSubject::User)
            .iter()
            .any(|row| row.reason == ReportReason::Flood));
    }

    /// The one thing a bot subject changes about the menu: a row in front, and nothing else.
    #[test]
    fn a_bot_subject_is_offered_the_bot_reason_first_and_keeps_every_other_row() {
        let menu = reasons_for(ReportSubject::Bot);
        assert_eq!(REPORT_REASONS.len() + 1, menu.len());
        assert_eq!(ReportReason::BotAbuse, menu[0].reason);
        // The nine behind it, in order and unaltered: the reporter who knows the bot is doing
        // something the bot row does not describe can still say so.
        let tail: Vec<ReportReason> = menu[1..].iter().map(|row| row.reason).collect();
        let generic: Vec<ReportReason> = REPORT_REASONS.iter().map(|row| row.reason).collect();
        assert_eq!(generic, tail);
        // Nothing duplicated, which is what makes building the list by concatenation safe.
        for (index, row) in menu.iter().enumerate() {
            assert!(
                !menu[index + 1..]
                    .iter()
                    .any(|later| later.reason == row.reason),
                "{:?} is offered twice",
                row.reason
            );
        }
    }

    /// Every other subject gets the generic menu untouched, which is the half a bot-only test
    /// would not notice breaking.
    #[test]
    fn every_other_subject_gets_the_generic_menu_untouched() {
        for subject in [
            ReportSubject::User,
            ReportSubject::Message,
            ReportSubject::Room,
        ] {
            let rows: Vec<ReportReason> =
                reasons_for(subject).iter().map(|row| row.reason).collect();
            let generic: Vec<ReportReason> = REPORT_REASONS.iter().map(|row| row.reason).collect();
            assert_eq!(generic, rows, "{subject:?} is not a bot");
        }
    }

    /// The pairing that matters: whatever the sheet opens with has to be a row the reporter can
    /// see picked. The reason and the rows are decided in two different functions, so they are
    /// asserted against each other here rather than assumed to agree.
    #[test]
    fn the_reason_a_bot_sheet_opens_on_is_the_row_its_menu_shows_first() {
        let target = person_target(
            Id::from_bytes([3u8; 16]),
            Some(Id::from_bytes([4u8; 16])),
            "Ana",
        );
        let opening = opening_reason(&target).expect("a bot subject opens on a reason");
        assert_eq!(opening, reasons_for(target.kind)[0].reason);
    }

    /// And the ordinary answer is none: Send stays dark until the reporter picks.
    #[test]
    fn every_other_subject_opens_with_nothing_chosen() {
        let account = Id::from_bytes([3u8; 16]);
        assert_eq!(None, opening_reason(&person_target(account, None, "Ana")));
        assert_eq!(
            None,
            opening_reason(&ReportTarget::room(account, "the room"))
        );
        assert_eq!(None, opening_reason(&ReportTarget::message(account)));
        // The row the bot sheet opens on is a row the generic menu does *not* hold, which is the
        // whole reason `reasons_for` has to add it: a sheet opening on a code it never drew would
        // be a live Send over an unseen choice.
        assert!(!REPORT_REASONS
            .iter()
            .any(|row| row.reason == ReportReason::BotAbuse));
    }

    /// Which id a report about a person carries, in the one place that decides it.
    #[test]
    fn an_account_that_speaks_as_a_bot_is_reported_as_the_bot_not_as_the_account() {
        let account = Id::from_bytes([3u8; 16]);
        let bot = Id::from_bytes([4u8; 16]);
        let target = person_target(account, Some(bot), "Ana");
        assert_eq!(ReportSubject::Bot, target.kind);
        assert_eq!(bot, target.id);
        assert_ne!(account, target.id, "the two ids are different things");

        // No bot named is an ordinary account report — and a null on the wire is the absence of a
        // claim, not a claim that the account is a person.
        let person = person_target(account, None, "Ana");
        assert_eq!(ReportSubject::User, person.kind);
        assert_eq!(account, person.id);
    }

    /// The bot-abuse row is offered to a bot and to nothing else, which is the rule `reasons_for`
    /// states as being about the subject rather than about the code.
    #[test]
    fn the_bot_row_is_offered_to_a_bot_and_to_no_other_subject() {
        for subject in [
            ReportSubject::User,
            ReportSubject::Message,
            ReportSubject::Room,
        ] {
            assert!(
                !reasons_for(subject)
                    .iter()
                    .any(|row| row.reason == ReportReason::BotAbuse),
                "{subject:?} is not a bot and must not be offered bot abuse"
            );
        }
        assert_eq!(ReportReason::BotAbuse, BOT_REPORT_REASON.reason);
        assert!(!BOT_REPORT_REASON.label.is_empty());
        assert!(!BOT_REPORT_REASON.hint.is_empty());
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
