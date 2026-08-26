//! The types the referee speaks in: who is calling, which games exist, the moves a client may
//! send, and the redacted views it gets back.
//!
//! # Enums here, raw integers in the store
//!
//! The store keeps a game's `kind` and `status` as raw `i16`, opaque to it, exactly as the
//! economy keeps a ledger entry's `source` as a raw integer. The meaning of those integers —
//! which kind is tic-tac-toe, which status is finished — is a domain fact, and domain facts
//! live in this crate, not in the storage layer. [`GameKind`] and [`GameStatus`] are that
//! mapping, in one place, with the conversion to and from the wire integer beside them.
//!
//! # Redaction is structural, not careful
//!
//! A [`GameView`] cannot leak a secret it does not hold. [`Render::GuessNumber`] has no field
//! for the target number; [`Render::RockPaperScissors`] reveals the two hands only through an
//! `Option` that the engine fills in solely once both are committed. There is no code path
//! that "remembers" to hide something, because the thing to hide is not in the type.

use migo_core::{Id, Timestamp};
use migo_ratelimit::TrustTier;
use migo_store::model::game_status;

/// The maximum number of games one `active` query returns for a conversation.
///
/// A conversation with hundreds of open games at once is not a real conversation; it is an
/// abuse, and the cap is what stops one listing from paging the lot. It is small on purpose —
/// a handful of live games is the realistic ceiling for one chat.
pub const MAX_ACTIVE_GAMES: u16 = 32;

/// Everything the referee needs to know about the caller of a request.
///
/// Identical in shape to every other layer-3 crate's caller: the account and device it came
/// from, the standing the rate limiter prices by, the server's clock for this request, and an
/// optional correlation id for the logs.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

/// Which game is being played.
///
/// The discriminant is the integer the store persists; it is part of the on-disk format and
/// must not be renumbered. The three shipped kinds span the three shapes a game can take —
/// one turn-based (tic-tac-toe), one simultaneous (rock-paper-scissors), one single-player
/// against a server secret (guess-the-number) — so the engine machinery is exercised by all
/// three archetypes of section 38.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i16)]
pub enum GameKind {
    /// Two players, alternating turns, three in a row wins.
    TicTacToe = 0,
    /// Two players, one simultaneous commit-and-reveal round.
    RockPaperScissors = 1,
    /// One player against a number the server drew and hid.
    GuessNumber = 2,
}

impl GameKind {
    /// Every kind, for catalogue listings, metric registration, and tests.
    pub const ALL: &'static [Self] = &[Self::TicTacToe, Self::RockPaperScissors, Self::GuessNumber];

    /// The integer the store persists for this kind.
    #[must_use]
    pub fn to_i16(self) -> i16 {
        self as i16
    }

    /// The kind for a persisted integer, or `None` if this build does not know it.
    ///
    /// `None` is a corrupt or future-version row, which the service turns into an internal
    /// error rather than guessing — a game whose kind it cannot name is one it cannot referee.
    #[must_use]
    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::TicTacToe),
            1 => Some(Self::RockPaperScissors),
            2 => Some(Self::GuessNumber),
            _ => None,
        }
    }

    /// A stable snake-case identifier, used as the closed label on every game metric and as
    /// the machine name a client selects a game by.
    ///
    /// It is derived from the kind, not the account or conversation, so it is safe as a metric
    /// label: its cardinality is exactly [`GameKind::ALL`].
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::TicTacToe => "tic_tac_toe",
            Self::RockPaperScissors => "rock_paper_scissors",
            Self::GuessNumber => "guess_number",
        }
    }

    /// The fewest players a game of this kind needs to start (the caller included).
    #[must_use]
    pub fn min_players(self) -> u8 {
        match self {
            Self::TicTacToe | Self::RockPaperScissors => 2,
            Self::GuessNumber => 1,
        }
    }

    /// The most players a game of this kind admits (the caller included).
    #[must_use]
    pub fn max_players(self) -> u8 {
        self.min_players()
    }
}

/// Where a game is in its life.
///
/// Mirrors the store's [`game_status`] integers one-to-one. `Open` is the only status a move
/// may be played against; the other two are terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i16)]
pub enum GameStatus {
    /// Accepting moves.
    Open = game_status::OPEN,
    /// Played to a result.
    Finished = game_status::FINISHED,
    /// Ended without a result — a player left, or it was forfeited.
    Abandoned = game_status::ABANDONED,
}

impl GameStatus {
    /// The integer the store persists for this status.
    #[must_use]
    pub fn to_i16(self) -> i16 {
        self as i16
    }

