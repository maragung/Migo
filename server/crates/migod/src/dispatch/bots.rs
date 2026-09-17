//! The BOTS application opcodes: registering a bot, dispatching a command to one, and the four
//! calls an owner manages one with.
//!
//! Six opcodes, each a thin translation from a wire frame onto the bot service. The service
//! owns every rule — the ownership check on management, the existence-and-enabled check on
//! command, the rate charge, the webhook delivery — so these handlers only decode, call, and
//! reply. The shape follows the other dispatch modules exactly: build the
//! [`Caller`](migo_bots::model::Caller), decode the body with [`from_frame`], await the
//! single service method, and [`reply`](ClientContext::reply) with the named response.
//!
//! # Opcode → method map
//!
//! | Opcode         | Wire payload    | Service method       | Response           |
//! |----------------|-----------------|----------------------|--------------------|
//! | `BOT_REGISTER` | `BotRegister`   | `Bots::register`     | `BotView`          |
//! | `BOT_COMMAND`  | `BotCommand`    | `Bots::command`      | `Acknowledged`     |
//! | `BOT_LIST`     | `BotListReq`    | `Bots::list`         | `BotListResponse`  |
//! | `BOT_ROTATE`   | `BotRotate`     | `Bots::rotate_token` | `BotView`          |
//! | `BOT_PAUSE`    | `BotPause`      | `Bots::set_paused`   | `BotView`          |
//! | `BOT_SCOPES`   | `BotScopes`     | `Bots::set_scopes`   | `BotView`          |
//!
//! The three calls that return a token or a changed bot all answer `BotView`, because an owner
//! who has just changed something needs to see the result and a second read to learn it would
//! be a round trip that can fail on its own. `BOT_REGISTER` is the one call that *mints* a
//! token — shown on the registering connection exactly once, here. `BOT_COMMAND` resolves to
//! [`Bots::command`], which delivers the command to the bot's registered webhook (§41) and
//! answers `Acknowledged` — the bot's substantive reply arrives later, from its own
//! account, through the ordinary messaging path.
//!
//! # What is deliberately not here
//!
//! `Bots::get` reads one bot by id and stays unwired. Every bot `get` can answer for is a bot
//! the caller owns, and an owner owns at most
//! [`max_bots_per_owner`](migo_bots::model::BotsConfig::max_bots_per_owner) of them, so
//! `BOT_LIST` already answers the question in full: a second opcode that could only ever
//! return a row the list had just returned would be a second way to ask one question, and a
//! second thing to keep in step. The method stays on the trait because the crate's own callers
//! and tests use it.

use migo_bots::model::{Caller as BotCaller, NewBotSpec, Scopes};
use migo_bots::{BotView as BotViewCrate, SharedBots};
use migo_core::Error;
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, Acknowledged, BotCommand, BotListReq, BotListResponse, BotPause,
    BotRegister, BotRotate, BotScopes, BotView, Frame,
};

/// Builds the caller every bots handler needs: the authenticated account and device, the
/// trust tier, and the one sampled `now`.
fn caller(ctx: &ClientContext<'_>) -> BotCaller {
    let identity = ctx.identity();
    BotCaller {
        account_id: identity.account_id(),
        device_id: identity.device_id(),
        tier: identity.tier,
        now: ctx.now(),
        request_id: None,
    }
}

/// Projects a crate bot onto the wire view.
///
/// The two names differ on purpose and the difference is the whole reason this function
/// exists: the crate calls the field `name` and the wire keeps that word, because the value is
/// the bot's display name and not the backing account's handle — the account has a username of
/// its own that this view never carried. `token` is a parameter rather than a field of the
/// crate view because the crate holds the plaintext only at the two moments it mints one, and
/// `paused` and `scopes` are projections of `disabled` and the bitmask, so that the wire
/// spells a paused bot and a permission set the way an operator reads them rather than the way
/// a store column stores them.
fn view_of(bot: &BotViewCrate, token: Option<String>) -> BotView {
    BotView {
        bot_id: bot.bot_id,
        name: bot.name.clone(),
        token,
        paused: Some(bot.disabled),
        scopes: Some(bot.scopes.slugs().into_iter().map(str::to_owned).collect()),
    }
}

/// Parses the permission slugs a caller asked for.
///
/// An unknown slug is a validation error and not a dropped bit. Dropping it would answer a
/// request for `moderate` with a bot that silently lacks it, and the owner would learn the
/// difference only when the bot failed to do the thing it was granted — so a build that does
/// not know a name says so instead of quietly granting less than it was asked to.
fn scopes_from_slugs(slugs: &[String]) -> Result<Scopes, Error> {
    let mut scopes = Scopes::NONE;
    for slug in slugs {
        let one = Scopes::from_slug(slug)
            .ok_or_else(|| fault::validation("scopes", &format!("unknown bot scope {slug:?}")))?;
        scopes = scopes.with(one);
    }
    Ok(scopes)
}

