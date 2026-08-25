//! Tic-tac-toe: two players, alternating turns, three in a row to win.
//!
//! Nothing in this game is hidden — both players see the whole board — so [`Engine::render`]
//! ignores the viewer. What it demonstrates is the turn discipline: a move is refused unless
//! the board's parity says it is this player's turn, so a player cannot move twice, and a
//! replayed move lands on a cell that is now taken and is refused as illegal.

use migo_core::id::ID_BYTE_LEN;
use migo_core::{Id, Random};

use super::{
    get_id, get_u8, put_id, Applied, ApplyError, Corrupt, Created, Decoded, Engine, Reject,
};
use crate::model::{Event, GamesConfig, Mark, Move, Outcome, Render};

/// The engine.
pub(crate) struct TicTacToe;

/// Version byte of the state format.
const VERSION: u8 = 1;
/// Offset of player X's id (the first player, who moves first).
const OFF_X: usize = 1;
/// Offset of player O's id.
const OFF_O: usize = OFF_X + ID_BYTE_LEN;
/// Offset of the nine cells.
const OFF_CELLS: usize = OFF_O + ID_BYTE_LEN;
/// Total encoded length.
const STATE_LEN: usize = OFF_CELLS + 9;

/// The mark in a cell for the empty, X, and O states.
const EMPTY: u8 = 0;
const X: u8 = 1;
const O: u8 = 2;

/// The eight lines that win: three rows, three columns, two diagonals.
const LINES: [[usize; 3]; 8] = [
    [0, 1, 2],
    [3, 4, 5],
    [6, 7, 8],
    [0, 3, 6],
    [1, 4, 7],
    [2, 5, 8],
    [0, 4, 8],
    [2, 4, 6],
];

/// Encodes a board.
fn encode(x: Id, o: Id, cells: &[u8; 9]) -> Vec<u8> {
    let mut out = Vec::with_capacity(STATE_LEN);
    out.push(VERSION);
    put_id(&mut out, x);
    put_id(&mut out, o);
    out.extend_from_slice(cells);
    out
}

/// Decodes a board, or [`Corrupt`] if the bytes are not one this build wrote.
fn decode(state: &[u8]) -> Result<(Id, Id, [u8; 9]), Corrupt> {
    if state.len() != STATE_LEN || get_u8(state, 0)? != VERSION {
        return Err(Corrupt);
    }
    let x = get_id(state, OFF_X)?;
    let o = get_id(state, OFF_O)?;
    let mut cells = [EMPTY; 9];
    for (i, cell) in cells.iter_mut().enumerate() {
        let value = get_u8(state, OFF_CELLS + i)?;
        if value > O {
            return Err(Corrupt);
        }
        *cell = value;
    }
    Ok((x, o, cells))
}

/// The winning mark, if a line is complete.
fn winner(cells: &[u8; 9]) -> Option<u8> {
    LINES.into_iter().find_map(|line| {
        let mark = cells[line[0]];
        (mark != EMPTY && mark == cells[line[1]] && mark == cells[line[2]]).then_some(mark)
    })
}

/// How many cells are filled.
fn filled(cells: &[u8; 9]) -> usize {
    cells.iter().filter(|&&cell| cell != EMPTY).count()
}

/// The mark whose turn it is on a board with this many cells filled: X on even, O on odd.
fn to_move(cells: &[u8; 9]) -> u8 {
    if filled(cells).is_multiple_of(2) {
        X
    } else {
        O
    }
}

/// The outcome of a board, if it is terminal.
fn outcome_of(cells: &[u8; 9], x: Id, o: Id) -> Option<Outcome> {
    match winner(cells) {
        Some(X) => Some(Outcome::Win { winner: x }),
        Some(O) => Some(Outcome::Win { winner: o }),
        _ if filled(cells) == 9 => Some(Outcome::Draw),
        _ => None,
    }
}

/// The mark a player holds, or `None` if they are not in this game.
fn mark_of(player: Id, x: Id, o: Id) -> Option<u8> {
    if player == x {
        Some(X)
    } else if player == o {
        Some(O)
    } else {
        None
    }
}

impl Engine for TicTacToe {
    fn create(&self, players: &[Id], _config: &GamesConfig, _rng: &mut dyn Random) -> Created {
        debug_assert_eq!(players.len(), 2, "tic-tac-toe is a two-player game");
        let (x, o) = (players[0], players[1]);
        Created {
            state: encode(x, o, &[EMPTY; 9]),
            turn_of: Some(x),
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
        let Move::Place { cell } = mv else {
            return Err(Reject::WrongKind.into());
        };
        let (x, o, mut cells) = decode(prior)?;
        let Some(mark) = mark_of(player, x, o) else {
            return Err(Reject::NotAPlayer.into());
        };
        // A move should never reach a decided board — the move that decided it closed the game
        // in the same write — but refuse it rather than trust that.
        if outcome_of(&cells, x, o).is_some() {
            return Err(Reject::IllegalMove("the game is already decided").into());
        }
        if mark != to_move(&cells) {
            return Err(Reject::NotYourTurn.into());
        }
        let index = usize::from(cell);
        if index >= 9 {
            return Err(Reject::IllegalMove("cell is out of range").into());
        }
        if cells[index] != EMPTY {
            return Err(Reject::IllegalMove("cell is already taken").into());
        }
        cells[index] = mark;

        let outcome = outcome_of(&cells, x, o);
        let state = encode(x, o, &cells);
        let mut events = vec![Event::Moved {
            game_id,
            by: player,
        }];
        let turn_of = if let Some(result) = outcome {
            events.push(Event::Finished {
                game_id,
                outcome: result,
            });
            None
        } else {
            let next = if mark == X { o } else { x };
            events.push(Event::TurnChanged {
                game_id,
                turn_of: next,
            });
            Some(next)
        };
        Ok(Applied {
            state,
            turn_of,
            finished: outcome,
            events,
        })
    }

    fn decode(&self, state: &[u8]) -> Result<Decoded, Corrupt> {
        let (x, o, cells) = decode(state)?;
        let outcome = outcome_of(&cells, x, o);
        let turn_of = if outcome.is_some() {
            None
        } else if to_move(&cells) == X {
            Some(x)
        } else {
            Some(o)
        };
        Ok(Decoded {
            players: vec![x, o],
            turn_of,
            outcome,
        })
    }

    fn render(&self, state: &[u8], _viewer: Id) -> Result<Render, Corrupt> {
        let (_x, _o, cells) = decode(state)?;
        let mut out = [None; 9];
        for (slot, &cell) in out.iter_mut().zip(cells.iter()) {
            *slot = match cell {
                X => Some(Mark::X),
                O => Some(Mark::O),
                _ => None,
            };
        }
        Ok(Render::TicTacToe { cells: out })
    }
}