    /// The status for a persisted integer, or `None` if this build does not know it.
    #[must_use]
    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            game_status::OPEN => Some(Self::Open),
            game_status::FINISHED => Some(Self::Finished),
            game_status::ABANDONED => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// Whether a move may still be played against a game in this status.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// A hand in rock-paper-scissors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Hand {
    /// Beats scissors, loses to paper.
    Rock = 0,
    /// Beats rock, loses to scissors.
    Paper = 1,
    /// Beats paper, loses to rock.
    Scissors = 2,
}

impl Hand {
    /// The byte this hand is stored as.
    #[must_use]
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    /// The hand for a stored byte, or `None` if it is not one of the three.
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Rock),
            1 => Some(Self::Paper),
            2 => Some(Self::Scissors),
            _ => None,
        }
    }

    /// Whether this hand beats `other`. A hand does not beat itself.
    #[must_use]
    pub fn beats(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Rock, Self::Scissors)
                | (Self::Paper, Self::Rock)
                | (Self::Scissors, Self::Paper)
        )
    }
}

/// A mark on a tic-tac-toe board.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Mark {
    /// The first player, who moves first.
    X = 1,
    /// The second player.
    O = 2,
}

/// How a guess compared to the hidden number, told to the guesser after each guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feedback {
    /// The secret is smaller than this guess.
    Lower,
    /// The secret is larger than this guess.
    Higher,
    /// This guess was the secret.
    Correct,
}

/// One guess in a guessing game's history, with the comparison the server returned.
///
/// The history is public to the one player it belongs to; the secret it was compared against
/// is not part of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guess {
    /// The number the player guessed.
    pub value: u16,
    /// How it compared to the secret.
    pub feedback: Feedback,
}

/// A move a client submits. The server decides whether it is legal; the client only proposes.
///
/// Which variant is valid depends on the game's kind, and sending the wrong one is a
/// validation error the engine returns — a `Throw` at a tic-tac-toe board is not a move, it is
/// a mistake, and it is named as one rather than silently ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    /// Place your mark on a tic-tac-toe cell, numbered 0 through 8, row-major.
    Place {
        /// The target cell, 0..=8.
        cell: u8,
    },
    /// Commit a hand in rock-paper-scissors. The opponent is not told which until both have
    /// committed.
    Throw {
        /// The hand to commit.
        hand: Hand,
    },
    /// Guess the hidden number.
    Guess {
        /// The number guessed.
        value: u16,
    },
}

/// How a game ended.
///
/// There is no monetary field, by construction: a game cannot pay out (sections 37 and 87).
/// The outcome names who won, if anyone; what that win is worth in experience or standing is
/// the [`crate::traits::Rewards`] port's decision, not the outcome's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// One player won. In a single-player game this is the player having solved it.
    Win {
        /// The winner.
        winner: Id,
    },
    /// The game was played to a tie — a full tic-tac-toe board, or matching hands.
    Draw,
    /// The game ended with no result: a single-player game whose attempts ran out, or a game a
    /// player abandoned.
    NoContest,
}

/// The part of a game a viewer is allowed to see, rendered per kind.
///
/// Each variant carries only public information. What a viewer must not know is absent from
/// the type, not merely blanked: the guessing game's secret has no field, and the two hands of
/// rock-paper-scissors are reachable only once both are down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Render {
    /// A tic-tac-toe board: nine cells, each empty or holding a mark.
    TicTacToe {
        /// Cells 0..=8, row-major; `None` is empty.
        cells: [Option<Mark>; 9],
    },
    /// A rock-paper-scissors round.
    RockPaperScissors {
        /// Whether player 0 and player 1 have each committed a hand. Knowing the opponent has
        /// locked in is fair; knowing what they locked in is not.
        committed: [bool; 2],
        /// The two hands — player 0 then player 1 — revealed only once both are committed, and
        /// `None` until then.
        reveal: Option<[Hand; 2]>,
    },
    /// A guessing game, from the guesser's side.
    GuessNumber {
        /// The lowest value still possible given the feedback so far, inclusive.
        low: u16,
        /// The highest value still possible given the feedback so far, inclusive.
        high: u16,
        /// Guesses remaining before the game is lost.
        remaining: u8,
        /// Every guess made so far, with the feedback each got.
        guesses: Vec<Guess>,
    },
}

