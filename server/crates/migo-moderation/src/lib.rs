//! Reports, moderator actions, the audit trail, and metadata-only abuse scoring.
//!
//! Brief sections 49 and 50 in one crate: the report queue a user fills, the actions a
//! member of staff takes, the audit trail that records both, and the adaptive rate
//! limiting that runs without anybody watching.
//!
//! # What this crate can see
//!
//! Almost nothing. Brief section 122 says that for an end-to-end conversation the server
//! holds ciphertext, so content validation lives on the client. That single sentence
//! decides the shape of everything here:
//!
//! - A report is a **pointer**, never a copy. The schema comment says so — *evidence is a
//!   reference, never a copy of message content: copying private ciphertext into a
//!   moderation table would defeat the point of encrypting it* — and this crate stores a
//!   subject kind, an id, a reason code, and the reporter's own note. Not the message.
//! - Of the six automated-detection categories brief section 49 lists, three — scam,
//!   malicious links, abusive behaviour — **cannot be detected here at all**, because each
//!   one is a judgement about a message body this server cannot read. What is left is rate
//!   and shape, which is what [`model::Signals`] carries.
//! - A takedown works on ids. [`model::Action::RemoveMessage`] tombstones a row without
//!   reading it, which is the only way a takedown can work when the bytes are opaque.
//!
//! # Two callers, on purpose
//!
//! [`model::Caller`] files reports. [`model::Operator`] acts on them. They are separate
//! types rather than one type with a flag so that the compiler notices when an ordinary
//! user's request reaches a path that suspends accounts — a check that costs nothing at
//! runtime and does not depend on anybody remembering to write it.
//!
//! Who counts as staff is not this crate's business. There is no global role column in the
//! schema, so [`traits::Roster`] asks the deployment, exactly as `migo_media` asks the
//! deployment for object storage. See [`traits`] for why that is a port and not a table.
//!
//! # What is written down and where
//!
//! | Thing | Where it goes | Who can read it |
//! |---|---|---|
//! | Report subject, reason, reporter note | `report` | Anyone with [`model::Powers::TRIAGE`] |
//! | What an operator did, and why | `audit_entry` | Anyone with [`model::Powers::AUDIT`] |
//! | Warnings | `audit_entry`, as the action `moderation.account.warn` | The same |
//! | Abuse score | Nowhere. It is computed and returned | Nobody, afterwards |
//!
//! The operator's free-text reason is in that table and in no other place. It never enters
//! an error, a metric label, a notification, or a log line — the rule `migo_rooms` applies
//! to a room ban reason, applied harder here because a moderation note is written for the
//! next moderator and a notification travels further than an error does.
//!
//! # Getting one
//!
//! ```ignore
//! let warden = migo_moderation::open(
//!     store,
//!     limiter,
//!     roster,
//!     Box::new(OsRandom),
//!     ModerationConfig::default(),
//!     &registry,
//! );
//! let filed = warden.file_report(&caller, Filing::new(Subject::User(who), Reason::Spam)).await?;
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod metrics;
pub mod model;
pub mod notice;
pub mod service;
pub mod traits;

pub use crate::model::{
    risk_of, score, Action, Assessment, Caller, Case, Filed, Filing, ModerationConfig, Operator,
    Powers, Reason, Resolution, Risk, Signals, Subject, DEFAULT_PAGE, MAX_NOTE_LEN, MAX_PAGE,
    MAX_REASON_LEN,
};
pub use crate::notice::{Notice, Outcome};
pub use crate::service::{effective_tier, open, Moderation};
pub use crate::traits::{Roster, SharedRoster, SharedWarden, Warden};
