//! What this crate offers the layer above, and the one port it asks for in return.
//!
//! # The referee is all the client may do
//!
//! Every method of [`Referee`] takes a [`Caller`]: a client starts, lists, reads, plays, and
//! abandons games it is a party to, and the rate limiter charges its budget for each. There is
//! deliberately no method by which a client learns a game's secret, forces a result, or
//! credits itself a reward — those are not rate-limited client actions the crate forgot to
//! guard, they are simply absent. The result of a game is computed by the server and revealed
//! only as a redacted [`GameView`].
//!
//! # Why rewarding does not depend on `migo-economy`
//!
//! A finished game is worth experience, and a won one confers standing. Both are the economy's
//! to grant — but this is a layer-3 crate, and a layer-3 crate reaching into another is how a
//! dependency graph grows a cycle. So the arrow is inverted: this crate defines the [`Rewards`]
//! port it needs, and the composition root satisfies it with an adapter over the economy's
//! server-facing award methods. This crate never learns that `migo-economy` exists, exactly as
//! the economy never learns `migo-notify` does. The anti-farming cap that stops a farmed game
//! minting unbounded experience is not here either; it lives in the economy, behind the port,
//! read from durable rows so a restart cannot reset it.
//!
//! # Why the port cannot pay money
//!
//! The port grants experience and marks a winner. It has no method that moves currency,
//! because a game that paid out currency would be a game one could cash out of, and sections
//! 37 and 87 forbid that without a regulatory review this crate is in no position to have
//! done. Standing is a badge, not a balance; experience is capped and non-spendable. Neither
//! is money, and there is no third method.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};

use crate::model::{Caller, GameInfo, GameKind, GameSummary, GameView, Move, MoveResult};

/// A shared referee, the shape the layer above holds.
pub type SharedReferee = Arc<dyn Referee>;

/// A shared rewards port.
pub type SharedRewards = Arc<dyn Rewards>;

/// Mini-games, as the layer above reaches them.
///
/// The whole of brief sections 38 to 42 behind one erased trait: start a game in a
/// conversation, see what is being played, take a turn, and leave. Authorization is the same
/// for all of them — the caller must be a member of the conversation the game lives in — and
/// it is enforced here against the store, not trusted from the caller.
#[async_trait]
pub trait Referee: Send + Sync {
    /// The games this deployment offers, for a client to build a menu from.
    ///
    /// Synchronous and uncharged: the catalogue is fixed in code, not a store read, so listing
    /// it costs nothing and cannot fail.
    fn catalogue(&self) -> Vec<GameInfo>;

    /// Starts a game of `kind` in a conversation, between the caller and `opponents`.
    ///
    /// The caller is always a player; `opponents` are the others, and their count must make
    /// the kind's player total ([`GameKind::min_players`]) — one opponent for a two-player
    /// game, none for a single-player one. Every named player, the caller included, must be a
    /// member of the conversation, or the start is refused. Returns the opening view.
    async fn start(
        &self,
        caller: &Caller,
        conversation_id: Id,
        kind: GameKind,
        opponents: &[Id],
    ) -> Result<GameView>;

    /// The open games in a conversation the caller belongs to, newest first.
    async fn active(&self, caller: &Caller, conversation_id: Id) -> Result<Vec<GameSummary>>;

    /// One game, as this caller is allowed to see it.
    ///
    /// A member of the conversation may watch a game they are not playing; the view they get
    /// is redacted the same way a player's is, so watching leaks nothing a player could not
    /// already see. A caller who is not a member is told the game does not exist.
    async fn view(&self, caller: &Caller, game_id: Id) -> Result<GameView>;

    /// Plays one move, returning the mover's fresh view and the deltas to broadcast.
    ///
    /// The server decides everything: whether it is the caller's turn, whether the move is
    /// legal for the kind, what it does, and whether the game is now over. A move against a
    /// board another move has already changed is retried against the fresh state and either
    /// applied — a genuine concurrent move — or rejected as illegal, which is what a replay
    /// becomes once the state already reflects it.
    async fn play(&self, caller: &Caller, game_id: Id, mv: Move) -> Result<MoveResult>;

    /// Abandons a game the caller is playing — a forfeit, or a cleanup on disconnect.
    ///
    /// Terminal: the game ends [`crate::model::GameStatus::Abandoned`] with no winner and no
    /// reward. Only a player may abandon a game, and only while it is still open.
    async fn abandon(&self, caller: &Caller, game_id: Id) -> Result<GameView>;
}

/// The port through which a finished game is turned into experience and standing.
///
/// Two methods, because there are two things a result is worth: experience, credited to each
/// player, and a winner's standing, conferred on whoever won. An implementation bridges to
/// whatever the deployment uses to reward — in production, an adapter over the economy's
/// `award` and `award_badge`; in a test, a recorder. Every call names the `game_id` so the
/// adapter can build a deterministic idempotency key from it: a reward is credited once per
/// game per account, and a retry that carries the same key returns the first credit rather
/// than doubling it. An `Err` is the rewarding layer failing, and the service treats it the
/// way the economy treats a push that would not send — logged and swallowed — because a game
/// that could not be rewarded is still a game that was played and decided.
#[async_trait]
pub trait Rewards: Send + Sync {
    /// Credits an account experience for a game it finished, identified for idempotency by the
    /// game it came from.
    async fn award_experience(
        &self,
        account_id: Id,
        amount: i64,
        game_id: Id,
        at: Timestamp,
    ) -> Result<()>;

    /// Marks that an account won a game — the adapter decides what standing that confers, a
    /// badge in the shipped case. Idempotent per game per account.
    async fn mark_winner(&self, account_id: Id, game_id: Id, at: Timestamp) -> Result<()>;
}

/// A rewards port that grants nothing.
///
/// The default when a deployment has wired no economy, which is the normal state of a
/// development machine and of every test that is not testing rewards. Dropping the reward is
/// safe because the game's result does not depend on it: the game is decided and recorded
/// authoritatively whether or not the experience for it was ever credited.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unrewarded;

#[async_trait]
impl Rewards for Unrewarded {
    async fn award_experience(&self, _: Id, _: i64, _: Id, _: Timestamp) -> Result<()> {
        Ok(())
    }

    async fn mark_winner(&self, _: Id, _: Id, _: Timestamp) -> Result<()> {
        Ok(())
    }
}
