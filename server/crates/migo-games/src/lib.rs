//! Mini-games played inside a conversation — brief sections 38 to 42, 86, 89 and 90 — decided
//! by a server that never trusts the client.
//!
//! # Server-authoritative, and what that costs the client
//!
//! The rule of section 89 is that the client may not be able to work out the result of a game
//! from what the server tells it. So the client never holds the game; the server does. A move
//! arrives as an [`Move`] — *place this mark*, *throw this hand*, *guess this number* — and
//! the server alone decides whether it is this caller's turn, whether the move is legal, what
//! it does to the board, whether the game is now won, and what each player is now allowed to
//! see. The reply is a [`GameView`] that has already had everything the caller may not know
//! removed: the opponent's un-revealed throw is not in it, the secret number is not in it —
//! not encrypted, not present. A client that wanted to cheat has nothing to read.
//!
//! Randomness is the server's too (section 90). The secret in a guessing game is drawn from
//! [`migo_core::Random`] at the moment the game starts and lives only in the authoritative
//! state; there is no seed a client could observe and replay.
//!
//! # Replay and the lost update, defeated at the store
//!
//! A server-authoritative game has two classic attacks: replay the same winning move twice,
//! and slip a move in against a board the server has already moved past. Both are defeated in
//! one place — the storage layer's compare-and-set. The authoritative state carries an
//! `updated_at` token; a move is written only if the token still matches the one the move was
//! computed against *and* the game is still open
//! ([`migo_store::traits::GameStore::advance_game`]). A replayed or superseded move names a
//! token that a prior write has already replaced, matches zero rows, and is rejected. The
//! engine re-reads and re-validates before retrying, so a genuine concurrent move (two players
//! committing at once in rock-paper-scissors) succeeds on the retry while a true replay is
//! caught by the engine finding the action already reflected in the fresh state. This service
//! adds no lock of its own, because a lock here is a lock a second writer races past.
//!
//! # No stake, no cash-out
//!
//! Sections 37 and 87 forbid gambling and any real-money cash-out. The default games carry no
//! stake: nothing is wagered, nothing is redistributed, there is no pot. Finishing a game
//! produces **experience** and, for a winner, a **standing** — never currency. Both are
//! handed to the [`Rewards`] port, which the composition root satisfies with the economy's
//! server-facing award methods; the anti-farming cap that keeps a farmed game from minting
//! unlimited experience lives there, in the economy, read from durable rows. This crate holds
//! no wallet and cannot move money. The store keeps stake columns, unused and reserved, for a
//! future extension that would have to clear its own regulatory review before a single one is
//! written.
//!
//! # What it depends on, and what it refuses to
//!
//! The store holds the authoritative state and the compare-and-set that guards it; the state
//! bytes are opaque to it. This crate reads a conversation's membership from that same store
//! ([`migo_store::traits::MessagingStore::is_member`]) to decide who may start, see, and play
//! a game — it does **not** depend on `migo-rooms` or `migo-messaging`, only on the narrow
//! traits the shared [`migo_store::Store`] aggregates. It rewards through the [`Rewards`] port
//! it owns rather than depending on `migo-economy`, so no arrow from this layer-3 crate points
//! sideways into another. See [`traits`] for the port, [`service`] for why the service is thin
//! over the store, and [`model`] for the game types.
//!
//! # Getting one
//!
//! ```ignore
//! let referee = migo_games::open(
//!     store,
//!     limiter,
//!     rewards,                    // an adapter over migo-economy, or `Arc::new(Unrewarded)`
//!     GamesConfig::default(),
//!     &registry,
//! );
//! let view = referee.start(&caller, conversation_id, GameKind::TicTacToe, &[opponent]).await?;
//! let result = referee.play(&caller, view.game_id, Move::Place { cell: 4 }).await?;
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod engine;
mod metrics;
pub mod model;
pub mod service;
pub mod traits;

pub use crate::model::{
    Caller, Event, GameInfo, GameKind, GameStatus, GameSummary, GameView, GamesConfig, Guess, Hand,
    Mark, Move, MoveResult, Outcome, Render,
};
pub use crate::service::{open, Games};
pub use crate::traits::{Referee, Rewards, SharedReferee, SharedRewards, Unrewarded};
