//! Rock-paper-scissors: two players, one simultaneous round.
//!
//! This is the game section 90 is about. Both players commit at once, and neither may learn
//! the other's hand before committing their own. There is no cryptographic commit-reveal here
//! and none is needed: the hands live in the authoritative state, which is the server's alone
//! and is never handed to a client. [`Engine::render`] returns the two hands only through an
//! `Option` it fills in solely once both are committed — until then a viewer sees *that* the
//! opponent has locked in, never *what* they locked in. A client has nothing to read and so
//! nothing to cheat with.
//!
//! Because the two commits are genuinely concurrent, this is also the game that exercises the
//! optimistic-lock retry: two commits computed against the same empty round race at the store,
//! one wins, and the other re-reads the now-half-committed round and adds itself to it. A
//! replayed commit re-reads a round it has already committed to and is refused.

use migo_core::id::ID_BYTE_LEN;
use migo_core::{Id, Random};

use super::{
    get_id, get_u8, put_id, Applied, ApplyError, Corrupt, Created, Decoded, Engine, Reject,
};
use crate::model::{Event, GamesConfig, Hand, Move, Outcome, Render};

/// The engine.
pub(crate) struct RockPaperScissors;

/// Version byte of the state format.
const VERSION: u8 = 1;
/// Offset of player 0's id.
const OFF_P0: usize = 1;
/// Offset of player 1's id.
const OFF_P1: usize = OFF_P0 + ID_BYTE_LEN;
/// Offset of player 0's commit byte.
const OFF_C0: usize = OFF_P1 + ID_BYTE_LEN;
/// Offset of player 1's commit byte.
const OFF_C1: usize = OFF_C0 + 1;
/// Total encoded length.
const STATE_LEN: usize = OFF_C1 + 1;

/// The commit byte for "not yet committed". A committed hand is stored as its byte plus one,
/// so that zero is unambiguously "none".
const NONE: u8 = 0;

/// Encodes a round.
fn encode(p0: Id, p1: Id, c0: u8, c1: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(STATE_LEN);
    out.push(VERSION);
    put_id(&mut out, p0);
    put_id(&mut out, p1);
    out.push(c0);
    out.push(c1);
    out
}

/// A stored commit byte as an optional hand, or [`Corrupt`] if it names no hand.
fn commit(byte: u8) -> Result<Option<Hand>, Corrupt> {
    match byte {
        NONE => Ok(None),
        other => Hand::from_u8(other - 1).map(Some).ok_or(Corrupt),
    }
}

/// Decodes a round: the two players and their optional hands.
fn decode(state: &[u8]) -> Result<(Id, Id, Option<Hand>, Option<Hand>), Corrupt> {
    if state.len() != STATE_LEN || get_u8(state, 0)? != VERSION {
        return Err(Corrupt);
    }
    let p0 = get_id(state, OFF_P0)?;
    let p1 = get_id(state, OFF_P1)?;
    let h0 = commit(get_u8(state, OFF_C0)?)?;
    let h1 = commit(get_u8(state, OFF_C1)?)?;
    Ok((p0, p1, h0, h1))
}

/// The outcome once both hands are in.
fn resolve(p0: Id, p1: Id, h0: Hand, h1: Hand) -> Outcome {
    if h0 == h1 {
        Outcome::Draw
    } else if h0.beats(h1) {
        Outcome::Win { winner: p0 }
    } else {
        Outcome::Win { winner: p1 }
    }
}

/// The outcome of a round, if both have committed.
fn outcome_of(p0: Id, p1: Id, h0: Option<Hand>, h1: Option<Hand>) -> Option<Outcome> {
    match (h0, h1) {
        (Some(a), Some(b)) => Some(resolve(p0, p1, a, b)),
        _ => None,
    }
}

impl Engine for RockPaperScissors {
    fn create(&self, players: &[Id], _config: &GamesConfig, _rng: &mut dyn Random) -> Created {
        debug_assert_eq!(players.len(), 2, "rock-paper-scissors is a two-player game");
        let (p0, p1) = (players[0], players[1]);
        Created {
            state: encode(p0, p1, NONE, NONE),
            // Simultaneous: there is no single player whose turn it is.
            turn_of: None,
        }
    }

    fn apply(
        &self,
        game_id: Id,
        prior: &[u8],
        player: Id,
        mv: Move,
        _config: &GamesConfig,
    ) -> Result<Applied, ApplyError> {
        let Move::Throw { hand } = mv else {
            return Err(Reject::WrongKind.into());
        };
        let (p0, p1, mut h0, mut h1) = decode(prior)?;
        let seat = if player == p0 {
            &mut h0
        } else if player == p1 {
            &mut h1
        } else {
            return Err(Reject::NotAPlayer.into());
        };
        // Committing twice is refused. This is also what defeats a replay: the second arrival
        // of the same commit finds the seat already taken.
        if seat.is_some() {
            return Err(Reject::IllegalMove("you have already committed a hand").into());
        }
        *seat = Some(hand);

        let outcome = outcome_of(p0, p1, h0, h1);
        let commit_byte = |hand: Option<Hand>| hand.map_or(NONE, |h| h.to_u8() + 1);
        let state = encode(p0, p1, commit_byte(h0), commit_byte(h1));
        let mut events = vec![Event::Moved {
            game_id,
            by: player,
        }];
        if let Some(result) = outcome {
            events.push(Event::Finished {
                game_id,
                outcome: result,
            });
        }
        Ok(Applied {
            state,
            // Simultaneous throughout: never a single player's turn.
            turn_of: None,
            finished: outcome,
            events,
        })
    }

    fn decode(&self, state: &[u8]) -> Result<Decoded, Corrupt> {
        let (p0, p1, h0, h1) = decode(state)?;
        Ok(Decoded {
            players: vec![p0, p1],
            turn_of: None,
            outcome: outcome_of(p0, p1, h0, h1),
        })
    }

    fn render(&self, state: &[u8], _viewer: Id) -> Result<Render, Corrupt> {
        let (_p0, _p1, h0, h1) = decode(state)?;
        // The reveal is gated on *both* being committed, so it is the same for every viewer:
        // there is no moment at which one player's hand is visible and the other's is not.
        let reveal = match (h0, h1) {
            (Some(a), Some(b)) => Some([a, b]),
            _ => None,
        };
        Ok(Render::RockPaperScissors {
            committed: [h0.is_some(), h1.is_some()],
            reveal,
        })
    }
}
