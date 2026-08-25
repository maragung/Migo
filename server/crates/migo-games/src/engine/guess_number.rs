//! Guess-the-number: one player against a number the server drew and hid.
//!
//! This is where the server's randomness lives (section 90). The secret is drawn from
//! [`Random`] the moment the game is created and stored in the authoritative state; there is
//! no seed a client observed and no way to replay the draw. [`Engine::render`] reads the
//! secret to work out each guess's feedback and the range still in play, but never puts the
//! secret into a [`Render`] — the type has no field for it, so it cannot leak. What the player
//! learns is exactly what the feedback tells them, and nothing more.

use migo_core::id::ID_BYTE_LEN;
use migo_core::{Id, Random};

use super::{
    get_id, get_u16, get_u8, put_id, Applied, ApplyError, Corrupt, Created, Decoded, Engine, Reject,
};
use crate::model::{Event, Feedback, GamesConfig, Guess, Move, Outcome, Render};

/// The engine.
pub(crate) struct GuessNumber;

/// Version byte of the state format.
const VERSION: u8 = 1;
/// Offset of the player's id.
const OFF_PLAYER: usize = 1;
/// Offset of the hidden secret.
const OFF_SECRET: usize = OFF_PLAYER + ID_BYTE_LEN;
/// Offset of the inclusive upper bound of the range (the lower bound is always one).
const OFF_BOUND: usize = OFF_SECRET + 2;
/// Offset of the number of guesses allowed.
const OFF_ATTEMPTS: usize = OFF_BOUND + 2;
/// Offset of the solved flag.
const OFF_SOLVED: usize = OFF_ATTEMPTS + 1;
/// Offset of the guess count.
const OFF_COUNT: usize = OFF_SOLVED + 1;
/// Offset of the first guess; each guess is a big-endian `u16`.
const OFF_GUESSES: usize = OFF_COUNT + 1;

/// A decoded round.
struct Round {
    player: Id,
    secret: u16,
    bound: u16,
    attempts: u8,
    solved: bool,
    guesses: Vec<u16>,
}

/// Encodes a round.
fn encode(round: &Round) -> Vec<u8> {
    let mut out = Vec::with_capacity(OFF_GUESSES + round.guesses.len() * 2);
    out.push(VERSION);
    put_id(&mut out, round.player);
    out.extend_from_slice(&round.secret.to_be_bytes());
    out.extend_from_slice(&round.bound.to_be_bytes());
    out.push(round.attempts);
    out.push(u8::from(round.solved));
    out.push(u8::try_from(round.guesses.len()).unwrap_or(u8::MAX));
    for &value in &round.guesses {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out
}

/// Decodes a round, or [`Corrupt`] if the bytes are not one this build wrote.
fn decode(state: &[u8]) -> Result<Round, Corrupt> {
    if state.len() < OFF_GUESSES || get_u8(state, 0)? != VERSION {
        return Err(Corrupt);
    }
    let player = get_id(state, OFF_PLAYER)?;
    let secret = get_u16(state, OFF_SECRET)?;
    let bound = get_u16(state, OFF_BOUND)?;
    let attempts = get_u8(state, OFF_ATTEMPTS)?;
    let solved = get_u8(state, OFF_SOLVED)? != 0;
    let count = usize::from(get_u8(state, OFF_COUNT)?);
    if state.len() != OFF_GUESSES + count * 2 {
        return Err(Corrupt);
    }
    let mut guesses = Vec::with_capacity(count);
    for i in 0..count {
        guesses.push(get_u16(state, OFF_GUESSES + i * 2)?);
    }
    Ok(Round {
        player,
        secret,
        bound,
        attempts,
        solved,
        guesses,
    })
}

/// How a guess compares to the secret. `Lower` means the secret is below the guess.
fn feedback(value: u16, secret: u16) -> Feedback {
    match value.cmp(&secret) {
        core::cmp::Ordering::Equal => Feedback::Correct,
        core::cmp::Ordering::Greater => Feedback::Lower,
        core::cmp::Ordering::Less => Feedback::Higher,
    }
}

/// The outcome of a round, if terminal: solved is a win, out of guesses is a no-contest.
fn outcome_of(round: &Round) -> Option<Outcome> {
    if round.solved {
        Some(Outcome::Win {
            winner: round.player,
        })
    } else if round.guesses.len() >= usize::from(round.attempts) {
        Some(Outcome::NoContest)
    } else {
        None
    }
}

impl Engine for GuessNumber {
    fn create(&self, players: &[Id], config: &GamesConfig, rng: &mut dyn Random) -> Created {
        debug_assert_eq!(players.len(), 1, "guess-the-number is a single-player game");
        let player = players[0];
        let bound = config.guess_bound.max(1);
        let attempts = config.guess_attempts.max(1);
        // `below(bound)` is in `0..bound`; the secret is that plus one, so `1..=bound`.
        let secret = u16::try_from(rng.below(u64::from(bound))).unwrap_or(0) + 1;
        let round = Round {
            player,
            secret,
            bound,
            attempts,
            solved: false,
            guesses: Vec::new(),
        };
        Created {
            state: encode(&round),
            turn_of: Some(player),
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
        let Move::Guess { value } = mv else {
            return Err(Reject::WrongKind.into());
        };
        let mut round = decode(prior)?;
        if player != round.player {
            return Err(Reject::NotAPlayer.into());
        }
        if outcome_of(&round).is_some() {
            return Err(Reject::IllegalMove("the game is already over").into());
        }
        if !(1..=round.bound).contains(&value) {
            return Err(Reject::IllegalMove("guess is out of range").into());
        }
        if matches!(feedback(value, round.secret), Feedback::Correct) {
            round.solved = true;
        }
        round.guesses.push(value);

        let outcome = outcome_of(&round);
        let state = encode(&round);
        let mut events = vec![Event::Moved {
            game_id,
            by: player,
        }];
        let turn_of = match outcome {
            Some(result) => {
                events.push(Event::Finished {
                    game_id,
                    outcome: result,
                });
                None
            }
            None => Some(player),
        };
        Ok(Applied {
            state,
            turn_of,
            finished: outcome,
            events,
        })
    }

    fn decode(&self, state: &[u8]) -> Result<Decoded, Corrupt> {
        let round = decode(state)?;
        let outcome = outcome_of(&round);
        let turn_of = if outcome.is_some() {
            None
        } else {
            Some(round.player)
        };
        Ok(Decoded {
            players: vec![round.player],
            turn_of,
            outcome,
        })
    }

    fn render(&self, state: &[u8], _viewer: Id) -> Result<Render, Corrupt> {
        let round = decode(state)?;
        let mut low = 1u16;
        let mut high = round.bound;
        let mut history = Vec::with_capacity(round.guesses.len());
        for &value in &round.guesses {
            let feedback = feedback(value, round.secret);
            match feedback {
                // Secret is below the guess: the guess is a new ceiling.
                Feedback::Lower => high = high.min(value.saturating_sub(1)),
                // Secret is above the guess: the guess is a new floor.
                Feedback::Higher => low = low.max(value.saturating_add(1)),
                Feedback::Correct => {
                    low = value;
                    high = value;
                }
            }
            history.push(Guess { value, feedback });
        }
        let remaining = round
            .attempts
            .saturating_sub(u8::try_from(round.guesses.len()).unwrap_or(u8::MAX));
        Ok(Render::GuessNumber {
            low,
            high,
            remaining,
            guesses: history,
        })
    }
}
