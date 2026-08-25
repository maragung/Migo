//! The referee, implemented: thin over the store, authoritative over the client.
//!
//! # Why the service is thin over the store
//!
//! The one rule that must hold even if this process crashes mid-move lives in the store, in a
//! single compare-and-set: a game advances only if it is still open and still carries the
//! token the move was computed against ([`migo_store::traits::GameStore::advance_game`]). This
//! service does not add a lock of its own, because a lock here is a lock a second writer races
//! past. It loads a game, hands the bytes to the right `engine`, and writes the result back
//! under that compare-and-set. Everything that makes the awkward cases fall out for free is a
//! consequence of that one guard:
//!
//! * A **replayed** move re-reads the game, and the engine re-validates it against the *fresh*
//!   state — where the cell is now taken, or the hand is now committed — and refuses it. The
//!   replay never even reaches the store.
//! * Two **genuinely concurrent** commits (rock-paper-scissors) both compute against the same
//!   round; the store lets exactly one through and answers the other with `None`. This service
//!   re-reads and re-applies the loser against the now-half-committed round, and it succeeds.
//!   The retry is bounded, so a pathologically contended game fails closed with a conflict
//!   rather than spinning.
//! * A move against an **already-finished** game finds the status no longer open — at the
//!   read, or at the compare-and-set if it finished in between — and is refused.
//!
//! # Authorisation is read from the store, not trusted from the caller
//!
//! Who may start, see, or play a game is decided by conversation membership, read from the
//! same store ([`migo_store::traits::MessagingStore::is_member`]). A caller who is not a
//! member of a game's conversation is told the game does not exist (section 48), so the
//! endpoint leaks nothing about games in conversations the caller cannot see. A member who is
//! not a *player* may watch but not move.
//!
//! # Rewards are inverted, and cannot pay money
//!
//! A finished game is turned into experience and standing through the [`Rewards`] port this
//! crate owns; the composition root satisfies it with the economy. The reward is best-effort —
//! a game that could not be rewarded is still a game that was decided and recorded — and it
//! can never be currency, because the port has no method that moves any.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use migo_core::metrics::Registry;
use migo_core::{Error, Id, OsRandom, Random, Result, Timestamp};
use migo_protocol::fault;
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::{game_status, AdvanceGame, GameSession, NewGame};
use migo_store::{SharedStore, Store};

use crate::engine::{engine, ApplyError, Corrupt, Reject};
use crate::metrics::{Conclusion, Meters, Rejection};
use crate::model::{
    Caller, GameInfo, GameKind, GameStatus, GameSummary, GameView, GamesConfig, Move, MoveResult,
    Outcome, MAX_ACTIVE_GAMES,
};
use crate::traits::{Referee, Rewards, SharedReferee, SharedRewards};

/// What starting a game costs the caller's rate-limit budget. A write, and dearer than a move:
/// it is the point at which a flood of empty games would be created, so it is the point to
/// price.
const START_COST: u32 = 10;
/// What one move costs. This is the game's cooldown (section 41): a client cannot play faster
/// than its budget refills, and there is no separate per-game timer to keep.
const PLAY_COST: u32 = 4;
/// What reading one game costs.
const READ_COST: u32 = 3;
/// What listing a conversation's open games costs.
const LIST_COST: u32 = 3;
/// What abandoning a game costs. Priced like a move: it is a write that ends a game.
const ABANDON_COST: u32 = 4;

/// The referee.
///
/// Generic over its collaborators so a test can supply an in-memory store, a permissive
/// limiter, and a recording rewards port, while production erases them to trait objects. The
/// defaults are those trait objects, so the ordinary spelling is simply `Games`.
pub struct Games<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter, R: ?Sized = dyn Rewards> {
    store: Arc<S>,
    limiter: Arc<L>,
    rewards: Arc<R>,
    config: GamesConfig,
    /// The server's randomness, behind a lock because [`Random`] takes `&mut self` and the
    /// service is shared. Only game creation draws from it (a guessing game's secret).
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
}

