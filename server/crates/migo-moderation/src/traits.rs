//! The moderation contract, and the one question this crate cannot answer itself.
//!
//! # Two traits, and why the second one is a port
//!
//! [`Warden`] is what moderation does. [`Roster`] is who is allowed to do it, and it is
//! implemented outside this crate.
//!
//! There is no global role column in the schema. `docs/04-data-model.md` gives a role to
//! a room member and to nobody else, which is correct — a room moderator is a property of
//! a room — and it leaves "is this account a member of staff" as a question with no answer
//! in the database. Every way of answering it is a deployment decision: a list of account
//! ids in configuration, a scope on the access token, a group in an external directory, a
//! table added by an operator who runs their own migrations.
//!
//! So this crate asks, and something else answers. That is the same shape `migo_media`
//! uses for object storage: an erased trait the composition root implements, so the domain
//! crate holds the rule and the deployment holds the mechanism. The alternative — a
//! `staff` table invented here — would be this crate inventing an identity system, and it
//! would be wrong for every deployment that already has one.
//!
//! # Why the powers are checked inside the service
//!
//! [`Warden`]'s operator methods take an [`Operator`] whose `powers` the *service* fills
//! in from [`Roster`], on every call, before anything else happens. A design where the
//! gateway resolved the powers and the service trusted them would work, and it would put
//! the authorization check in the layer that has the most paths through it. One of those
//! paths would eventually be written by somebody who did not know about the check.
//!
//! # What is deliberately not here
//!
//! **No room bans and no room mutes.** `migo_rooms` owns `set_room_sanction` and the
//! permission model around it. A second crate writing that column would be two owners of
//! one rule, and the first time they disagreed a ban would be lifted by the code that did
//! not know about it. A report about a room resolves to an archive here, or to an
//! escalation that a room's own moderators act on there.
//!
//! **No automated reading of message content.** Brief section 49 asks for detection of
//! scams, malicious links, and abusive behaviour. All three live inside a message body,
//! and for a private conversation the server holds ciphertext — section 122 says so, and
//! says the validation moves to the client. What is left on the server is rate and shape,
//! which is what [`Warden::assess`] scores. A detector here that needed plaintext would be
//! a reason to stop encrypting, and that trade is not on offer.
//!
//! **No deletion of an account.** `AccountStatus::Deleted` purges personal data and is
//! reached through the account lifecycle, which owns the ordering of that purge against
//! sessions, devices, and the ledger. A moderator suspends; the account service deletes.
//!
//! **No appeal workflow.** An appeal is a report filed by the suspended account about the
//! action taken against it, which the queue already models, and a separate table for it
//! would be a second queue that nobody watches.

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_store::model::{AuditEntry, AuditTargetKind};

use crate::model::{
    Action, Assessment, Caller, Case, Filed, Filing, Operator, Powers, Resolution, Signals,
};
use crate::notice::Notice;

/// Who is a member of staff, and what they may do.
///
/// Implemented by the composition root. Returning [`Powers::NONE`] for an unknown account
/// is the correct answer and the safe one: the failure mode of a roster that cannot decide
/// is an operator who is told they may not, which is recoverable, rather than a stranger
/// who is told they may, which is not.
#[async_trait]
pub trait Roster: Send + Sync {
    /// What this account may do.
    ///
    /// Called on every operator request. An implementation that consults a network service
    /// should cache — but should cache the grant briefly and the *absence* of a grant not
    /// at all, because the direction that matters is revocation taking effect quickly.
    async fn powers(&self, account_id: Id) -> Result<Powers>;
}

/// A shared, fully erased staff directory.
pub type SharedRoster = std::sync::Arc<dyn Roster>;

