//! What this crate offers the layer above: a bot's whole lifecycle behind one erased trait.
//!
//! # Two audiences, one trait
//!
//! [`Bots`] serves two callers that never overlap. An **owner** — a human — registers a bot,
//! rotates its token, widens or narrows its scopes, pauses it, and lists what they own; every
//! one of those takes a [`Caller`] and is charged against that human's budget. The
//! **gateway** calls exactly one method, [`Bots::authenticate`], on the token a connecting
//! bot presents, and gets back a [`BotIdentity`] to build the bot's request context from.
//!
//! There is deliberately no method by which a bot manages itself, reads a conversation, or
//! grants itself a scope: those are not owner actions the trait forgot to guard, they are
//! simply absent. A bot's whole vocabulary is the token it presents; everything it may then
//! *do* is the messaging, rooms, and games surfaces, gated by the scopes this trait reports.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::{Id, Result, Secret};

use crate::model::{BotIdentity, BotView, Caller, NewBotSpec, Registered, Scopes};

/// A shared bot subsystem, the shape the layer above holds.
pub type SharedBots = Arc<dyn Bots>;

/// The bot subsystem, as the layer above reaches it.
///
/// Ownership is enforced here, not trusted from the caller: every management method resolves
/// the bot and refuses — as [`fault::not_found`](migo_protocol::fault::not_found), hiding the
/// bot's existence per brief section 48 — if the caller is not its owner. Authentication is
/// resolved against the stored keyed tag; a token that matches nothing, and one whose bot is
/// disabled, both fail with the same opaque error, because any difference between them is a
/// valid-token oracle (section 161).
#[async_trait]
pub trait Bots: Send + Sync {
    /// Registers a new bot owned by `owner`, returning the view and the token shown once.
    ///
    /// Creates the backing account, its profile, and the bot row in one store write. The
    /// username is validated exactly as a human registration's would be, the display name and
    /// webhook URL locally; the owner's bot count is capped
    /// ([`BotsConfig::max_bots_per_owner`](crate::model::BotsConfig::max_bots_per_owner)). The
    /// returned [`Registered::token`] is the only time the token is ever available.
    async fn register(&self, owner: &Caller, spec: NewBotSpec) -> Result<Registered>;

    /// Resolves a bearer token to the bot's identity, for the gateway.
    ///
    /// Uncharged and read-only: authentication happens once per connection and the gateway
    /// prices the connection itself. Fails identically for an unknown token and a disabled
    /// bot — the caller learns only that the token is not usable, never which of the two it
    /// was.
    async fn authenticate(&self, token: &str) -> Result<BotIdentity>;

    /// Rotates a bot's token, invalidating the old one and returning the new one once.
    ///
    /// The path an owner takes after a leak, or a lost token: the previous token stops
    /// authenticating the instant the new tag is written. Only the bot's owner may rotate it.
    async fn rotate_token(&self, owner: &Caller, bot_id: Id) -> Result<Secret>;

    /// Sets a bot's permission scopes to exactly `scopes`, returning the updated view.
    ///
    /// A full replacement, not a delta: the owner sends the set they want the bot to have,
    /// which is the shape that cannot leave a stale bit set by accident. Only the owner may
    /// change them.
    async fn set_scopes(&self, owner: &Caller, bot_id: Id, scopes: Scopes) -> Result<BotView>;

    /// Pauses or resumes a bot at its owner's request, returning the updated view.
    ///
    /// `paused` true disables it — its token stops authenticating and its row survives so
    /// history stays intact; `false` re-enables it. This is the owner's own control, distinct
    /// from a moderator's `DisableBot`, though both land on the same stored column.
    async fn set_paused(&self, owner: &Caller, bot_id: Id, paused: bool) -> Result<BotView>;

    /// The bots `owner` owns, newest first.
    async fn list(&self, owner: &Caller) -> Result<Vec<BotView>>;

    /// One bot the caller owns, or [`fault::not_found`](migo_protocol::fault::not_found) if it
    /// is not theirs or does not exist.
    async fn get(&self, owner: &Caller, bot_id: Id) -> Result<BotView>;
}