impl<S, L, R> Games<S, L, R>
where
    S: Store + ?Sized,
    L: RateLimiter + ?Sized,
    R: Rewards + ?Sized,
{
    /// Assembles a referee from its collaborators.
    pub fn new(
        store: Arc<S>,
        limiter: Arc<L>,
        rewards: Arc<R>,
        config: GamesConfig,
        random: Box<dyn Random>,
        registry: &Registry,
    ) -> Self {
        Self {
            store,
            limiter,
            rewards,
            config,
            random: Mutex::new(random),
            meters: Meters::new(registry),
        }
    }

    /// A fresh, time-ordered id, drawn under the randomness lock.
    fn new_id(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, &mut **random)
    }

    /// Charges the caller `cost` against their account budget, or returns a rate-limit error.
    async fn charge(&self, caller: &Caller, cost: u32) -> Result<()> {
        self.limiter
            .charge(
                &[BucketKey::account(caller.account_id)],
                cost,
                caller.tier,
                caller.now,
            )
            .await?
            .into_result()
    }

    /// Refuses unless the caller is a member of the conversation, hiding its existence
    /// otherwise (section 48).
    async fn require_member(&self, caller: &Caller, conversation_id: Id) -> Result<()> {
        if self
            .store
            .is_member(conversation_id, caller.account_id)
            .await?
        {
            Ok(())
        } else {
            Err(fault::not_found("conversation"))
        }
    }

    /// Turns a finished game into experience and standing, best-effort. A failure of the port
    /// is counted and logged, never propagated: the game is already decided and recorded.
    async fn reward(&self, players: &[Id], outcome: Outcome, game_id: Id, at: Timestamp) {
        for &player in players {
            let amount = match outcome {
                Outcome::Win { winner } if winner == player => self.config.win_experience,
                _ => self.config.finish_experience,
            };
            if amount > 0 {
                if let Err(error) = self
                    .rewards
                    .award_experience(player, amount, game_id, at)
                    .await
                {
                    self.meters.reward_dropped();
                    tracing::warn!(code = error.code(), "awarding game experience failed");
                }
            }
        }
        if let Outcome::Win { winner } = outcome {
            if let Err(error) = self.rewards.mark_winner(winner, game_id, at).await {
                self.meters.reward_dropped();
                tracing::warn!(code = error.code(), "marking a game winner failed");
            }
        }
    }
}

/// A stored state that would not decode: this crate wrote it, so it is an internal error, and
/// the caller never sees the detail (section 161).
fn corrupt(_: Corrupt) -> Error {
    fault::internal("a game's stored state could not be decoded")
}

/// Maps a refused move to the metric to count it under and the error to return the caller.
fn reject_to_fault(reject: Reject) -> (Rejection, Error) {
    match reject {
        Reject::NotAPlayer => (Rejection::NotAPlayer, fault::permission_denied("game")),
        Reject::NotYourTurn => (Rejection::NotYourTurn, fault::conflict("turn")),
        Reject::IllegalMove(why) => (Rejection::IllegalMove, fault::validation("move", why)),
        Reject::WrongKind => (
            Rejection::WrongKind,
            fault::validation("move", "this move does not fit the game"),
        ),
    }
}

/// The kind of a persisted game, or an internal error if its integer is one this build does not
/// know.
fn kind_of(session: &GameSession) -> Result<GameKind> {
    GameKind::from_i16(session.kind).ok_or_else(|| fault::internal("unknown game kind"))
}

/// The status of a persisted game, or an internal error if its integer is unknown.
fn status_of(session: &GameSession) -> Result<GameStatus> {
    GameStatus::from_i16(session.status).ok_or_else(|| fault::internal("unknown game status"))
}