/// Everything moderation does.
#[async_trait]
pub trait Warden: Send + Sync {
    /// Files a report.
    ///
    /// Idempotent on the pair of reporter and subject while a report about it is open, so
    /// filing twice returns the first report with [`Filed::duplicate`] set rather than an
    /// error. Brief section 153 asks for that shape, and the alternative is a table where
    /// one grievance can become a hundred thousand rows.
    ///
    /// Every account may file, including one that is suspended. A suspension is the
    /// server's opinion about somebody's behaviour; it is not a reason to stop hearing
    /// what they have to say about somebody else's.
    async fn file_report(&self, caller: &Caller, filing: Filing) -> Result<Filed>;

    /// The queue, oldest first.
    ///
    /// Oldest first because a report that has waited longest is the one most likely to
    /// matter, which is also the order the partial index on open reports is built for.
    /// Requires [`Powers::TRIAGE`].
    async fn queue(&self, operator: &Operator, limit: Option<u16>) -> Result<Vec<Case>>;

    /// One report.
    ///
    /// Requires [`Powers::TRIAGE`]. Answers `NOT_FOUND` when there is no such report and
    /// also when the caller has no business knowing whether there is: brief section 48
    /// asks for `NOT_FOUND` rather than `PERMISSION_DENIED` wherever the existence of an
    /// object is itself the secret, and the existence of a report about somebody is.
    async fn report(&self, operator: &Operator, report_id: Id) -> Result<Case>;

    /// Closes a report.
    ///
    /// Requires [`Powers::TRIAGE`] and a re-authenticated session. Refuses with `CONFLICT`
    /// when the report was already closed, which is the store's behaviour and the right
    /// one: two moderators opening the same report is normal, both of them deciding it is
    /// not, and the second one needs to be told rather than have their verdict silently
    /// overwrite the first.
    ///
    /// [`Resolution::Escalated`] leaves the report open, and the audit entry is what
    /// records that it was escalated. An escalation that closed the report would lose the
    /// only sign that somebody is still waiting.
    async fn resolve(
        &self,
        operator: &Operator,
        report_id: Id,
        resolution: Resolution,
        reason: Option<&str>,
    ) -> Result<Case>;

    /// Takes an action.
    ///
    /// Requires the power [`Action::requires`] names and a re-authenticated session. The
    /// audit entry is written in the same call, after the change and before the return, so
    /// that an action without a record cannot be observed by anything downstream.
    ///
    /// Returns the [`Notice`] to deliver, which is `None` for every content takedown — see
    /// the [`notice`](crate::notice) module for why that is a fact about what this crate
    /// knows rather than a decision about what a user deserves.
    async fn act(
        &self,
        operator: &Operator,
        action: Action,
        reason: Option<&str>,
    ) -> Result<Option<Notice>>;

    /// The audit trail for one target, newest first.
    ///
    /// Requires [`Powers::AUDIT`]. This is also the warning history: a warning is an audit
    /// entry and nothing else, so `audit` over an account with
    /// [`AuditTargetKind::Account`] is what brief section 49's dashboard shows under
    /// "warnings".
    async fn audit(
        &self,
        operator: &Operator,
        target_kind: AuditTargetKind,
        target_id: Id,
        limit: Option<u16>,
    ) -> Result<Vec<AuditEntry>>;

    /// Scores one account's metadata.
    ///
    /// The caller supplies the rate counters it already keeps for the limiter; this crate
    /// fills in the one signal that comes from its own table, which is how many other
    /// people complained.
    ///
    /// Not rate limited. It is called on the path of an operation that has already been
    /// charged, and charging again would bill one user action twice and make a send budget
    /// depend on how many checks the implementation happens to run.
    ///
    /// Writes nothing unless the level reaches [`crate::model::Risk::Restrict`] *and* the
    /// deployment turned automatic suspension on. The default is to make the queue loud,
    /// not to make the ban automatic.
    async fn assess(&self, account_id: Id, signals: Signals, now: Timestamp) -> Result<Assessment>;
}

/// A shared, fully erased moderation service.
pub type SharedWarden = std::sync::Arc<dyn Warden>;
