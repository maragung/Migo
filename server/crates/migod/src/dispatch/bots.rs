//! The BOTS application opcodes: registering a bot and dispatching a command to one.
//!
//! Two opcodes, each a thin translation from a wire frame onto the bot service. The service
//! owns every rule — the owner check, the rate charge, the default-empty scopes — so these
//! handlers only decode, call, and reply. The shape follows the other dispatch modules
//! exactly: build the [`Caller`](migo_bots::model::Caller), decode the body with
//! [`from_frame`], await the single service method, and [`reply`](ClientContext::reply) with
//! the named response.
//!
//! # Opcode → method map
//!
//! | Opcode         | Wire payload   | Service method        | Response              |
//! |----------------|----------------|-----------------------|-----------------------|
//! | `BOT_REGISTER` | `BotRegister`  | `Bots::register`      | `BotView`             |
//! | `BOT_COMMAND`  | `BotCommand`   | — (no inbound channel)| `Acknowledged`        |
//!
//! `BOT_REGISTER` is the one call with a backing service method. The bot subsystem has no
//! inbound command channel of its own: a bot acts through the messaging, rooms, and games
//! surfaces, gated by the scopes its token reports, and those crates already route its
//! `send`/`typing`/`play` actions. So `BOT_COMMAND` decodes the request — proving the frame
//! is well-formed — and acknowledges; the substantive effect of a bot's "command" is carried
//! by the other domains' handlers, not a method on `Bots`, and no `BotView` is defined for it,
//! so the reply is [`Acknowledged`](migo_protocol::Acknowledged) per the dispatch contract.

use migo_bots::model::{Caller as BotCaller, NewBotSpec, Scopes};
use migo_bots::SharedBots;
use migo_core::Error;
use migo_gateway::ClientContext;
use migo_protocol::{fault, from_frame, Acknowledged, BotCommand, BotRegister, BotView, Frame};

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
    let identity = ctx.identity();
    let caller = BotCaller {
        account_id: identity.account_id(),
        device_id: identity.device_id(),
        tier: identity.tier,
        now: ctx.now(),
        request_id: None,
    };
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
/// The bot subsystem exposes no inbound command method — a bot's actions are the ordinary
/// messaging/rooms/games operations gated by its scopes — so there is no `Bots` call to make.
/// The frame is still decoded to enforce a well-formed request, and the reply is
/// [`Acknowledged`](migo_protocol::Acknowledged) because no structured `BotView` is defined
/// for this opcode.
pub(crate) async fn handle_command(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedBots,
) -> Result<(), Error> {
    let _request: BotCommand = from_frame(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}
