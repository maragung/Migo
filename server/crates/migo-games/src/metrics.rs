//! Counters for games started, moves played, moves refused, and games concluded.
//!
//! # What may label a series here, and what may never
//!
//! Brief section 174 forbids a metric series labelled by account; this crate adds that no
//! series is labelled by conversation either. A counter keyed on conversation would let a
//! dashboard rebuild which chats are most active — a shape of the social graph — from the
//! metrics endpoint, and that is exactly what section 174 keeps out of it. So a game
//! increments a counter for its *kind* and nothing about who played it or where.
//!
//! Every label domain in this module is a closed enum — the game kind, one rejection enum,
//! one conclusion enum — so the cardinality of the whole crate is fixed at compile time, and
//! adding a variant to any of them is a diff a reviewer sees. The two lock-contention series
//! carry no label at all.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

use crate::model::{GameKind, Outcome};

/// Why a move was refused.
///
/// Worth splitting rather than folding into one "rejected" counter, because the variants mean
/// very different things about the client. A spike in `IllegalMove` is a client sending moves
/// the rules forbid — a bug, or a probe. A spike in `Contended` is not the client's fault at
/// all; it is genuine simultaneous play exhausting the optimistic-lock retries, and it calls
/// for a different response than a spike in `NotYourTurn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// The caller is not one of the game's players.
    NotAPlayer,
    /// It is not the caller's turn.
    NotYourTurn,
    /// The move is not legal for this game's state — an occupied cell, a second commit, a
    /// guess out of range.
    IllegalMove,
    /// The move's variant does not match the game's kind — a thrown hand at a board.
    WrongKind,
    /// The move kept losing the optimistic-lock race and ran out of retries.
    Contended,
}

impl Rejection {
    pub(crate) const ALL: [Self; 5] = [
        Self::NotAPlayer,
        Self::NotYourTurn,
        Self::IllegalMove,
        Self::WrongKind,
        Self::Contended,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NotAPlayer => "not_a_player",
            Self::NotYourTurn => "not_your_turn",
            Self::IllegalMove => "illegal_move",
            Self::WrongKind => "wrong_kind",
            Self::Contended => "contended",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// How a game concluded, as a closed label.
///
/// [`Outcome`] itself cannot be a label — its `Win` carries the winner's id, which is an
/// account, which section 174 forbids. This is the outcome with the id stripped off: the
/// three shapes an ending takes, and nothing about who it happened to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Conclusion {
    /// Somebody won.
    Win,
    /// A tie.
    Draw,
    /// No result — attempts ran out, or the game was abandoned.
    NoContest,
}

impl Conclusion {
    pub(crate) const ALL: [Self; 3] = [Self::Win, Self::Draw, Self::NoContest];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Win => "win",
            Self::Draw => "draw",
            Self::NoContest => "no_contest",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// The conclusion of an outcome, with the winner's id discarded.
    pub(crate) const fn of(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Win { .. } => Self::Win,
            Outcome::Draw => Self::Draw,
            Outcome::NoContest => Self::NoContest,
        }
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    started: Vec<Arc<Counter>>,
    moves: Vec<Arc<Counter>>,
    rejected: Vec<Arc<Counter>>,
    finished: Vec<Arc<Counter>>,
    cas_retries: Arc<Counter>,
    rewards_dropped: Arc<Counter>,
}

/// Registers one counter per variant, each tagged `key` with the variant's own label.
///
/// The per-variant series share a shape — a name, a help string, and one label whose value is
/// the variant's — so they share a builder. Registering the whole variant set up front is what
/// gives a dashboard a flat line instead of a gap for an outcome nobody has hit yet.
fn per_variant<T>(
    registry: &Registry,
    name: &'static str,
    help: &'static str,
    key: &'static str,
    variants: &[T],
    label: impl Fn(&T) -> &'static str,
) -> Vec<Arc<Counter>> {
    variants
        .iter()
        .map(|variant| registry.counter(name, help, &[(key, label(variant))]))
        .collect()
}

impl Meters {
    /// Registers every series at zero, all of them up front, so a dashboard shows a flat line
    /// rather than a gap for an outcome nobody has hit yet.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            started: per_variant(
                registry,
                "migo_games_started_total",
                "Games started, by kind.",
                "kind",
                GameKind::ALL,
                |kind| kind.slug(),
            ),
            moves: per_variant(
                registry,
                "migo_games_moves_total",
                "Moves accepted and applied, by kind.",
                "kind",
                GameKind::ALL,
                |kind| kind.slug(),
            ),
            rejected: per_variant(
                registry,
                "migo_games_moves_rejected_total",
                "Moves refused, by reason.",
                "reason",
                &Rejection::ALL,
                |reason| reason.label(),
            ),
            finished: per_variant(
                registry,
                "migo_games_finished_total",
                "Games concluded, by conclusion.",
                "conclusion",
                &Conclusion::ALL,
                |conclusion| conclusion.label(),
            ),
            cas_retries: registry.counter(
                "migo_games_cas_retries_total",
                "Times a move lost the optimistic-lock race and re-read to retry.",
                &[],
            ),
            rewards_dropped: registry.counter(
                "migo_games_rewards_dropped_total",
                "Rewards for a finished game that the rewards port failed to grant.",
                &[],
            ),
        }
    }

    pub(crate) fn started(&self, kind: GameKind) {
        if let Some(counter) = self.started.get(kind as usize) {
            counter.inc();
        }
    }

    pub(crate) fn moved(&self, kind: GameKind) {
        if let Some(counter) = self.moves.get(kind as usize) {
            counter.inc();
        }
    }

    pub(crate) fn rejected(&self, reason: Rejection) {
        if let Some(counter) = self.rejected.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn finished(&self, conclusion: Conclusion) {
        if let Some(counter) = self.finished.get(conclusion.index()) {
            counter.inc();
        }
    }

    pub(crate) fn cas_retry(&self) {
        self.cas_retries.inc();
    }

    pub(crate) fn reward_dropped(&self) {
        self.rewards_dropped.inc();
    }
}
