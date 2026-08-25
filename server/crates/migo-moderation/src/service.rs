//! The moderation service.
//!
//! # The five rules this file exists to enforce
//!
//! **An action and its audit entry are one operation.** Every method that changes
//! something writes the change and then appends the audit row before it returns. The
//! schema comment says it — *written in the same transaction as the action it records, so
//! an action without an audit row cannot exist* — and while a store trait without
//! transactions cannot quite promise that, this file can promise that nothing downstream
//! ever observes the change without the record, which is the part that matters when
//! somebody asks six months later who did this.
//!
//! **Powers are resolved here, on every call.** Not read off the request, not trusted from
//! the gateway. The [`Roster`] is asked, the answer is put on the [`Operator`], and the
//! required power is checked before anything else happens.
//!
//! **Existence is not disclosed.** A caller without triage powers asking for a report gets
//! `NOT_FOUND`, not `PERMISSION_DENIED`. Brief section 48 asks for exactly that wherever
//! the existence of an object is itself the secret, and "is there a report about this
//! account" is a question whose answer is worth money to the wrong person.
//!
//! **Nothing here reads message content.** The abuse scorer works on rates and counts. The
//! takedown path works on ids. Brief section 122 moved content validation to the client
//! because for a private conversation the server holds ciphertext, and a detector here
//! that needed plaintext would be an argument for not encrypting.
//!
//! **The operator's own words never leave the audit table.** They go into
//! `audit_entry.reason`, which is read behind [`Powers::AUDIT`]. They never enter an error,
//! a metric label, a notification, or a log line. `migo_rooms` established the rule for a
//! room ban reason; a moderation note travels further, so the rule holds harder.
//!
//! # Where these prices come from
//!
//! Brief section 145 priced the moderation block: `REPORT_CREATE` 20,
//! `MODERATION_ACTION` 10, `MODERATION_EVENT` 0. The opcodes are not in the packet
//! registry yet, so they cannot be charged through `charge_opcode`, but the numbers the
//! brief chose are the numbers used here — copied rather than invented, so that generating
//! the frames later is a change of mechanism and not a reprice.
//!
//! A report costs twice what an action does, which looks backwards until you notice who
//! pays: the report price is charged to an ordinary account and is the only thing standing
//! between a script and a full queue, while the action price is charged to a member of
//! staff whose worst case is clicking too fast.
//!
//! Reads are priced in this file, by analogy with the listings in the rooms and social
//! crates. They are charged to the operator's own budget, which is deliberate — a
//! dashboard that polls the queue every second should feel it.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::metrics::Registry;
use migo_core::random::Random;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{codes, fault};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter, TrustTier};
use migo_store::model::{
    report_status, AccountStatus, AuditActorKind, AuditEntry, AuditTargetKind, Report,
};
use migo_store::{SharedStore, Store};
use parking_lot::Mutex;

use crate::metrics::{Meters, Refused};
use crate::model::{
    risk_of, score, text_is_usable, Action, Assessment, Caller, Case, Filed, Filing,
    ModerationConfig, Operator, Powers, Resolution, Risk, Signals, MAX_NOTE_LEN, MAX_PAGE,
    MAX_REASON_LEN, REPORT_WINDOW_MS,
};
use crate::notice::Notice;
use crate::traits::{Roster, SharedRoster, SharedWarden, Warden};

/// What filing a report costs. Brief section 145, opcode 192.
const REPORT_COST: u32 = 20;

/// What a moderator action costs. Brief section 145, opcode 193.
const ACTION_COST: u32 = 10;

/// What one queue read costs.
///
/// By analogy with the listings in the rooms and social crates, which is where the number
/// 3 for a page of rows comes from.
const QUEUE_COST: u32 = 3;

/// What reading one report costs.
const READ_COST: u32 = 2;

/// What reading the audit trail costs.
///
/// More than a queue read. An audit query is the most sensitive read in this crate — it
/// returns who did what to whom — and a price is a small, honest brake on a script walking
/// it account by account.
const AUDIT_COST: u32 = 5;

/// The who-and-when of one audit entry.
///
/// Bundled rather than passed as six parameters because these six always travel together
/// and always come from the same place: a [`Caller`], an [`Operator`], or nobody at all.
/// Borrowed rather than owned so that assembling one costs nothing — the clone happens once,
/// inside [`Moderation::record`], at the moment the row is built.
struct Trace<'a> {
    actor_id: Option<Id>,
    actor_kind: AuditActorKind,
    request_id: Option<&'a String>,
    ip_class: Option<&'a String>,
    at: Timestamp,
}