/// Registers a new bot owned by the authenticated caller and replies with its `BotView`.
///
/// The wire [`BotRegister`] carries only the bot's `username` and `display_name`; the crate's
/// `register` takes a [`NewBotSpec`], whose remaining fields are not on the wire. Section 41
/// mandates that a freshly registered bot hold the minimum authority, which is none, so the
/// `scopes` the owner did not specify default to [`Scopes::NONE`] and the owner widens them
/// deliberately afterwards. The returned token is shown to the owner exactly once, here on
/// the registering connection, via the wire `BotView`'s `token` field.
pub(crate) async fn handle_register(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedBots,
) -> Result<(), Error> {
    let caller = caller(ctx);
    let request: BotRegister = from_frame(frame).map_err(fault::from_wire)?;
    let spec = NewBotSpec {
        username: request.username,
        display_name: request.display_name,
        // The wire carries no scopes; the section 41 minimum is nothing, granted deliberately.
        scopes: Scopes::NONE,
        webhook_url: None,
        locale: None,
    };
    let registered = svc.register(&caller, spec).await?;
    let response = view_of(
        &registered.bot,
        // The token is shown to the owner exactly once, here on the registering connection.
        Some(registered.token.expose().to_string()),
    );
    ctx.reply(&response)
}

/// Dispatches a command to a bot and acknowledges.
///
/// The service delivers the command to the bot's registered webhook and refuses — `NOT_FOUND`
/// for an unknown or paused bot, a validation error when no webhook is registered, one opaque
/// error when the webhook is unreachable — so whatever happens, the caller learns of it here.
/// The bot's reply is a separate arrival: the bot speaks through its own account, on the
/// ordinary messaging path, like every other participant.
pub(crate) async fn handle_command(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedBots,
) -> Result<(), Error> {
    let caller = caller(ctx);
    let request: BotCommand = from_frame(frame).map_err(fault::from_wire)?;
    svc.command(
        &caller,
        request.bot_id,
        &request.command,
        &request.args.unwrap_or_default(),
    )
    .await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Lists every bot the caller owns.
///
/// The request carries no owner because the session is the owner: a body naming whose bots to
/// list would be a parameter the caller could only ever get wrong, and one the handler would
/// have to remember to ignore. The order is the store's, which is oldest first, so a
/// management screen listing the answer twice sees the same order twice.
pub(crate) async fn handle_list(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedBots,
) -> Result<(), Error> {
    let caller = caller(ctx);
    // Decoded even though it carries nothing, so a body with trailing bytes is refused here
    // rather than accepted as an empty request.
    let _request: BotListReq = from_frame(frame).map_err(fault::from_wire)?;
    let bots = svc.list(&caller).await?;
    let response = BotListResponse {
        bots: bots.iter().map(|bot| view_of(bot, None)).collect(),
    };
    ctx.reply(&response)
}

/// Mints a fresh token for a bot and replies with the view that shows it.
///
/// The old token stops working the moment this returns, so a caller that loses the reply has
/// lost the bot's credential and must rotate again — which is the intended shape rather than a
/// hazard, because the alternative is a store that can hand a token back a second time, and
/// section 77 says the store keeps only a tag. `NOT_FOUND` covers both an id that names no bot
/// and an id that names someone else's, so an owner probing ids learns nothing about other
/// people's bots. The view comes back from the rotation itself rather than from a second read,
/// so one call is one charge and one round trip to the store.
pub(crate) async fn handle_rotate(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedBots,
) -> Result<(), Error> {
    let caller = caller(ctx);
    let request: BotRotate = from_frame(frame).map_err(fault::from_wire)?;
    let rotated = svc.rotate_token(&caller, request.bot_id).await?;
    ctx.reply(&view_of(
        &rotated.bot,
        Some(rotated.token.expose().to_string()),
    ))
}

/// Pauses a bot or resumes it, and replies with the view that shows which.
///
/// A paused bot refuses to authenticate, so it stops speaking without losing its row, its
/// scopes, or its webhook — which is what makes pausing the reversible answer to a bot that is
/// misbehaving, and why section 49's report offers it. Resuming is the same call with the flag
/// the other way, so a client needs one control rather than two opcodes that could disagree.
pub(crate) async fn handle_pause(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedBots,
) -> Result<(), Error> {
    let caller = caller(ctx);
    let request: BotPause = from_frame(frame).map_err(fault::from_wire)?;
    let bot = svc
        .set_paused(&caller, request.bot_id, request.paused)
        .await?;
    ctx.reply(&view_of(&bot, None))
}

/// Replaces a bot's permissions with exactly the set named.
///
/// A replacement and not a delta: the request lists every slug the bot should hold, so a
/// client that has just shown an owner a set of checkboxes sends the boxes that are ticked
/// and nothing else. A delta would need both add and remove lists and would leave two clients
/// editing the same bot able to interleave into a set neither owner asked for.
pub(crate) async fn handle_scopes(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedBots,
) -> Result<(), Error> {
    let caller = caller(ctx);
    let request: BotScopes = from_frame(frame).map_err(fault::from_wire)?;
    let scopes = scopes_from_slugs(&request.scopes)?;
    let bot = svc.set_scopes(&caller, request.bot_id, scopes).await?;
    ctx.reply(&view_of(&bot, None))
}