/// Builds the view of a game as `viewer` may see it, redaction and all.
fn build_view(viewer: Id, session: &GameSession) -> Result<GameView> {
    let kind = kind_of(session)?;
    let status = status_of(session)?;
    let decoded = engine(kind).decode(&session.state).map_err(corrupt)?;
    let render = engine(kind)
        .render(&session.state, viewer)
        .map_err(corrupt)?;
    // The store's status is authoritative for whether a game is over; the state decides the
    // outcome of a game finished by play, and an abandoned game has no winner.
    let (turn_of, outcome) = match status {
        GameStatus::Open => (decoded.turn_of, None),
        GameStatus::Finished => (None, decoded.outcome),
        GameStatus::Abandoned => (None, Some(Outcome::NoContest)),
    };
    Ok(GameView {
        game_id: session.game_id,
        kind,
        conversation_id: session.conversation_id,
        status,
        your_turn: turn_of == Some(viewer),
        players: decoded.players,
        turn_of,
        outcome,
        render,
    })
}

#[async_trait]
impl<S, L, R> Referee for Games<S, L, R>
where
    S: Store + ?Sized,
    L: RateLimiter + ?Sized,
    R: Rewards + ?Sized,
{
    fn catalogue(&self) -> Vec<GameInfo> {
        GameKind::ALL.iter().copied().map(GameInfo::of).collect()
    }

    async fn start(
        &self,
        caller: &Caller,
        conversation_id: Id,
        kind: GameKind,
        opponents: &[Id],
    ) -> Result<GameView> {
        self.charge(caller, START_COST).await?;
        self.require_member(caller, conversation_id).await?;

        // The caller is a player; the opponents are the rest. The count must be exactly the
        // kind's, and every player distinct.
        let wanted = usize::from(kind.min_players());
        if opponents.len() + 1 != wanted {
            return Err(fault::validation(
                "opponents",
                "wrong number of players for this game",
            ));
        }
        let mut players = Vec::with_capacity(wanted);
        players.push(caller.account_id);
        for &opponent in opponents {
            if players.contains(&opponent) {
                return Err(fault::validation("opponents", "a player is named twice"));
            }
            self.require_member(caller, conversation_id).await?;
            if !self.store.is_member(conversation_id, opponent).await? {
                return Err(fault::validation(
                    "opponents",
                    "a player is not in the conversation",
                ));
            }
            players.push(opponent);
        }

        let game_id = self.new_id(caller.now);
        let created = {
            let mut random = self.random.lock();
            engine(kind).create(&players, &self.config, &mut **random)
        };
        let session = self
            .store
            .create_game(NewGame {
                game_id,
                kind: kind.to_i16(),
                conversation_id,
                state: created.state,
                turn_of: created.turn_of,
                // No stake: the default games wager nothing (sections 37 and 87).
                stake_currency: None,
                stake_amount: None,
                at: caller.now,
            })
            .await?;
        self.meters.started(kind);
        build_view(caller.account_id, &session)
    }

    async fn active(&self, caller: &Caller, conversation_id: Id) -> Result<Vec<GameSummary>> {
        self.charge(caller, LIST_COST).await?;
        self.require_member(caller, conversation_id).await?;
        let sessions = self
            .store
            .active_games(conversation_id, MAX_ACTIVE_GAMES)
            .await?;
        let mut out = Vec::with_capacity(sessions.len());
        for session in &sessions {
            let kind = kind_of(session)?;
            let decoded = engine(kind).decode(&session.state).map_err(corrupt)?;
            out.push(GameSummary {
                game_id: session.game_id,
                kind,
                status: status_of(session)?,
                players: decoded.players,
                turn_of: decoded.turn_of,
            });
        }
        Ok(out)
    }

    async fn view(&self, caller: &Caller, game_id: Id) -> Result<GameView> {
        self.charge(caller, READ_COST).await?;
        let session = self
            .store
            .game(game_id)
            .await?
            .ok_or_else(|| fault::not_found("game"))?;
        self.require_member(caller, session.conversation_id).await?;
        build_view(caller.account_id, &session)
    }

    async fn play(&self, caller: &Caller, game_id: Id, mv: Move) -> Result<MoveResult> {
        self.charge(caller, PLAY_COST).await?;
        // Re-read and re-apply for as many rounds as the budget allows. A lost compare-and-set
        // is a genuine concurrent move, not a client error, so it is retried rather than
        // rejected; a replay is caught by the engine on the re-read, not here.
        for _ in 0..=self.config.retry_budget {
            let session = self
                .store
                .game(game_id)
                .await?
                .ok_or_else(|| fault::not_found("game"))?;
            self.require_member(caller, session.conversation_id).await?;
            if !status_of(&session)?.is_open() {
                return Err(fault::conflict("game"));
            }
            let kind = kind_of(&session)?;

            let applied = match engine(kind).apply(
                game_id,
                &session.state,
                caller.account_id,
                mv,
                &self.config,
            ) {
                Ok(applied) => applied,
                Err(ApplyError::Corrupt) => return Err(corrupt(Corrupt)),
                Err(ApplyError::Reject(reject)) => {
                    let (rejection, error) = reject_to_fault(reject);
                    self.meters.rejected(rejection);
                    return Err(error);
                }
            };

            let status_after = if applied.finished.is_some() {
                game_status::FINISHED
            } else {
                game_status::OPEN
            };
            let advanced = self
                .store
                .advance_game(AdvanceGame {
                    game_id,
                    expected_updated_at: session.updated_at,
                    state: applied.state,
                    turn_of: applied.turn_of,
                    status: status_after,
                    at: caller.now,
                })
                .await?;

            match advanced {
                Some(updated) => {
                    self.meters.moved(kind);
                    let view = build_view(caller.account_id, &updated)?;
                    if let Some(outcome) = applied.finished {
                        self.meters.finished(Conclusion::of(outcome));
                        self.reward(&view.players, outcome, game_id, caller.now)
                            .await;
                    }
                    return Ok(MoveResult {
                        view,
                        events: applied.events,
                        outcome: applied.finished,
                    });
                }
                None => {
                    // Lost the race; re-read and try again.
                    self.meters.cas_retry();
                }
            }
        }
        // The budget is spent and every attempt lost its race: fail closed.
        self.meters.rejected(Rejection::Contended);
        Err(fault::conflict("game"))
    }

    async fn abandon(&self, caller: &Caller, game_id: Id) -> Result<GameView> {
        self.charge(caller, ABANDON_COST).await?;
        let session = self
            .store
            .game(game_id)
            .await?
            .ok_or_else(|| fault::not_found("game"))?;
        self.require_member(caller, session.conversation_id).await?;

        // Only a player may abandon; a watching member may not end someone else's game.
        let kind = kind_of(&session)?;
        let decoded = engine(kind).decode(&session.state).map_err(corrupt)?;
        if !decoded.players.contains(&caller.account_id) {
            return Err(fault::permission_denied("game"));
        }
        if !status_of(&session)?.is_open() {
            return Err(fault::conflict("game"));
        }

        let abandoned = self
            .store
            .abandon_game(game_id, caller.now)
            .await?
            .ok_or_else(|| fault::conflict("game"))?;
        self.meters.finished(Conclusion::NoContest);
        build_view(caller.account_id, &abandoned)
    }
}

/// Assembles a referee behind the erased [`Referee`] trait, with the operating-system
/// randomness a production deployment uses.
///
/// The `rewards` port is the deployment's bridge to the economy; a deployment without one
/// passes `Arc::new(`[`Unrewarded`](crate::traits::Unrewarded)`)`, and games are played and
/// decided exactly the same, only unrewarded.
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    rewards: SharedRewards,
    config: GamesConfig,
    registry: &Registry,
) -> SharedReferee {
    Arc::new(Games::new(
        store,
        limiter,
        rewards,
        config,
        Box::new(OsRandom) as Box<dyn Random>,
        registry,
    ))
}