impl<'a> Trace<'a> {
    /// An ordinary account acting for itself: filing a report.
    fn user(caller: &'a Caller) -> Self {
        Self {
            actor_id: Some(caller.account_id),
            actor_kind: AuditActorKind::User,
            request_id: caller.request_id.as_ref(),
            ip_class: caller.ip_class.as_ref(),
            at: caller.now,
        }
    }

    /// A member of staff.
    fn operator(operator: &'a Operator) -> Self {
        Self {
            actor_id: Some(operator.account_id),
            actor_kind: AuditActorKind::Operator,
            request_id: operator.request_id.as_ref(),
            ip_class: operator.ip_class.as_ref(),
            at: operator.now,
        }
    }

    /// The scorer, acting on its own.
    ///
    /// No `actor_id` and no request: nobody decided this and no request carried it. Naming
    /// whichever operator happened to be on shift would put a person's name against a
    /// decision a function made.
    fn system(at: Timestamp) -> Self {
        Self {
            actor_id: None,
            actor_kind: AuditActorKind::System,
            request_id: None,
            ip_class: None,
            at,
        }
    }
}

/// Moderation over a store, a rate limiter, and a staff directory.
///
/// No cache. A stale answer here is a suspended account still being served or a resolved
/// report being acted on twice, and brief section 173 requires that losing the cache lose
/// nothing that matters — the way to honour that for a moderation decision is not to put
/// it there.
///
/// `Random` is present because this crate mints two kinds of id — a report and an audit
/// entry — and neither may be derived from anything a caller supplied.
pub struct Moderation<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter, R: ?Sized = dyn Roster> {
    store: Arc<S>,
    limiter: Arc<L>,
    roster: Arc<R>,
    config: ModerationConfig,
    /// Behind a `Mutex` and never held across an `await`. Every use in this file takes the
    /// lock, generates, and drops it on the same line.
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
}

impl<S: Store + ?Sized, L: RateLimiter + ?Sized, R: Roster + ?Sized> Moderation<S, L, R> {
    /// Builds a service over concrete parts.
    pub fn new(
        store: Arc<S>,
        limiter: Arc<L>,
        roster: Arc<R>,
        random: Box<dyn Random>,
        config: ModerationConfig,
        registry: &Registry,
    ) -> Self {
        Self {
            store,
            limiter,
            roster,
            config,
            random: Mutex::new(random),
            meters: Meters::new(registry),
        }
    }

