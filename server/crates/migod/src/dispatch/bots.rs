//! The BOTS application opcodes: registering a bot and dispatching a command to one.
//!
//! Two opcodes, each a thin translation from a wire frame onto the bot service. The service
//! owns every rule — the ownership check on management, the existence-and-enabled check on
//! command, the rate charge, the webhook delivery — so these handlers only decode, call, and
//! reply. The shape follows the other dispatch modules exactly: build the
//! [`Caller`](migo_bots::model::Caller), decode the body with [`from_frame`], await the
//! single service method, and [`reply`](ClientContext::reply) with the named response.
//!
//! # Opcode → method map
//!
//! | Opcode         | Wire payload   | Service method     | Response       |
//! |----------------|----------------|--------------------|----------------|
//! | `BOT_REGISTER` | `BotRegister`  | `Bots::register`   | `BotView`      |
//! | `BOT_COMMAND`  | `BotCommand`   | `Bots::command`    | `Acknowledged` |
//!
//! `BOT_REGISTER` is the one call with a structured response: the token is shown to the
//! owner exactly once, here on the registering connection. `BOT_COMMAND` resolves to
//! [`Bots::command`], which delivers the command to the bot's registered webhook (§41) and
//! answers `Acknowledged` — the bot's substantive reply arrives later, from its own
//! account, through the ordinary messaging path.

use migo_bots::model::{Caller as BotCaller, NewBotSpec, Scopes};
use migo_bots::SharedBots;
use migo_core::Error;
use migo_gateway::ClientContext;
use migo_protocol::{fault, from_frame, Acknowledged, BotCommand, BotRegister, BotView, Frame};

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
    let response = BotView {
        bot_id: registered.bot.bot_id,
        username: registered.bot.name,
        // The token is shown to the owner exactly once, here on the registering connection.
        token: Some(registered.token.expose().to_string()),
    };
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
