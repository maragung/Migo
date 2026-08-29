//! The MODERATION domain SPEC opcodes: filing a report and acting on one.
//!
//! Two opcodes, two callers. `REPORT_CREATE` is a user — anybody with a session — pointing
//! the warden at something; it builds a [`Caller`](migo_moderation::Caller) and calls
//! [`Warden::file_report`]. `MODERATION_ACTION` is a member of staff closing a case; it builds
//! an [`Operator`](migo_moderation::Operator) and calls [`Warden::resolve`]. The operator's
//! powers are *not* taken from the request — the service re-resolves them from its roster on
//! every call and throws the struct's field away — so the only thing this layer contributes is
//! the account, device, and whether the session proved a factor recently enough.

use migo_core::Error;
use migo_core::Id;
use migo_gateway::ClientContext;
use migo_moderation::SharedWarden;
use migo_moderation::Caller as WardenCaller;
use migo_moderation::Filing;
use migo_moderation::Operator;
use migo_moderation::Powers;
use migo_moderation::Reason;
use migo_moderation::Resolution;
use migo_moderation::Subject;
use migo_protocol::{from_frame, fault, Acknowledged, Frame, ModAction, ReportFile};

/// Files a report on behalf of the authenticated account.
///
/// The wire carries a subject kind and id plus a reason code, never the content: a report is a
/// pointer, and copying private ciphertext into a moderation table would defeat the point of
/// encrypting it. The caller is the session, the cost is charged to the account, and an `ok`
/// acknowledgement is all the client needs — idempotency means a repeat filing returns the same
/// report rather than an error.
pub(crate) async fn handle_report(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedWarden,
) -> Result<(), Error> {
    let identity = ctx.identity();
    let now = ctx.now();

    let caller = WardenCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);

    let request: ReportFile = from_frame(frame).map_err(fault::from_wire)?;
    let subject = subject_from_wire(request.subject_kind, request.subject_id)?;
    let reason = Reason::of_i16(request.reason as i16);

    let filing = match request.note {
        Some(note) => Filing::new(subject, reason).with_note(note),
        None => Filing::new(subject, reason),
    };

    svc.file_report(&caller, filing).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Acts on a report (case) on behalf of a member of staff.
///
/// The wire names the case and a decision code. The decision is a [`Resolution`], the case is the
/// `report_id` the service mints, and the operator is the session — with the powers the roster
/// grants and the re-authentication the service demands of every action. There is no note field on
/// the wire, so the audit entry this writes carries none of the operator's own words.
pub(crate) async fn handle_action(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedWarden,
) -> Result<(), Error> {
    let identity = ctx.identity();
    let now = ctx.now();

    let operator = if identity.is_fresh(now) {
        Operator::new(identity.account_id(), identity.device_id(), Powers::NONE, now)
            .reauthenticated()
    } else {
        Operator::new(identity.account_id(), identity.device_id(), Powers::NONE, now)
    };

    let request: ModAction = from_frame(frame).map_err(fault::from_wire)?;
    let resolution = Resolution::of_i16(request.action as i16);

    svc.resolve(&operator, request.case_id, resolution, None).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Projects the wire's subject kind and id onto the domain [`Subject`].
///
/// The wire encodes only a single id per subject, so the four kinds it names that fold onto one are
/// direct; a message report would carry its conversation and message as two ids, which the frozen
/// `ReportFile` does not, so the single id stands in for both halves of the key here. An unknown
/// kind is a client fault, never a panic.
fn subject_from_wire(kind: u32, id: Id) -> Result<Subject, Error> {
    match kind {
        0 => Ok(Subject::User(id)),
        1 => Ok(Subject::Message {
            conversation_id: id,
            message_id: id,
        }),
        2 => Ok(Subject::Room(id)),
        3 => Ok(Subject::Bot(id)),
        _ => Err(fault::validation("subject_kind", "unknown subject kind")),
    }
}