    /// A fresh id stamped with `at`.
    ///
    /// Stamped and not merely random, because an id whose prefix is its own creation time
    /// makes the queue's oldest-first order and the audit trail's newest-first order agree
    /// without either of them having to trust a clock a caller supplied.
    fn new_id(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, &mut **random)
    }

    /// Charges one account's budget.
    ///
    /// One key, the account. Not the device: a report limit that was per device would be a
    /// report limit somebody defeats by opening a second tab, and brief section 50 prices
    /// abuse per person.
    async fn charge(&self, account_id: Id, tier: TrustTier, cost: u32, now: Timestamp) -> Result<()> {
        self.limiter
            .charge(&[BucketKey::account(account_id)], cost, tier, now)
            .await?
            .into_result()
    }

    /// Charges a reporter, counting a refusal.
    async fn charge_reporter(&self, caller: &Caller, cost: u32) -> Result<()> {
        self.charge(caller.account_id, caller.tier, cost, caller.now)
            .await
            .inspect_err(|_| self.meters.refused(Refused::RateLimited))
    }

    /// Charges an operator, counting a refusal.
    ///
    /// Staff traffic is charged at [`TrustTier::Established`] and not at whatever tier the
    /// session happens to carry. A moderator's budget should not depend on how old their
    /// account is, and it should not be raised by making them `Trusted` either — the number
    /// wanted here is "enough for a person working a queue", which is what the established
    /// budget already is.
    async fn charge_operator(&self, operator: &Operator, cost: u32) -> Result<()> {
        self.charge(
            operator.account_id,
            TrustTier::Established,
            cost,
            operator.now,
        )
        .await
        .inspect_err(|_| self.meters.refused(Refused::RateLimited))
    }

    /// Resolves an operator's powers and checks one of them.
    ///
    /// Returns a copy of the operator with the resolved powers attached, so that a caller
    /// which passed [`Powers::NONE`] cannot accidentally be trusted and one which passed
    /// [`Powers::ALL`] cannot accidentally be believed.
    ///
    /// Every refusal is `NOT_FOUND`-shaped at the call sites that need it and
    /// `PERMISSION_DENIED` here; the distinction is made by the caller, because only the
    /// caller knows whether the *existence* of what was asked for is a secret.
    async fn resolve_powers(&self, operator: &Operator, needs: Powers) -> Result<Operator> {
        let powers = self.roster.powers(operator.account_id).await?;
        if !powers.contains(needs) {
            self.meters.refused(Refused::Denied);
            return Err(fault::permission_denied("not permitted"));
        }
        Ok(Operator {
            powers,
            ..operator.clone()
        })
    }

    /// The same, plus the freshness requirement every action carries.
    ///
    /// Checked after the power and before the limiter, in that order and on purpose: a
    /// caller who is not staff should not learn that the freshness rule exists, and a
    /// refusal that had already spent budget would let somebody drain a moderator's
    /// allowance by replaying a stale session.
    async fn resolve_for_action(&self, operator: &Operator, needs: Powers) -> Result<Operator> {
        let resolved = self.resolve_powers(operator, needs).await?;
        if !resolved.reauthenticated {
            self.meters.refused(Refused::Stale);
            return Err(fault::error(
                codes::REAUTHENTICATION_REQUIRED,
                "a moderator action needs a recently proved factor",
            ));
        }
        Ok(resolved)
    }

    /// Appends one audit entry.
    ///
    /// Takes the operator-written reason and puts it exactly here. This is the only place
    /// in the crate that stores it and the only place that should ever be edited when
    /// somebody asks where moderator notes go.
    ///
    /// The failure is propagated, and that is the one place this crate deliberately
    /// disagrees with `migo_auth`, which logs an audit failure and carries on. It is right
    /// there and wrong here: a sign-in that went unlogged is a gap in a history, while a
    /// suspension that went unlogged is an account somebody cannot find out who closed.
    /// The schema comment asks for the stronger promise — *an action without an audit row
    /// cannot exist* — so a caller that could not write the row is told the action failed.
    async fn record(
        &self,
        trace: Trace<'_>,
        action: &str,
        target_kind: AuditTargetKind,
        target_id: Id,
        summary: String,
        reason: Option<&str>,
    ) -> Result<()> {
        self.store
            .append_audit(AuditEntry {
                audit_id: self.new_id(trace.at),
                actor_id: trace.actor_id,
                actor_kind: trace.actor_kind.to_i16(),
                action: action.to_string(),
                target_kind: target_kind.to_i16(),
                target_id: Some(target_id),
                summary,
                reason: reason.map(str::to_string),
                request_id: trace.request_id.cloned(),
                ip_class: trace.ip_class.cloned(),
                created_at: trace.at,
            })
            .await?;
        self.meters.audit_written();
        Ok(())
    }

    /// Validates an operator-supplied reason.
    fn vet_reason(reason: Option<&str>) -> Result<Option<&str>> {
        match reason {
            None => Ok(None),
            Some(text) if text_is_usable(text, MAX_REASON_LEN) => Ok(Some(text.trim())),
            Some(_) => Err(fault::field_too_long("reason", MAX_REASON_LEN)),
        }
    }

    /// The page size to use.
    fn page(&self, limit: Option<u16>) -> u16 {
        limit.unwrap_or(self.config.page).clamp(1, MAX_PAGE)
    }

    /// Reads a report or refuses without saying whether it is there.
    async fn load(&self, report_id: Id) -> Result<Report> {
        self.store.report(report_id).await?.ok_or_else(|| {
            self.meters.refused(Refused::Missing);
            fault::not_found("report")
        })
    }
}