/// A game as one caller may see it right now — the reply to a start, a read, or a move.
#[derive(Clone, Debug)]
pub struct GameView {
    /// The game's id.
    pub game_id: Id,
    /// Which game this is.
    pub kind: GameKind,
    /// The conversation it is played in.
    pub conversation_id: Id,
    /// Where it is in its life.
    pub status: GameStatus,
    /// The players, in seat order (player 0 first). For tic-tac-toe, player 0 is X.
    pub players: Vec<Id>,
    /// Whose move it is, or `None` if the game is over or awaiting simultaneous commits.
    pub turn_of: Option<Id>,
    /// Whether it is the caller's move — a convenience the client would otherwise derive from
    /// `turn_of` and its own id.
    pub your_turn: bool,
    /// The result, present once the game is finished.
    pub outcome: Option<Outcome>,
    /// The board as this caller may see it.
    pub render: Render,
    /// Which state this view is of, monotonically non-decreasing per game.
    ///
    /// The store's `updated_at`, which is already the optimistic-lock token every move is
    /// applied against, rendered as an opaque number. It is here because a client that
    /// receives two broadcasts out of order has no other way to tell which one describes the
    /// later board, and inventing a second counter beside the lock token would create two
    /// notions of "which state" that could disagree.
    ///
    /// Opaque on purpose: it is not a move count and not a wall-clock time a client should
    /// display. The only operation defined on it is comparison against another version of the
    /// same game.
    pub state_version: u64,
}

/// A one-line entry in a conversation's list of open games.
#[derive(Clone, Debug)]
pub struct GameSummary {
    /// The game's id.
    pub game_id: Id,
    /// Which game this is.
    pub kind: GameKind,
    /// Where it is in its life (always [`GameStatus::Open`] in an `active` listing).
    pub status: GameStatus,
    /// The players, in seat order.
    pub players: Vec<Id>,
    /// Whose move it is, if anyone's.
    pub turn_of: Option<Id>,
}

/// A game this deployment offers, for a client to render a menu from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameInfo {
    /// Which game.
    pub kind: GameKind,
    /// Its stable machine name.
    pub slug: &'static str,
    /// The fewest players it needs.
    pub min_players: u8,
    /// The most players it admits.
    pub max_players: u8,
}

impl GameInfo {
    /// The catalogue entry for a kind.
    #[must_use]
    pub fn of(kind: GameKind) -> Self {
        Self {
            kind,
            slug: kind.slug(),
            min_players: kind.min_players(),
            max_players: kind.max_players(),
        }
    }
}

/// A compact delta the gateway broadcasts to a game's players after a move (section 39).
///
/// It is a *delta*, not a whole board: a player who has the previous view applies the event to
/// get the next one, and the server does not re-broadcast the entire state on every move. Each
/// event is safe to send to every player, because none of them carries a secret — a `Moved`
/// says only *that* a player moved, never a hand or a number that a recipient may not see. A
/// client that needs the full, per-viewer board asks for a [`GameView`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A game began.
    Started {
        /// The game.
        game_id: Id,
        /// Its kind.
        kind: GameKind,
    },
    /// A player made a move. What the move *was* is not here; the recipient re-reads its own
    /// view to see the parts it is allowed to.
    Moved {
        /// The game.
        game_id: Id,
        /// Who moved.
        by: Id,
    },
    /// It is now a different player's turn.
    TurnChanged {
        /// The game.
        game_id: Id,
        /// Whose turn it now is.
        turn_of: Id,
    },
    /// The game ended.
    Finished {
        /// The game.
        game_id: Id,
        /// How it ended.
        outcome: Outcome,
    },
}

/// The result of a move: the mover's fresh view, the deltas to broadcast, and the outcome if
/// the move ended the game.
#[derive(Clone, Debug)]
pub struct MoveResult {
    /// The mover's view after the move.
    pub view: GameView,
    /// The deltas the gateway broadcasts to the game's players.
    pub events: Vec<Event>,
    /// The outcome, present only if this move ended the game.
    pub outcome: Option<Outcome>,
}

/// The tunables a deployment may set for games.
///
/// Rate-limit costs are not here — those are fixed constants priced against the shared budget,
/// like every other crate's. What is here is the reward economy of a game (how much experience
/// a win and a mere finish are worth) and the shape of the guessing game (its range and how
/// many tries it allows), because those are balance decisions an operator might tune.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GamesConfig {
    /// Experience credited to the winner of a game.
    pub win_experience: i64,
    /// Experience credited to a player who finished a game without winning — the loser, or
    /// each player in a draw. Small on purpose: playing is worth something, losing is not
    /// worth as much as winning, and neither is worth farming (the economy's daily cap is the
    /// real ceiling).
    pub finish_experience: i64,
    /// The guessing game's range is 1 through this value, inclusive.
    pub guess_bound: u16,
    /// How many guesses a guessing game allows before it is lost.
    pub guess_attempts: u8,
    /// How many times a move re-reads and retries after losing an optimistic-lock race before
    /// giving up with a conflict. Small: a handful of genuine concurrent commits is the most a
    /// two-player game produces at once.
    pub retry_budget: u8,
}

impl Default for GamesConfig {
    fn default() -> Self {
        Self {
            win_experience: 50,
            finish_experience: 10,
            guess_bound: 100,
            guess_attempts: 7,
            retry_budget: 4,
        }
    }
}
