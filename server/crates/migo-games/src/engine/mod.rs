//! The engines — one per game kind — and the machinery they share.
//!
//! An engine is the rules of one game, and nothing else: it has no store, no clock, no
//! randomness of its own, and no state between calls. It is handed a byte string that is the
//! whole authoritative state, a move, and (at creation) the server's randomness, and it
//! returns the next byte string. Everything that decides a game — whose turn it is, whether a
//! move is legal, who won — is a pure function of those bytes, computed here on the server.
//!
//! # The state is a private, versioned byte string
//!
//! This crate has no serialization dependency, exactly as the economy has none: it does not
//! speak on the network, the gateway does. So a game's state is a compact, fixed-layout byte
//! string this module builds and reads with the helpers below, prefixed with a one-byte
//! version so a later format can be told from an earlier one. The store persists it as opaque
//! bytes and never looks inside; a byte string this build cannot decode is a corruption or a
//! version skew, and the engine says so with [`Corrupt`] rather than guessing.
//!
//! # Redaction lives in `render`, decision in `apply`
//!
//! [`Engine::apply`] sees the whole truth and decides the move. [`Engine::render`] takes the
//! whole truth and returns only what one viewer may see — the guessing game's secret and the
//! un-revealed hand never reach a [`Render`], because `render` does not put them there. The
//! two are separate methods so that the code that decides a game and the code that shows it
//! cannot be confused for one another.

use migo_core::id::ID_BYTE_LEN;
use migo_core::{Id, Random};

use crate::model::{Event, GameKind, GamesConfig, Move, Outcome, Render};

mod guess_number;
mod rock_paper_scissors;
mod tic_tac_toe;

use guess_number::GuessNumber;
use rock_paper_scissors::RockPaperScissors;
use tic_tac_toe::TicTacToe;

/// A stored state this build could not decode: a corruption, or a version it does not know.
///
/// It is never a client's fault — the bytes were written by this crate — so the service turns
/// it into an internal error, not a rejection the caller sees the detail of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Corrupt;

/// Why a move was refused by an engine.
///
/// Distinct from a [`Corrupt`] state: this is the client proposing something the rules do not
/// allow, which is an ordinary, expected answer, not a fault of the server's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reject {
    /// The caller is not one of this game's players.
    NotAPlayer,
    /// It is not the caller's turn.
    NotYourTurn,
    /// The move is not legal against the current state, for the reason given.
    IllegalMove(&'static str),
    /// The move's variant does not match the game's kind.
    WrongKind,
}

/// The two ways [`Engine::apply`] can fail: an undecodable prior state, or a refused move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyError {
    /// The prior state could not be decoded.
    Corrupt,
    /// The move was refused.
    Reject(Reject),
}

impl From<Corrupt> for ApplyError {
    fn from(_: Corrupt) -> Self {
        Self::Corrupt
    }
}

impl From<Reject> for ApplyError {
    fn from(reject: Reject) -> Self {
        Self::Reject(reject)
    }
}

/// The engine-agnostic summary the service builds a view and a listing from.
pub(crate) struct Decoded {
    /// The players, in seat order.
    pub players: Vec<Id>,
    /// Whose move it is, if the game is open and turn-based.
    pub turn_of: Option<Id>,
    /// The result, if the state is terminal (a normal finish; abandonment is the store's, not
    /// the state's).
    pub outcome: Option<Outcome>,
}

/// The opening state of a new game.
pub(crate) struct Created {
    /// The encoded state.
    pub state: Vec<u8>,
    /// Whose move it is first, for the store's denormalized column.
    pub turn_of: Option<Id>,
}

/// The result of applying a move.
pub(crate) struct Applied {
    /// The next encoded state.
    pub state: Vec<u8>,
    /// Whose move it is next, or `None` if finished or awaiting simultaneous commits.
    pub turn_of: Option<Id>,
    /// The outcome if this move ended the game; `None` if it is still open. Its presence is
    /// what tells the service to persist the game as finished.
    pub finished: Option<Outcome>,
    /// The deltas to broadcast for this move (section 39), each already carrying the game's id.
    /// None of them carries a secret: a `Moved` says only that a player moved, so it is safe
    /// to send to every player including one who may not see what the move was.
    pub events: Vec<Event>,
}

/// One game's rules.
pub(crate) trait Engine: Send + Sync {
    /// Builds the opening state for `players` (already validated to the right count for the
    /// kind). Only a game with a server secret consults `rng`.
    fn create(&self, players: &[Id], config: &GamesConfig, rng: &mut dyn Random) -> Created;

    /// Applies `player`'s move to `prior`, deciding it. This is the whole of the
    /// server-authoritative validation: player, turn, legality, and kind are all checked here.
    /// The `game_id` is used only to stamp the [`Event`]s the move produces.
    fn apply(
        &self,
        game_id: Id,
        prior: &[u8],
        player: Id,
        mv: Move,
        config: &GamesConfig,
    ) -> Result<Applied, ApplyError>;

    /// Decodes the parts of `state` the service needs regardless of viewer: the players, whose
    /// turn it is, and the outcome if terminal.
    fn decode(&self, state: &[u8]) -> Result<Decoded, Corrupt>;

    /// Renders `state` as `viewer` is allowed to see it, omitting anything they may not know.
    fn render(&self, state: &[u8], viewer: Id) -> Result<Render, Corrupt>;
}

/// The engine for a kind.
///
/// The engines are zero-sized and stateless, so a shared static reference to each is all that
/// is ever needed; there is no per-game or per-service engine instance to build.
pub(crate) fn engine(kind: GameKind) -> &'static dyn Engine {
    match kind {
        GameKind::TicTacToe => &TicTacToe,
        GameKind::RockPaperScissors => &RockPaperScissors,
        GameKind::GuessNumber => &GuessNumber,
    }
}

// --- codec helpers -------------------------------------------------------------------------
//
// Private to this module, and therefore visible to the engine submodules beneath it, but to
// nothing else. Big-endian throughout, so a hex dump of a state reads left to right.

/// Appends an id's 16 bytes.
fn put_id(out: &mut Vec<u8>, id: Id) {
    out.extend_from_slice(id.as_bytes());
}

/// Reads the 16-byte id at `at`, or [`Corrupt`] if the slice is short.
fn get_id(bytes: &[u8], at: usize) -> Result<Id, Corrupt> {
    let slice = bytes.get(at..at + ID_BYTE_LEN).ok_or(Corrupt)?;
    let array: [u8; ID_BYTE_LEN] = slice.try_into().map_err(|_| Corrupt)?;
    Ok(Id::from_bytes(array))
}

/// Reads the byte at `at`, or [`Corrupt`] if it is past the end.
fn get_u8(bytes: &[u8], at: usize) -> Result<u8, Corrupt> {
    bytes.get(at).copied().ok_or(Corrupt)
}

/// Reads the big-endian `u16` at `at`, or [`Corrupt`] if the slice is short.
fn get_u16(bytes: &[u8], at: usize) -> Result<u16, Corrupt> {
    let slice = bytes.get(at..at + 2).ok_or(Corrupt)?;
    Ok(u16::from_be_bytes(slice.try_into().map_err(|_| Corrupt)?))
}