#[async_trait]
impl<S: Store + ?Sized, L: RateLimiter + ?Sized, R: Roster + ?Sized> Warden for Moderation<S, L, R> {
    async fn file_report(&self, caller: &Caller, filing: Filing) -> Result<Filed> {
        if caller.account_id.is_nil() {
            self.meters.refused(Refused::Invalid);
            return Err(fault::unauthenticated("a report needs an account"));
        }
        if filing.subject.is_nil() {
            self.meters.refused(Refused::Invalid);
            return Err(fault::validation("subject", "an id is required"));
        }
        // Reporting yourself is not a moderation problem. It is either a client bug or
        // somebody testing the button, and either way it must not reach the queue a human
        // has to read.
        if filing.subject == crate::model::Subject::User(caller.account_id) {
            self.meters.refused(Refused::Invalid);
            return Err(fault::validation("subject", "cannot report yourself"));
        }
        let note = match filing.note.as_deref() {
            None => None,
            Some(text) if text_is_usable(text, MAX_NOTE_LEN) => Some(text.trim().to_string()),
            Some(_) => {
                self.meters.refused(Refused::Invalid);
                return Err(fault::field_too_long("note", MAX_NOTE_LEN));
            }
        };

        self.charge_reporter(caller, REPORT_COST).await?;

        let kind = filing.subject.kind();
        let subject_id = filing.subject.id();
        // Idempotency before insertion, and scoped to reports that are still open. A
        // subject reported, actioned, and then misbehaving again is a new report, not a
        // duplicate of a closed one.
        if let Some(existing) = self
            .store
            .open_report_by_reporter(caller.account_id, kind, subject_id)
            .await?
        {
            self.meters.duplicate();
            return Ok(Filed {
                report_id: existing.report_id,
                duplicate: true,
            });
        }

        let report_id = self.new_id(caller.now);
        self.store
            .create_report(Report {
                report_id,
                reporter_id: caller.account_id,
                subject_kind: kind,
                subject_id,
                room_id: filing.room_id,
                reason: filing.reason.to_i16(),
                note,
                evidence_ref: filing.evidence_ref,
                status: report_status::OPEN,
                created_at: caller.now,
                resolved_at: None,
                resolved_by: None,
                resolution: None,
            })
            .await?;

        // The reporter is the actor, so `AuditActorKind::User`. The summary names the
        // reason code and nothing else: not the note, which is the reporter's words, and
        // not the subject's username, which would put a person's name in a row that is
        // kept for as long as the audit trail is.
        self.record(
            Trace::user(caller),
            "moderation.report.create",
            AuditTargetKind::Report,
            report_id,
            format!(
                "report filed about {} for {}",
                filing.subject.label(),
                filing.reason.label()
            ),
            None,
        )
        .await?;

        self.meters.filed(kind, filing.reason);
        Ok(Filed {
            report_id,
            duplicate: false,
        })
    }

    async fn queue(&self, operator: &Operator, limit: Option<u16>) -> Result<Vec<Case>> {
        let resolved = self.resolve_powers(operator, Powers::TRIAGE).await?;
        self.charge_operator(&resolved, QUEUE_COST).await?;
        let rows = self.store.open_reports(self.page(limit)).await?;
        self.meters.queue_read(rows.len());
        Ok(rows.into_iter().map(Case::of).collect())
    }

    async fn report(&self, operator: &Operator, report_id: Id) -> Result<Case> {
        // `NOT_FOUND` and not `PERMISSION_DENIED`. Brief section 48: for an object that
        // should not be known to exist, the answer is the one that does not confirm it
        // does. A caller who is not staff learns nothing about whether this report is real.
        let resolved = match self.resolve_powers(operator, Powers::TRIAGE).await {
            Ok(resolved) => resolved,
            Err(error) if error.code() == codes::PERMISSION_DENIED => {
                return Err(fault::not_found("report"))
            }
            Err(error) => return Err(error),
        };
        self.charge_operator(&resolved, READ_COST).await?;
        Ok(Case::of(self.load(report_id).await?))
    }

    async fn resolve(
        &self,
        operator: &Operator,
        report_id: Id,
        resolution: Resolution,
        reason: Option<&str>,
    ) -> Result<Case> {
        let reason = Self::vet_reason(reason)?;
        let resolved = self.resolve_for_action(operator, Powers::TRIAGE).await?;
        self.charge_operator(&resolved, ACTION_COST).await?;
        let report = self.load(report_id).await?;
        if report.status != report_status::OPEN {
            self.meters.refused(Refused::Settled);
            return Err(fault::conflict("report already resolved"));
        }

        // An escalation leaves the report open, so there is nothing to write to the report
        // row: the audit entry is the whole record of what happened.
        if let Some(status) = resolution.status() {
            self.store
                .resolve_report(
                    report_id,
                    status,
                    resolution.to_i16(),
                    resolved.account_id,
                    resolved.now,
                )
                .await?;
        }

        self.record(
            Trace::operator(&resolved),
            "moderation.report.resolve",
            AuditTargetKind::Report,
            report_id,
            format!("report resolved as {}", resolution.label()),
            reason,
        )
        .await?;

        self.meters.resolved(resolution);
        // Re-read rather than patch the copy in hand. The store is the thing that knows
        // what the row says, and a projection assembled from what this function *intended*
        // to write is how an API starts reporting a state the database is not in.
        Ok(Case::of(self.load(report_id).await?))
    }

    async fn act(
        &self,
        operator: &Operator,
        action: Action,
        reason: Option<&str>,
    ) -> Result<Option<Notice>> {
        let reason = Self::vet_reason(reason)?;
        let resolved = self.resolve_for_action(operator, action.requires()).await?;
        self.charge_operator(&resolved, ACTION_COST).await?;
        // Acting on yourself is either a mistake or somebody covering their tracks by
        // reinstating their own suspended account, and neither is a thing to allow.
        if action.subject_account() == Some(resolved.account_id) {
            self.meters.refused(Refused::Invalid);
            return Err(fault::validation("target", "cannot act on your own account"));
        }

        let summary = self.apply(&action, &resolved).await?;
        self.record(
            Trace::operator(&resolved),
            action.name(),
            action.target_kind(),
            action.target_id(),
            summary,
            reason,
        )
        .await?;

        self.meters.acted(action.index());
        // No reason code on the notice: an action taken outside the queue has no report to
        // take one from, and reading it out of the operator's free text is exactly the
        // thing the notice module refuses to do.
        Ok(Notice::of(&action, None, resolved.now))
    }

    async fn audit(
        &self,
        operator: &Operator,
        target_kind: AuditTargetKind,
        target_id: Id,
        limit: Option<u16>,
    ) -> Result<Vec<AuditEntry>> {
        if target_id.is_nil() {
            self.meters.refused(Refused::Invalid);
            return Err(fault::validation("target_id", "an id is required"));
        }
        let resolved = self.resolve_powers(operator, Powers::AUDIT).await?;
        self.charge_operator(&resolved, AUDIT_COST).await?;
        let rows = self
            .store
            .audit_for_target(target_kind.to_i16(), target_id, self.page(limit))
            .await?;
        self.meters.audit_read();
        Ok(rows)
    }

    async fn assess(
        &self,
        account_id: Id,
        signals: Signals,
        now: Timestamp,
    ) -> Result<Assessment> {
        if account_id.is_nil() {
            return Err(fault::validation("account_id", "an id is required"));
        }
        let since = now.saturating_add_millis(-REPORT_WINDOW_MS);
        let mut signals = signals;
        // The one signal this crate fills in itself, because it is the one that lives in a
        // table it owns. Everything else was counted by the caller for the limiter, and a
        // second set of counters here would be a second set of counters that disagrees.
        signals.reports_against = self
            .store
            .count_reports_about(migo_store::model::report_subject::USER, account_id, since)
            .await?;

        let score = score(&signals, &self.config);
        let risk = risk_of(score, &self.config);
        self.meters.assessed(risk);

        if risk == Risk::Restrict && self.config.auto_suspend {
            // Always for a fixed period, never indefinitely. An automatic decision that
            // never expires is an automatic decision nobody will ever revisit, and the
            // person it was wrong about has no way to wait it out.
            let until = now.saturating_add_millis(self.config.auto_suspend_ms);
            self.store
                .set_status(account_id, AccountStatus::Suspended, Some(until), now)
                .await?;
            // `AuditActorKind::System`, with no `actor_id`. Nobody decided this; a function
            // did, and the audit trail should say so rather than name whichever operator
            // happened to be on shift.
            self.record(
                Trace::system(now),
                "moderation.account.suspend",
                AuditTargetKind::Account,
                account_id,
                format!("automatic suspension at score {score}"),
                None,
            )
            .await?;
            self.meters.auto_suspended();
        }

        Ok(Assessment {
            account_id,
            score,
            risk,
        })
    }
}

impl<S: Store + ?Sized, L: RateLimiter + ?Sized, R: Roster + ?Sized> Moderation<S, L, R> {
    /// Carries out one action and returns the audit summary for it.
    ///
    /// Split from [`Warden::act`] so that the sequence there reads as authorize, charge,
    /// apply, record — and so that the one place a store write happens per action is
    /// visible in a single screen.
    async fn apply(&self, action: &Action, operator: &Operator) -> Result<String> {
        match *action {
            // A warning writes nothing but the audit row. Brief section 49 lists warnings
            // beside audit logs in the dashboard, and a warning *is* an audit entry:
            // `audit_for_target` over the account is the warning history, already indexed
            // and already newest-first. A separate table would be a second history that
            // drifts from the first.
            Action::Warn { account_id } => {
                self.require_account(account_id).await?;
                Ok("account warned".to_string())
            }
            Action::Suspend { account_id, until } => {
                self.require_account(account_id).await?;
                self.store
                    .set_status(account_id, AccountStatus::Suspended, until, operator.now)
                    .await?;
                Ok(match until {
                    Some(until) => format!("account suspended until {}", until.to_rfc3339()),
                    None => "account suspended indefinitely".to_string(),
                })
            }
            Action::Reinstate { account_id } => {
                self.require_account(account_id).await?;
                // `None` for the expiry as well as `Active` for the status. A
                // reinstatement that left `suspended_until` set would leave a date in the
                // row that nothing reads and every future reader misinterprets.
                self.store
                    .set_status(account_id, AccountStatus::Active, None, operator.now)
                    .await?;
                Ok("account reinstated".to_string())
            }
            Action::RemoveMessage {
                conversation_id,
                message_id,
            } => {
                // The tombstone the messaging layer already understands. Nothing here reads
                // the envelope, and for an end-to-end conversation nothing could.
                let removed = self
                    .store
                    .delete_message(conversation_id, message_id, operator.account_id, operator.now)
                    .await?;
                if removed.is_none() {
                    self.meters.refused(Refused::Missing);
                    return Err(fault::not_found("message"));
                }
                Ok("message removed".to_string())
            }
            Action::RemoveMedia { media_id } => {
                if self.store.media(media_id).await?.is_none() {
                    self.meters.refused(Refused::Missing);
                    return Err(fault::not_found("media"));
                }
                // Rejected first, tombstoned second. The scan column is what the media
                // crate consults before it signs a download URL, so a crash between the
                // two leaves an object that is unservable rather than one that is servable
                // and gone.
                self.store
                    .set_media_scan_status(
                        media_id,
                        migo_store::model::media_scan::REJECTED,
                        operator.now,
                    )
                    .await?;
                self.store.delete_media(media_id, operator.now).await?;
                Ok("media removed".to_string())
            }
            Action::ArchiveRoom { room_id } => {
                if self.store.room(room_id).await?.is_none() {
                    self.meters.refused(Refused::Missing);
                    return Err(fault::not_found("room"));
                }
                self.store.archive_room(room_id, operator.now).await?;
                Ok("room archived".to_string())
            }
            Action::DisableBot { bot_id } => {
                if self.store.bot(bot_id).await?.is_none() {
                    self.meters.refused(Refused::Missing);
                    return Err(fault::not_found("bot"));
                }
                // The bot itself, not its owner's account: its token stops authenticating at
                // once and the row survives, so the action is reviewable and reversible.
                self.store
                    .set_bot_disabled(bot_id, Some(operator.now))
                    .await?;
                Ok("bot disabled".to_string())
            }
        }
    }

    /// Refuses unless the account exists.
    ///
    /// Checked before a suspension rather than left to the store, because `set_status` on a
    /// missing account is a no-op in at least one backend and a silent no-op here would
    /// mean an audit row claiming somebody was suspended who never was.
    async fn require_account(&self, account_id: Id) -> Result<()> {
        if self.store.account_by_id(account_id).await?.is_none() {
            self.meters.refused(Refused::Missing);
            return Err(fault::not_found("account"));
        }
        Ok(())
    }
}

/// Builds a fully erased moderation service.
///
/// The form the composition root wants: every part arrives as a trait object, so `migod`
/// can hand over a Postgres store, a Redis-backed limiter, and whichever staff directory
/// this deployment uses without this crate knowing that any of them exist.
#[must_use]
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    roster: SharedRoster,
    random: Box<dyn Random>,
    config: ModerationConfig,
    registry: &Registry,
) -> SharedWarden {
    Arc::new(Moderation::new(
        store, limiter, roster, random, config, registry,
    ))
}

/// The tier a session should be charged at, given an assessment.
///
/// Brief section 50's adaptive rate limiting, in the smallest honest form: the gateway
/// already knows the tier a session would have, and this narrows it. A free function
/// rather than a method because it needs nothing from the service, and because a gateway
/// holding a cached [`Assessment`] should be able to apply it without another call.
#[must_use]
pub fn effective_tier(assessment: &Assessment, tier: TrustTier) -> TrustTier {
    assessment.risk.clamp(tier)
}
