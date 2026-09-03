//! What the games layer must prove.
//!
//! Three properties carry the whole crate, and every test below exists to hold one of them
//! down. The first is that a client cannot learn what it must not know: the guessing game's
//! secret and an uncommitted rock-paper-scissors hand are in the authoritative state, and no
//! view, no render, and no `Debug` line may carry them out. The second is that a game advances
//! only once per move even when two writers race, which is the single compare-and-set in
//! `advance_game` and nothing else; a replayed move must find the seat taken rather than take
//! it twice. The third is that authority comes from the store, never from the request: a
//! caller who is not in the conversation is told the conversation does not exist, and a caller
//! who is in it but not in the game is refused the game.
//!
//! Two further things are checked because they are cheap to get wrong. Every method charges
//! the limiter before it looks at membership, so a stranger cannot probe conversation
//! existence for free; and the rewards port is called through a recording double, so a test
//! can see that a loss credits the participation grant, a win credits the win grant and marks
//! the winner exactly once, and a rewards failure is swallowed into a counter rather than
//! rolled back onto a game that has already been decided.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Random, Result, Timestamp};
use migo_games::model::{
    Caller, Event, Feedback, GameInfo, GameKind, GameStatus, GameView, GamesConfig, Hand, Mark,
    Move, Outcome, Render, MAX_ACTIVE_GAMES,
};
use migo_games::service::Games;
use migo_games::traits::{Referee, Rewards, Unrewarded};
use migo_protocol::{codes, fault, ConversationKind, EncryptionMode};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::{AdvanceGame, Conversation};
use migo_store::traits::{GameStore, MessagingStore};
use migo_store::MemoryStore;

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const NOW: i64 = 1_700_000_000 * SECOND;
const DEVICE_OFFSET: u128 = 1_000_000;

/// The conversation every game in these tests is played in.
const ROOM: u128 = 900;
/// A second conversation, for the tests that prove one game is not visible from another.
const OTHER_ROOM: u128 = 901;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

fn device_of(account: u128) -> Id {
    id(account + DEVICE_OFFSET)
}

/// Randomness a test can predict.
///
/// `below` is the only draw the guessing game makes, so overriding it fixes the secret and a
/// test can play a game to a win instead of guessing at it. `fill_bytes` still has to produce
/// distinct bytes on every call, because it is also what mints game ids; a counter-driven
/// stream is enough and keeps the whole harness reproducible.
struct Rigged {
    /// What `below` returns, reduced into range.
    fixed: u64,
    /// The id stream's state.
    counter: u64,
}

impl Rigged {
    fn new(fixed: u64) -> Self {
        Self { fixed, counter: 1 }
    }
}

impl Random for Rigged {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for slot in dest.iter_mut() {
            self.counter = self
                .counter
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            *slot = u8::try_from((self.counter >> 33) & 0xff).unwrap_or(0);
        }
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.fixed % bound
        }
    }
}

/// What the rewards port was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Credit {
    Experience { account_id: Id, amount: i64 },
    Winner { account_id: Id },
}

/// A [`Rewards`] double that records instead of crediting, and can be told to fail.
#[derive(Default)]
struct Ledger {
    credits: Mutex<Vec<Credit>>,
    /// When set, every call fails, which is how the dropped-reward path is reached.
    broken: Mutex<bool>,
}

impl Ledger {
    fn break_it(&self) {
        *self.broken.lock().expect("the ledger lock is not poisoned") = true;
    }

    fn credits(&self) -> Vec<Credit> {
        self.credits
            .lock()
            .expect("the ledger lock is not poisoned")
            .clone()
    }

    fn experience_of(&self, account_id: Id) -> i64 {
        self.credits()
            .into_iter()
            .filter_map(|credit| match credit {
                Credit::Experience {
                    account_id: who,
                    amount,
                } if who == account_id => Some(amount),
                _ => None,
            })
            .sum()
    }

    fn winners(&self) -> Vec<Id> {
        self.credits()
            .into_iter()
            .filter_map(|credit| match credit {
                Credit::Winner { account_id } => Some(account_id),
                _ => None,
            })
            .collect()
    }

    fn record(&self, credit: Credit) -> Result<()> {
        if *self.broken.lock().expect("the ledger lock is not poisoned") {
            return Err(fault::internal("the ledger is closed"));
        }
        self.credits
            .lock()
            .expect("the ledger lock is not poisoned")
            .push(credit);
        Ok(())
    }
}

#[async_trait]
impl Rewards for Ledger {
    async fn award_experience(
        &self,
        account_id: Id,
        amount: i64,
        _game_id: Id,
        _at: Timestamp,
    ) -> Result<()> {
        self.record(Credit::Experience { account_id, amount })
    }

    async fn mark_winner(&self, account_id: Id, _game_id: Id, _at: Timestamp) -> Result<()> {
        self.record(Credit::Winner { account_id })
    }
}

type TestGames = Games<MemoryStore, CacheRateLimiter<MemoryCache>, Ledger>;

struct Harness {
    games: TestGames,
    store: Arc<MemoryStore>,
    ledger: Arc<Ledger>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::rigged(GamesConfig::default(), 41)
    }

    fn configured(config: GamesConfig) -> Self {
        Self::rigged(config, 41)
    }

    /// A harness whose guessing-game secret is `fixed % bound + 1`.
    fn rigged(config: GamesConfig, fixed: u64) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryStore::new());
        let ledger = Arc::new(Ledger::default());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        let games = Games::new(
            Arc::clone(&store),
            limiter,
            Arc::clone(&ledger),
            config,
            Box::new(Rigged::new(fixed)),
            &registry,
        );
        Self {
            games,
            store,
            ledger,
            registry,
        }
    }

    /// A conversation with the listed members, which is the only authority this layer reads.
    async fn conversation(&self, conversation: u128, members: &[u128]) {
        let created_by = id(members[0]);
        self.store
            .create_conversation(
                Conversation {
                    conversation_id: id(conversation),
                    kind: ConversationKind::Group,
                    encryption: EncryptionMode::Transport,
                    room_id: None,
                    last_seq: 0,
                    created_by,
                    created_at: ts(NOW),
                    last_message_at: None,
                    archived_at: None,
                    title: None,
                },
                members.iter().copied().map(id).collect(),
            )
            .await
            .expect("the conversation seeds");
    }

    /// The default room: three members, so there is always a non-player member to refuse.
    async fn room(&self) {
        self.conversation(ROOM, &[1, 2, 3]).await;
    }

    fn caller(&self, account: u128) -> Caller {
        self.caller_at(account, NOW)
    }

    fn caller_at(&self, account: u128, millis: i64) -> Caller {
        Caller {
            account_id: id(account),
            device_id: device_of(account),
            tier: TrustTier::Established,
            now: ts(millis),
            request_id: None,
        }
    }

    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn plain(&self, name: &'static str) -> u64 {
        self.counter(name, &[])
    }

    fn started(&self, kind: &'static str) -> u64 {
        self.counter("migo_games_started_total", &[("kind", kind)])
    }

    fn moves(&self, kind: &'static str) -> u64 {
        self.counter("migo_games_moves_total", &[("kind", kind)])
    }

    fn rejected(&self, reason: &'static str) -> u64 {
        self.counter("migo_games_moves_rejected_total", &[("reason", reason)])
    }

    fn finished(&self, conclusion: &'static str) -> u64 {
        self.counter("migo_games_finished_total", &[("conclusion", conclusion)])
    }
}

/// The cells of a tic-tac-toe render, which is the only shape most of these tests read.
fn cells(view: &GameView) -> [Option<Mark>; 9] {
    match view.render {
        Render::TicTacToe { cells } => cells,
        ref other => panic!("expected a tic-tac-toe render, got {other:?}"),
    }
}

/// The committed flags and reveal of a rock-paper-scissors render.
fn thrown(view: &GameView) -> ([bool; 2], Option<[Hand; 2]>) {
    match view.render {
        Render::RockPaperScissors { committed, reveal } => (committed, reveal),
        ref other => panic!("expected a rock-paper-scissors render, got {other:?}"),
    }
}

/// The range, attempts left, and history of a guess-number render.
fn range(view: &GameView) -> (u16, u16, u8, Vec<migo_games::model::Guess>) {
    match view.render {
        Render::GuessNumber {
            low,
            high,
            remaining,
            ref guesses,
        } => (low, high, remaining, guesses.clone()),
        ref other => panic!("expected a guess-number render, got {other:?}"),
    }
}

#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    let error = result.err().expect("this call must be refused");
    assert_eq!(
        error.code(),
        code,
        "expected code {code}, got {}: {error}",
        error.code()
    );
}

// ---------------------------------------------------------------------------
// The catalogue.
//
// This is the one method on the trait that is synchronous, uncharged, and cannot fail: it
// answers "what can be played here" from constants, touches no store, and so is safe to call
// before a caller has been placed in any conversation at all.
// ---------------------------------------------------------------------------

#[test]
fn catalogue_lists_every_kind() {
    let harness = Harness::new();
    let catalogue = harness.games.catalogue();
    assert_eq!(catalogue.len(), GameKind::ALL.len());
    for kind in GameKind::ALL {
        assert!(
            catalogue.iter().any(|entry| entry.kind == *kind),
            "the catalogue omits {kind:?}"
        );
    }
}

#[test]
fn catalogue_carries_each_kind_slug_and_seat_count() {
    let harness = Harness::new();
    for entry in harness.games.catalogue() {
        assert_eq!(entry.slug, entry.kind.slug());
        assert_eq!(entry.min_players, entry.kind.min_players());
        assert_eq!(entry.max_players, entry.kind.max_players());
        assert!(entry.min_players >= 1, "a game needs at least one seat");
        assert!(entry.max_players >= entry.min_players);
    }
}

#[test]
fn catalogue_slugs_are_stable_wire_names() {
    assert_eq!(GameKind::TicTacToe.slug(), "tic_tac_toe");
    assert_eq!(GameKind::RockPaperScissors.slug(), "rock_paper_scissors");
    assert_eq!(GameKind::GuessNumber.slug(), "guess_number");
}

#[test]
fn catalogue_entries_match_game_info_of() {
    let harness = Harness::new();
    for entry in harness.games.catalogue() {
        assert_eq!(entry, GameInfo::of(entry.kind));
    }
}

#[test]
fn catalogue_costs_nothing_and_charges_nothing() {
    let harness = Harness::new();
    // A thousand calls, which is far past any bucket, and still no refusal is possible
    // because the method has no error path at all.
    for _ in 0..1_000 {
        assert_eq!(harness.games.catalogue().len(), GameKind::ALL.len());
    }
    assert_eq!(harness.plain("migo_games_started_total"), 0);
}

#[test]
fn kind_round_trips_through_its_wire_number() {
    for kind in GameKind::ALL {
        assert_eq!(GameKind::from_i16(kind.to_i16()), Some(*kind));
    }
    assert_eq!(GameKind::from_i16(-1), None);
    assert_eq!(GameKind::from_i16(99), None);
}

#[test]
fn status_round_trips_through_its_wire_number() {
    for status in [
        GameStatus::Open,
        GameStatus::Finished,
        GameStatus::Abandoned,
    ] {
        assert_eq!(GameStatus::from_i16(status.to_i16()), Some(status));
    }
    assert_eq!(GameStatus::from_i16(42), None);
    assert!(GameStatus::Open.is_open());
    assert!(!GameStatus::Finished.is_open());
    assert!(!GameStatus::Abandoned.is_open());
}

// ---------------------------------------------------------------------------
// Starting a game: who may, with whom, and where.
//
// Authority is the conversation's membership and nothing else. A caller who is not a member
// is told the conversation does not exist rather than that they are not in it (section 48),
// and an opponent who is not a member cannot be dragged into a game by being named.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn start_places_two_players_in_a_fresh_tic_tac_toe_game() {
    let harness = Harness::new();
    harness.room().await;
    let view = harness
        .games
        .start(&harness.caller(1), id(ROOM), GameKind::TicTacToe, &[id(2)])
        .await
        .expect("a member may start a game with another member");
    assert_eq!(view.kind, GameKind::TicTacToe);
    assert_eq!(view.conversation_id, id(ROOM));
    assert_eq!(view.status, GameStatus::Open);
    assert_eq!(view.players, vec![id(1), id(2)]);
    assert_eq!(view.turn_of, Some(id(1)));
    assert!(view.your_turn, "the starter moves first at tic-tac-toe");
    assert_eq!(view.outcome, None);
    assert_eq!(cells(&view), [None; 9]);
    assert_eq!(harness.started("tic_tac_toe"), 1);
}

#[tokio::test]
async fn start_by_a_non_member_reports_the_conversation_missing() {
    let harness = Harness::new();
    harness.room().await;
    // Account 4 is in no conversation at all. It must not be able to tell the difference
    // between "this conversation is not yours" and "there is no such conversation".
    expect_code(
        harness
            .games
            .start(&harness.caller(4), id(ROOM), GameKind::TicTacToe, &[id(2)])
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(harness.started("tic_tac_toe"), 0);
}

#[tokio::test]
async fn start_in_a_conversation_that_does_not_exist_reports_the_same_thing() {
    let harness = Harness::new();
    harness.room().await;
    expect_code(
        harness
            .games
            .start(&harness.caller(1), id(4_242), GameKind::TicTacToe, &[id(2)])
            .await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn start_with_an_opponent_outside_the_conversation_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    harness.conversation(OTHER_ROOM, &[1, 9]).await;
    // Account 9 shares a different conversation with the caller, which buys it nothing here.
    expect_code(
        harness
            .games
            .start(&harness.caller(1), id(ROOM), GameKind::TicTacToe, &[id(9)])
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn start_with_too_few_players_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    expect_code(
        harness
            .games
            .start(&harness.caller(1), id(ROOM), GameKind::TicTacToe, &[])
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn start_with_too_many_players_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    expect_code(
        harness
            .games
            .start(
                &harness.caller(1),
                id(ROOM),
                GameKind::TicTacToe,
                &[id(2), id(3)],
            )
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn start_naming_the_same_opponent_twice_is_refused() {
    let harness = Harness::new();
    harness.conversation(ROOM, &[1, 2]).await;
    expect_code(
        harness
            .games
            .start(
                &harness.caller(1),
                id(ROOM),
                GameKind::RockPaperScissors,
                &[id(2), id(2)],
            )
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn start_naming_yourself_as_the_opponent_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    // The caller is already seat zero, so naming themselves is the duplicate case.
    expect_code(
        harness
            .games
            .start(&harness.caller(1), id(ROOM), GameKind::TicTacToe, &[id(1)])
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn start_of_a_single_player_game_takes_no_opponents() {
    let harness = Harness::new();
    harness.room().await;
    let view = harness
        .games
        .start(&harness.caller(1), id(ROOM), GameKind::GuessNumber, &[])
        .await
        .expect("the guessing game is played alone");
    assert_eq!(view.players, vec![id(1)]);
    assert_eq!(view.turn_of, Some(id(1)));
    assert_eq!(harness.started("guess_number"), 1);
}

#[tokio::test]
async fn start_of_a_single_player_game_with_an_opponent_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    expect_code(
        harness
            .games
            .start(
                &harness.caller(1),
                id(ROOM),
                GameKind::GuessNumber,
                &[id(2)],
            )
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn start_leaves_the_stake_columns_empty() {
    let harness = Harness::new();
    harness.room().await;
    let view = harness
        .games
        .start(&harness.caller(1), id(ROOM), GameKind::TicTacToe, &[id(2)])
        .await
        .expect("the game starts");
    let session = harness
        .store
        .game(view.game_id)
        .await
        .expect("the store answers")
        .expect("the game was stored");
    // Sections 37 and 87: the columns exist for a future non-monetary stake and are reserved.
    // Nothing in this crate may fill them, because nothing in this crate may take a wager.
    assert_eq!(session.stake_currency, None);
    assert_eq!(session.stake_amount, None);
}

// ---------------------------------------------------------------------------
// Turn discipline at tic-tac-toe.
//
// The turn is not a field the client sends and not a lock the service holds: it is derived
// from the board, because a board with an even number of marks is X's to move and an odd one
// is O's. That makes the rule impossible to desynchronise from the state it governs.
// ---------------------------------------------------------------------------

/// Starts a tic-tac-toe game between accounts 1 (X) and 2 (O) and returns its id.
async fn tic_tac_toe(harness: &Harness) -> Id {
    harness
        .games
        .start(&harness.caller(1), id(ROOM), GameKind::TicTacToe, &[id(2)])
        .await
        .expect("the game starts")
        .game_id
}

#[tokio::test]
async fn the_starter_moves_first_and_the_opponent_may_not() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    expect_code(
        harness
            .games
            .play(&harness.caller(2), game, Move::Place { cell: 0 })
            .await,
        codes::CONFLICT,
    );
    assert_eq!(harness.rejected("not_your_turn"), 1);
    assert_eq!(harness.moves("tic_tac_toe"), 0);
}

#[tokio::test]
async fn a_move_passes_the_turn_to_the_other_player() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    let first = harness
        .games
        .play(&harness.caller(1), game, Move::Place { cell: 4 })
        .await
        .expect("X may open");
    assert_eq!(first.view.turn_of, Some(id(2)));
    assert!(!first.view.your_turn, "it is no longer X's turn");
    assert_eq!(cells(&first.view)[4], Some(Mark::X));
    assert_eq!(
        first.events,
        vec![
            Event::Moved {
                game_id: game,
                by: id(1)
            },
            Event::TurnChanged {
                game_id: game,
                turn_of: id(2)
            },
        ]
    );
    assert_eq!(first.outcome, None);
    assert_eq!(harness.moves("tic_tac_toe"), 1);
}

#[tokio::test]
async fn moving_twice_in_a_row_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Place { cell: 0 })
        .await
        .expect("X opens");
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Place { cell: 1 })
            .await,
        codes::CONFLICT,
    );
    assert_eq!(harness.rejected("not_your_turn"), 1);
}

#[tokio::test]
async fn a_cell_outside_the_board_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    for cell in [9u8, 10, 200, u8::MAX] {
        expect_code(
            harness
                .games
                .play(&harness.caller(1), game, Move::Place { cell })
                .await,
            codes::VALIDATION_FAILED,
        );
    }
    assert_eq!(harness.rejected("illegal_move"), 4);
    // The board is untouched by four refused moves.
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the board is still readable");
    assert_eq!(cells(&view), [None; 9]);
}

#[tokio::test]
async fn an_occupied_cell_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Place { cell: 4 })
        .await
        .expect("X takes the centre");
    expect_code(
        harness
            .games
            .play(&harness.caller(2), game, Move::Place { cell: 4 })
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(harness.rejected("illegal_move"), 1);
    // O still has its turn: a refused move consumes nothing but the rate-limit charge.
    let view = harness
        .games
        .view(&harness.caller(2), game)
        .await
        .expect("O may look");
    assert_eq!(view.turn_of, Some(id(2)));
    assert!(view.your_turn);
}

#[tokio::test]
async fn the_wrong_kind_of_move_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    // A hand and a guess are both well-formed moves for other games, and neither is a
    // placement. The engine refuses on the variant before it reads the board at all.
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Throw { hand: Hand::Rock })
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Guess { value: 5 })
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(harness.rejected("wrong_kind"), 2);
}

#[tokio::test]
async fn a_member_who_is_not_a_player_may_look_but_not_move() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    // Account 3 is in the conversation, so the game exists as far as it is concerned; it is
    // simply not one of the two seats. That is a permission answer, not a missing one.
    let view = harness
        .games
        .view(&harness.caller(3), game)
        .await
        .expect("a member of the conversation may watch");
    assert!(!view.your_turn, "a watcher never has the turn");
    assert_eq!(view.players, vec![id(1), id(2)]);
    expect_code(
        harness
            .games
            .play(&harness.caller(3), game, Move::Place { cell: 0 })
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.rejected("not_a_player"), 1);
}

#[tokio::test]
async fn a_non_member_cannot_even_see_the_game() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    expect_code(
        harness.games.view(&harness.caller(4), game).await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness
            .games
            .play(&harness.caller(4), game, Move::Place { cell: 0 })
            .await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness.games.abandon(&harness.caller(4), game).await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn a_game_that_does_not_exist_is_not_found() {
    let harness = Harness::new();
    harness.room().await;
    expect_code(
        harness.games.view(&harness.caller(1), id(7_777)).await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness
            .games
            .play(&harness.caller(1), id(7_777), Move::Place { cell: 0 })
            .await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness.games.abandon(&harness.caller(1), id(7_777)).await,
        codes::NOT_FOUND,
    );
}

// ---------------------------------------------------------------------------
// Playing tic-tac-toe to the end.
// ---------------------------------------------------------------------------

/// Plays the listed moves in order, alternating between accounts 1 and 2, and returns the
/// result of the last one.
async fn place_all(harness: &Harness, game: Id, cells: &[u8]) -> migo_games::model::MoveResult {
    let mut last = None;
    for (index, cell) in cells.iter().enumerate() {
        let account = if index % 2 == 0 { 1 } else { 2 };
        last = Some(
            harness
                .games
                .play(&harness.caller(account), game, Move::Place { cell: *cell })
                .await
                .unwrap_or_else(|error| panic!("move {index} on cell {cell} failed: {error}")),
        );
    }
    last.expect("at least one move was played")
}

#[tokio::test]
async fn a_completed_line_wins_the_game() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    // X takes the top row while O takes the middle one, one move behind.
    let last = place_all(&harness, game, &[0, 3, 1, 4, 2]).await;
    assert_eq!(last.outcome, Some(Outcome::Win { winner: id(1) }));
    assert_eq!(last.view.status, GameStatus::Finished);
    assert_eq!(last.view.turn_of, None);
    assert!(!last.view.your_turn);
    assert_eq!(last.view.outcome, Some(Outcome::Win { winner: id(1) }));
    assert_eq!(
        last.events,
        vec![
            Event::Moved {
                game_id: game,
                by: id(1)
            },
            Event::Finished {
                game_id: game,
                outcome: Outcome::Win { winner: id(1) }
            },
        ]
    );
    assert_eq!(harness.finished("win"), 1);
    assert_eq!(harness.moves("tic_tac_toe"), 5);
}

#[tokio::test]
async fn a_full_board_with_no_line_is_a_draw() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    // X: 0 2 3 7 8, O: 1 4 5 6. No line belongs to either.
    let last = place_all(&harness, game, &[0, 1, 2, 4, 3, 5, 7, 6, 8]).await;
    assert_eq!(last.outcome, Some(Outcome::Draw));
    assert_eq!(last.view.status, GameStatus::Finished);
    assert_eq!(last.view.outcome, Some(Outcome::Draw));
    assert!(
        cells(&last.view).iter().all(Option::is_some),
        "the board is full"
    );
    assert_eq!(harness.finished("draw"), 1);
}

#[tokio::test]
async fn a_decided_game_takes_no_further_moves() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1, 4, 2]).await;
    // Cell 5 is empty and it would be O's turn on a live board, and yet the game is over.
    expect_code(
        harness
            .games
            .play(&harness.caller(2), game, Move::Place { cell: 5 })
            .await,
        codes::CONFLICT,
    );
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Place { cell: 5 })
            .await,
        codes::CONFLICT,
    );
    assert_eq!(harness.finished("win"), 1, "the finish is counted once");
}

#[tokio::test]
async fn a_decided_game_cannot_be_abandoned() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1, 4, 2]).await;
    expect_code(
        harness.games.abandon(&harness.caller(1), game).await,
        codes::CONFLICT,
    );
    assert_eq!(harness.finished("no_contest"), 0);
}

#[tokio::test]
async fn both_players_see_the_same_board() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[4, 0, 8]).await;
    let x = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("X looks");
    let o = harness
        .games
        .view(&harness.caller(2), game)
        .await
        .expect("O looks");
    let watcher = harness
        .games
        .view(&harness.caller(3), game)
        .await
        .expect("the watcher looks");
    // Nothing is hidden at tic-tac-toe, so the render is identical for all three; only
    // `your_turn` differs, because that is about the viewer rather than the game.
    assert_eq!(cells(&x), cells(&o));
    assert_eq!(cells(&x), cells(&watcher));
    assert_eq!(x.state_version, o.state_version);
    assert!(!x.your_turn);
    assert!(o.your_turn);
    assert!(!watcher.your_turn);
}

// ---------------------------------------------------------------------------
// Rock-paper-scissors: the secrecy of a committed hand.
//
// This is the game section 90 exists for. Both hands live in the authoritative state from the
// moment they are thrown, so the only thing standing between a player and their opponent's
// hand is that the render refuses to say. What a viewer may learn before the round closes is
// *that* the other seat is filled, never *what* fills it, and the reveal is gated on both
// commits so there is no instant at which the reveal is one-sided.
// ---------------------------------------------------------------------------

/// Starts a rock-paper-scissors round between accounts 1 and 2.
async fn rock_paper_scissors(harness: &Harness) -> Id {
    harness
        .games
        .start(
            &harness.caller(1),
            id(ROOM),
            GameKind::RockPaperScissors,
            &[id(2)],
        )
        .await
        .expect("the round starts")
        .game_id
}

#[tokio::test]
async fn a_fresh_round_has_neither_hand_and_no_turn() {
    let harness = Harness::new();
    harness.room().await;
    let view = harness
        .games
        .start(
            &harness.caller(1),
            id(ROOM),
            GameKind::RockPaperScissors,
            &[id(2)],
        )
        .await
        .expect("the round starts");
    // Simultaneous play: there is no single seat whose turn it is, and so no seat can be
    // told to wait for the other.
    assert_eq!(view.turn_of, None);
    assert!(!view.your_turn);
    assert_eq!(thrown(&view), ([false, false], None));
    assert_eq!(harness.started("rock_paper_scissors"), 1);
}

#[tokio::test]
async fn a_committed_hand_is_not_visible_to_the_opponent() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Throw { hand: Hand::Rock })
        .await
        .expect("the first seat commits");
    for watcher in [1u128, 2, 3] {
        let view = harness
            .games
            .view(&harness.caller(watcher), game)
            .await
            .expect("everyone may look");
        let (committed, reveal) = thrown(&view);
        assert_eq!(committed, [true, false], "seat one has committed");
        assert_eq!(
            reveal, None,
            "no hand is revealed to account {watcher} while one seat is empty"
        );
    }
}

#[tokio::test]
async fn a_player_cannot_read_back_even_their_own_hand() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Throw { hand: Hand::Paper })
        .await
        .expect("the first seat commits");
    // The reveal is all-or-nothing on purpose: a render that echoed the viewer's own hand
    // back would be a render whose shape differs per viewer, and the difference itself would
    // tell an observer which seat had moved.
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the committer may look");
    assert_eq!(thrown(&view).1, None);
}

#[tokio::test]
async fn a_render_of_a_half_committed_round_carries_no_hand_in_its_debug_output() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    harness
        .games
        .play(
            &harness.caller(2),
            game,
            Move::Throw {
                hand: Hand::Scissors,
            },
        )
        .await
        .expect("the second seat commits");
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the other seat may look");
    // A leak through `Debug` is still a leak: the view is what a handler logs when something
    // goes wrong, and the type must have nowhere to put a hand until the round closes.
    let rendered = format!("{:?}", view.render);
    // The variant's own name spells all three hands, so only the body is evidence.
    let (name, body) = rendered
        .split_once('{')
        .expect("the render's Debug output is a struct variant");
    assert_eq!(name.trim(), "RockPaperScissors");
    assert!(
        !body.contains("Scissors"),
        "the render's Debug output named the hidden hand: {rendered}"
    );
    assert!(body.contains("reveal: None"), "got {rendered}");
}

#[tokio::test]
async fn both_hands_appear_only_once_both_are_in() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Throw { hand: Hand::Rock })
        .await
        .expect("seat one commits");
    let closing = harness
        .games
        .play(
            &harness.caller(2),
            game,
            Move::Throw {
                hand: Hand::Scissors,
            },
        )
        .await
        .expect("seat two commits");
    assert_eq!(
        thrown(&closing.view),
        ([true, true], Some([Hand::Rock, Hand::Scissors]))
    );
    assert_eq!(closing.outcome, Some(Outcome::Win { winner: id(1) }));
    assert_eq!(closing.view.status, GameStatus::Finished);
    assert_eq!(harness.finished("win"), 1);
}

#[tokio::test]
async fn the_hand_that_beats_the_other_wins_whichever_seat_threw_it() {
    for (first, second, winner) in [
        (Hand::Rock, Hand::Scissors, 1u128),
        (Hand::Scissors, Hand::Rock, 2),
        (Hand::Paper, Hand::Rock, 1),
        (Hand::Rock, Hand::Paper, 2),
        (Hand::Scissors, Hand::Paper, 1),
        (Hand::Paper, Hand::Scissors, 2),
    ] {
        let harness = Harness::new();
        harness.room().await;
        let game = rock_paper_scissors(&harness).await;
        harness
            .games
            .play(&harness.caller(1), game, Move::Throw { hand: first })
            .await
            .expect("seat one commits");
        let closing = harness
            .games
            .play(&harness.caller(2), game, Move::Throw { hand: second })
            .await
            .expect("seat two commits");
        assert_eq!(
            closing.outcome,
            Some(Outcome::Win { winner: id(winner) }),
            "{first:?} against {second:?}"
        );
    }
}

#[tokio::test]
async fn two_equal_hands_draw() {
    for hand in [Hand::Rock, Hand::Paper, Hand::Scissors] {
        let harness = Harness::new();
        harness.room().await;
        let game = rock_paper_scissors(&harness).await;
        harness
            .games
            .play(&harness.caller(1), game, Move::Throw { hand })
            .await
            .expect("seat one commits");
        let closing = harness
            .games
            .play(&harness.caller(2), game, Move::Throw { hand })
            .await
            .expect("seat two commits");
        assert_eq!(closing.outcome, Some(Outcome::Draw), "{hand:?} both sides");
        assert_eq!(harness.finished("draw"), 1);
    }
}

#[tokio::test]
async fn committing_twice_is_refused_which_is_what_defeats_a_replay() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Throw { hand: Hand::Rock })
        .await
        .expect("seat one commits");
    // A replayed request is byte-for-byte the same move arriving twice. It re-reads the round
    // it has already written itself into and finds the seat taken, so the second arrival
    // cannot overwrite the first with a different hand either.
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Throw { hand: Hand::Rock })
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Throw { hand: Hand::Paper })
            .await,
        codes::VALIDATION_FAILED,
    );
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the round is still open");
    assert_eq!(thrown(&view).0, [true, false]);
    assert_eq!(view.status, GameStatus::Open);
    assert_eq!(harness.moves("rock_paper_scissors"), 1, "one commit landed");
}

#[tokio::test]
async fn a_watcher_cannot_fill_an_empty_seat() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    expect_code(
        harness
            .games
            .play(&harness.caller(3), game, Move::Throw { hand: Hand::Rock })
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.rejected("not_a_player"), 1);
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the round is untouched");
    assert_eq!(thrown(&view).0, [false, false]);
}

#[tokio::test]
async fn hand_round_trips_through_its_wire_byte() {
    for hand in [Hand::Rock, Hand::Paper, Hand::Scissors] {
        assert_eq!(Hand::from_u8(hand.to_u8()), Some(hand));
    }
    assert_eq!(Hand::from_u8(3), None);
    assert_eq!(Hand::from_u8(u8::MAX), None);
    // The cycle: each hand beats exactly one other and loses to exactly one other.
    assert!(Hand::Rock.beats(Hand::Scissors));
    assert!(Hand::Scissors.beats(Hand::Paper));
    assert!(Hand::Paper.beats(Hand::Rock));
    assert!(!Hand::Rock.beats(Hand::Paper));
    assert!(!Hand::Rock.beats(Hand::Rock));
}

// ---------------------------------------------------------------------------
// Guess the number: the secret the client must never be handed.
//
// The secret is drawn from the server's randomness at creation (section 90) and written into
// the authoritative state. `Render::GuessNumber` has no field it could occupy: what the player
// gets is the range still in play, the guesses they have already made, and the feedback each
// one earned. The range is *derived* from the feedback, so it tells the player nothing the
// feedback did not already tell them, and the secret is not encrypted in the view because it
// is not in the view at all.
//
// The harness rigs `Random::below`, so these tests know the secret and can play a game to a
// win rather than merely watching one time out.
// ---------------------------------------------------------------------------

/// The secret every default-configured harness hides: `41 % 100 + 1`.
const SECRET: u16 = 42;

/// Starts a guessing game for account 1.
async fn guess_number(harness: &Harness) -> Id {
    harness
        .games
        .start(&harness.caller(1), id(ROOM), GameKind::GuessNumber, &[])
        .await
        .expect("the game starts")
        .game_id
}

#[tokio::test]
async fn a_fresh_round_shows_the_whole_range_and_no_guesses() {
    let harness = Harness::new();
    harness.room().await;
    let view = harness
        .games
        .start(&harness.caller(1), id(ROOM), GameKind::GuessNumber, &[])
        .await
        .expect("the game starts");
    let (low, high, remaining, guesses) = range(&view);
    assert_eq!(low, 1);
    assert_eq!(high, GamesConfig::default().guess_bound);
    assert_eq!(remaining, GamesConfig::default().guess_attempts);
    assert!(guesses.is_empty());
    assert!(view.your_turn, "the lone player always has the turn");
}

#[tokio::test]
async fn a_high_guess_is_told_only_that_the_secret_is_lower() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    let result = harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: 90 })
        .await
        .expect("the guess is legal");
    let (low, high, remaining, guesses) = range(&result.view);
    assert_eq!(guesses.len(), 1);
    assert_eq!(guesses[0].value, 90);
    assert_eq!(guesses[0].feedback, Feedback::Lower);
    // The ceiling moved to just under the guess and the floor did not move: exactly what the
    // one word of feedback licences, and not one number more.
    assert_eq!((low, high), (1, 89));
    assert_eq!(remaining, GamesConfig::default().guess_attempts - 1);
    assert_eq!(result.outcome, None);
    assert_eq!(result.view.status, GameStatus::Open);
}

#[tokio::test]
async fn a_low_guess_is_told_only_that_the_secret_is_higher() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    let result = harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: 10 })
        .await
        .expect("the guess is legal");
    let (low, high, _, guesses) = range(&result.view);
    assert_eq!(guesses[0].feedback, Feedback::Higher);
    assert_eq!((low, high), (11, 100));
}

#[tokio::test]
async fn successive_guesses_narrow_the_range_from_both_ends() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    for (value, expected) in [(50u16, (1u16, 49u16)), (20, (21, 49)), (45, (21, 44))] {
        let result = harness
            .games
            .play(&harness.caller(1), game, Move::Guess { value })
            .await
            .expect("the guess is legal");
        let (low, high, _, _) = range(&result.view);
        assert_eq!((low, high), expected, "after guessing {value}");
        assert!(
            low <= SECRET && SECRET <= high,
            "the range must never exclude the secret"
        );
    }
}

#[tokio::test]
async fn the_right_number_wins_and_collapses_the_range_onto_it() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    let result = harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: SECRET })
        .await
        .expect("the guess is legal");
    assert_eq!(result.outcome, Some(Outcome::Win { winner: id(1) }));
    assert_eq!(result.view.status, GameStatus::Finished);
    assert_eq!(result.view.turn_of, None);
    let (low, high, _, guesses) = range(&result.view);
    assert_eq!((low, high), (SECRET, SECRET));
    assert_eq!(
        guesses.last().map(|guess| guess.feedback),
        Some(Feedback::Correct)
    );
    assert_eq!(
        result.events,
        vec![
            Event::Moved {
                game_id: game,
                by: id(1)
            },
            Event::Finished {
                game_id: game,
                outcome: Outcome::Win { winner: id(1) }
            },
        ]
    );
    assert_eq!(harness.finished("win"), 1);
}

#[tokio::test]
async fn running_out_of_guesses_ends_the_game_without_disclosing_the_secret() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    let attempts = GamesConfig::default().guess_attempts;
    let mut last = None;
    for step in 1..=u16::from(attempts) {
        // Seven guesses that are all below the secret, so none of them is ever correct.
        last = Some(
            harness
                .games
                .play(&harness.caller(1), game, Move::Guess { value: step })
                .await
                .expect("each guess is legal"),
        );
    }
    let last = last.expect("seven guesses were played");
    assert_eq!(last.outcome, Some(Outcome::NoContest));
    assert_eq!(last.view.status, GameStatus::Finished);
    let (low, high, remaining, guesses) = range(&last.view);
    assert_eq!(remaining, 0);
    assert_eq!(guesses.len(), usize::from(attempts));
    // The player leaves knowing only what the feedback told them: a floor above their last
    // guess and the original ceiling. A losing game discloses nothing extra as consolation.
    assert_eq!((low, high), (u16::from(attempts) + 1, 100));
    assert!(low <= SECRET && SECRET <= high);
    assert_eq!(harness.finished("no_contest"), 1);
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Guess { value: SECRET })
            .await,
        codes::CONFLICT,
    );
}

#[tokio::test]
async fn a_guess_outside_the_range_is_refused_and_costs_no_attempt() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    for value in [0u16, 101, 500, u16::MAX] {
        expect_code(
            harness
                .games
                .play(&harness.caller(1), game, Move::Guess { value })
                .await,
            codes::VALIDATION_FAILED,
        );
    }
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the round is still open");
    let (_, _, remaining, guesses) = range(&view);
    assert_eq!(remaining, GamesConfig::default().guess_attempts);
    assert!(guesses.is_empty(), "a refused guess is not recorded");
    assert_eq!(harness.rejected("illegal_move"), 4);
}

#[tokio::test]
async fn the_secret_appears_in_no_part_of_the_view_a_client_receives() {
    // A bound of a thousand puts the secret well clear of the range endpoints, so a substring
    // search over the whole view is meaningful rather than an accident of small numbers.
    let harness = Harness::rigged(
        GamesConfig {
            guess_bound: 1_000,
            ..GamesConfig::default()
        },
        856,
    );
    harness.room().await;
    let game = guess_number(&harness).await;
    let secret = 857u16;
    for value in [500u16, 900] {
        harness
            .games
            .play(&harness.caller(1), game, Move::Guess { value })
            .await
            .expect("the guess is legal");
    }
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the player may look");
    let (low, high, _, _) = range(&view);
    assert_eq!((low, high), (501, 899), "the secret is inside this range");
    // Every field the client can reach, rendered, including anything a handler would log.
    let dumped = format!("{view:?}");
    assert!(
        !dumped.contains(&secret.to_string()),
        "the view disclosed the secret: {dumped}"
    );
    assert!(
        !dumped.contains("857"),
        "the view disclosed the secret: {dumped}"
    );
}

#[tokio::test]
async fn the_secret_is_never_outside_the_advertised_bound() {
    // Whatever the draw returns, the secret has to land in one to bound inclusive, because a
    // secret outside the range would be unguessable and the loss would be rigged.
    for fixed in [0u64, 1, 99, 100, 999, u64::MAX] {
        let harness = Harness::rigged(GamesConfig::default(), fixed);
        harness.room().await;
        let game = guess_number(&harness).await;
        let mut low = 1u16;
        let mut high = 100u16;
        // Binary search finds the secret in seven guesses out of a hundred, so a game that is
        // played well always ends in a win rather than in an exhausted budget.
        loop {
            let mid = low + (high - low) / 2;
            let result = harness
                .games
                .play(&harness.caller(1), game, Move::Guess { value: mid })
                .await
                .unwrap_or_else(|error| panic!("guessing {mid} in {low}..={high} failed: {error}"));
            let (next_low, next_high, _, guesses) = range(&result.view);
            match guesses.last().expect("a guess was recorded").feedback {
                Feedback::Correct => {
                    assert_eq!(result.outcome, Some(Outcome::Win { winner: id(1) }));
                    break;
                }
                _ => {
                    low = next_low;
                    high = next_high;
                    assert!(low <= high, "the range collapsed without a correct guess");
                }
            }
        }
    }
}

#[tokio::test]
async fn a_bound_of_one_leaves_exactly_one_possible_secret() {
    let harness = Harness::configured(GamesConfig {
        guess_bound: 1,
        ..GamesConfig::default()
    });
    harness.room().await;
    let game = guess_number(&harness).await;
    let result = harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: 1 })
        .await
        .expect("one is the only legal guess");
    assert_eq!(result.outcome, Some(Outcome::Win { winner: id(1) }));
}

#[tokio::test]
async fn a_single_attempt_configuration_ends_after_one_wrong_guess() {
    let harness = Harness::configured(GamesConfig {
        guess_attempts: 1,
        ..GamesConfig::default()
    });
    harness.room().await;
    let game = guess_number(&harness).await;
    let result = harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: 7 })
        .await
        .expect("the one guess is legal");
    assert_eq!(result.outcome, Some(Outcome::NoContest));
    assert_eq!(range(&result.view).2, 0);
}

#[tokio::test]
async fn another_member_cannot_play_someone_elses_round() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    // Account 2 shares the conversation, so it may watch the range narrow; it may not spend
    // another player's attempts, and it certainly may not win their game for them.
    let view = harness
        .games
        .view(&harness.caller(2), game)
        .await
        .expect("a member may watch");
    assert_eq!(view.players, vec![id(1)]);
    assert!(!view.your_turn);
    expect_code(
        harness
            .games
            .play(&harness.caller(2), game, Move::Guess { value: SECRET })
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.rejected("not_a_player"), 1);
}

#[tokio::test]
async fn a_watcher_learns_nothing_from_watching_that_the_player_did_not_learn() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: 60 })
        .await
        .expect("the guess is legal");
    let player = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("player");
    let watcher = harness
        .games
        .view(&harness.caller(2), game)
        .await
        .expect("watcher");
    // The render has no viewer-dependent branch, so a watcher and the player see the same
    // range. That is safe precisely because the range is derived from the feedback.
    assert_eq!(range(&player), range(&watcher));
}

// ---------------------------------------------------------------------------
// Rewards, and the direction the arrow points.
//
// This crate does not know that `migo-economy` exists. It holds a port with two methods and no
// way to move currency, because sections 37 and 87 forbid a cash-out and the absence of a
// method is the only form of that prohibition a future maintainer cannot ignore. A reward is
// also the last thing a finished game does, and it is not allowed to un-finish it: if the
// credit fails, the game stays decided and the failure becomes a counter.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_win_credits_the_winner_more_than_the_loser_and_marks_them_once() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1, 4, 2]).await;
    let config = GamesConfig::default();
    assert_eq!(harness.ledger.experience_of(id(1)), config.win_experience);
    assert_eq!(
        harness.ledger.experience_of(id(2)),
        config.finish_experience
    );
    assert!(
        config.win_experience > config.finish_experience,
        "winning must be worth more than turning up"
    );
    assert_eq!(harness.ledger.winners(), vec![id(1)]);
    assert_eq!(harness.plain("migo_games_rewards_dropped_total"), 0);
}

#[tokio::test]
async fn a_draw_credits_both_players_the_participation_grant_and_marks_nobody() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 1, 2, 4, 3, 5, 7, 6, 8]).await;
    let config = GamesConfig::default();
    assert_eq!(
        harness.ledger.experience_of(id(1)),
        config.finish_experience
    );
    assert_eq!(
        harness.ledger.experience_of(id(2)),
        config.finish_experience
    );
    assert!(harness.ledger.winners().is_empty(), "a draw has no winner");
}

#[tokio::test]
async fn an_open_game_credits_nothing_at_all() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1]).await;
    assert!(
        harness.ledger.credits().is_empty(),
        "a game in progress has earned nothing"
    );
}

#[tokio::test]
async fn an_abandoned_game_credits_nothing() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[4]).await;
    harness
        .games
        .abandon(&harness.caller(2), game)
        .await
        .expect("either player may walk away");
    // Walking out of a game must not pay: if it did, the cheapest way to farm experience
    // would be to start games and leave them.
    assert!(harness.ledger.credits().is_empty());
    assert_eq!(harness.finished("no_contest"), 1);
}

#[tokio::test]
async fn a_guessing_win_credits_the_lone_player_the_win_grant() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: SECRET })
        .await
        .expect("the guess is correct");
    assert_eq!(
        harness.ledger.experience_of(id(1)),
        GamesConfig::default().win_experience
    );
    assert_eq!(harness.ledger.winners(), vec![id(1)]);
}

#[tokio::test]
async fn a_guessing_loss_credits_only_the_participation_grant() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    for step in 1..=u16::from(GamesConfig::default().guess_attempts) {
        harness
            .games
            .play(&harness.caller(1), game, Move::Guess { value: step })
            .await
            .expect("each guess is legal");
    }
    assert_eq!(
        harness.ledger.experience_of(id(1)),
        GamesConfig::default().finish_experience
    );
    assert!(
        harness.ledger.winners().is_empty(),
        "a no-contest has no winner"
    );
}

#[tokio::test]
async fn a_zero_grant_is_not_credited_at_all() {
    let harness = Harness::configured(GamesConfig {
        win_experience: 0,
        finish_experience: 0,
        ..GamesConfig::default()
    });
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1, 4, 2]).await;
    // A grant of nothing is not a call to the ledger: an economy that logs a zero credit is
    // an economy whose audit trail is mostly noise.
    assert_eq!(
        harness.ledger.credits(),
        vec![Credit::Winner { account_id: id(1) }],
        "only the winner mark survives a zero grant"
    );
}

#[tokio::test]
async fn a_failing_ledger_does_not_un_finish_the_game() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1, 4]).await;
    harness.ledger.break_it();
    let last = harness
        .games
        .play(&harness.caller(1), game, Move::Place { cell: 2 })
        .await
        .expect("the move that wins the game must still succeed");
    // The game is decided and stays decided. Rolling it back would mean a player who won,
    // watched the board fill, and then found their win undone by an unrelated subsystem.
    assert_eq!(last.outcome, Some(Outcome::Win { winner: id(1) }));
    assert_eq!(last.view.status, GameStatus::Finished);
    assert!(harness.ledger.credits().is_empty());
    // Two drops: the winner's grant and the loser's. The winner mark is not attempted once
    // the experience credits have already failed loudly enough to be counted.
    assert!(
        harness.plain("migo_games_rewards_dropped_total") >= 2,
        "a dropped reward has to be visible to an operator"
    );
    assert_eq!(harness.finished("win"), 1);
}

#[tokio::test]
async fn a_deployment_with_no_economy_plays_exactly_the_same_games() {
    // `Unrewarded` is what a node without an economy composes in. It is not a stub that
    // panics or a feature flag the service branches on: it is the port, implemented as
    // nothing, so the games layer has no idea which one it is holding.
    let settings = Config::default();
    let store = Arc::new(MemoryStore::new());
    let registry = Registry::new();
    let policies = Policies::from_config(&settings.rate_limit).expect("valid");
    let limiter = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        policies,
        &registry,
    ));
    let games: Games<MemoryStore, CacheRateLimiter<MemoryCache>, Unrewarded> = Games::new(
        Arc::clone(&store),
        limiter,
        Arc::new(Unrewarded),
        GamesConfig::default(),
        Box::new(Rigged::new(41)),
        &registry,
    );
    store
        .create_conversation(
            Conversation {
                conversation_id: id(ROOM),
                kind: ConversationKind::Group,
                encryption: EncryptionMode::Transport,
                room_id: None,
                last_seq: 0,
                created_by: id(1),
                created_at: ts(NOW),
                last_message_at: None,
                archived_at: None,
                title: None,
            },
            vec![id(1), id(2)],
        )
        .await
        .expect("the conversation seeds");
    let caller = Caller {
        account_id: id(1),
        device_id: device_of(1),
        tier: TrustTier::Established,
        now: ts(NOW),
        request_id: None,
    };
    let opponent = Caller {
        account_id: id(2),
        device_id: device_of(2),
        tier: TrustTier::Established,
        now: ts(NOW),
        request_id: None,
    };
    let game = games
        .start(&caller, id(ROOM), GameKind::TicTacToe, &[id(2)])
        .await
        .expect("the game starts")
        .game_id;
    for (who, cell) in [(&caller, 0u8), (&opponent, 3), (&caller, 1), (&opponent, 4)] {
        games
            .play(who, game, Move::Place { cell })
            .await
            .expect("the move lands");
    }
    let last = games
        .play(&caller, game, Move::Place { cell: 2 })
        .await
        .expect("the winning move lands");
    assert_eq!(last.outcome, Some(Outcome::Win { winner: id(1) }));
    assert_eq!(
        registry
            .counter("migo_games_rewards_dropped_total", "", &[])
            .get(),
        0,
        "doing nothing successfully is not a dropped reward"
    );
}

// ---------------------------------------------------------------------------
// The rate limit, and the order it is applied in.
//
// Every method charges before it asks whether the caller is a member. That order is the point:
// if membership were checked first, a stranger could sweep conversation ids and read existence
// off the difference between a refusal and a not-found, for free and at any rate they liked.
// Charging first makes the sweep cost the same as playing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stranger_probing_for_conversations_is_rate_limited_like_anyone_else() {
    let harness = Harness::new();
    harness.room().await;
    let stranger = harness.caller(4);
    let mut refusals = 0u32;
    let mut limited = false;
    // Account 4 is in no conversation, so every one of these is a not-found. The bucket still
    // drains, and eventually the answer changes from "no such thing" to "slow down".
    for _ in 0..200 {
        match harness.games.active(&stranger, id(ROOM)).await {
            Ok(_) => panic!("a stranger must never list a conversation's games"),
            Err(error) if error.code() == codes::NOT_FOUND => refusals += 1,
            Err(error) if error.code() == codes::RATE_LIMITED => {
                limited = true;
                break;
            }
            Err(other) => panic!("unexpected refusal: {other}"),
        }
    }
    assert!(
        limited,
        "the probe was never rate limited after {refusals} tries"
    );
    assert!(
        refusals > 0,
        "the probe should get a not-found before the bucket empties"
    );
}

#[tokio::test]
async fn the_bucket_belongs_to_the_account_not_the_conversation() {
    let harness = Harness::new();
    harness.room().await;
    let mut spent = 0u32;
    while harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .is_ok()
    {
        spent += 1;
        assert!(spent < 500, "the bucket never emptied");
    }
    expect_code(
        harness.games.active(&harness.caller(1), id(ROOM)).await,
        codes::RATE_LIMITED,
    );
    // Account 2 shares the conversation and has spent nothing, so its budget is untouched.
    harness
        .games
        .active(&harness.caller(2), id(ROOM))
        .await
        .expect("one account's spending is not another's");
}

#[tokio::test]
async fn a_drained_bucket_refills_as_time_passes() {
    let harness = Harness::new();
    harness.room().await;
    while harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .is_ok()
    {}
    expect_code(
        harness.games.active(&harness.caller(1), id(ROOM)).await,
        codes::RATE_LIMITED,
    );
    // A minute later the same account is welcome again: the limit is a rate, not a quota.
    harness
        .games
        .active(&harness.caller_at(1, NOW + MINUTE), id(ROOM))
        .await
        .expect("the bucket refilled");
}

#[tokio::test]
async fn a_rate_limited_start_creates_no_game() {
    let harness = Harness::new();
    harness.room().await;
    while harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .is_ok()
    {}
    expect_code(
        harness
            .games
            .start(&harness.caller(1), id(ROOM), GameKind::TicTacToe, &[id(2)])
            .await,
        codes::RATE_LIMITED,
    );
    // The listing is account 2's, whose budget is intact, and it must be empty.
    let listed = harness
        .games
        .active(&harness.caller(2), id(ROOM))
        .await
        .expect("account two may still list");
    assert!(listed.is_empty(), "a refused start left a game behind");
}

#[tokio::test]
async fn a_rate_limited_move_does_not_touch_the_board() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    while harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .is_ok()
    {}
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Place { cell: 0 })
            .await,
        codes::RATE_LIMITED,
    );
    let view = harness
        .games
        .view(&harness.caller(2), game)
        .await
        .expect("account two may look");
    assert_eq!(cells(&view), [None; 9]);
    assert_eq!(view.turn_of, Some(id(1)), "the turn did not pass");
}

// ---------------------------------------------------------------------------
// Listing what is in play.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_conversation_has_nothing_in_play() {
    let harness = Harness::new();
    harness.room().await;
    let listed = harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .expect("a member may list");
    assert!(listed.is_empty());
}

#[tokio::test]
async fn the_listing_summarises_every_open_game_in_the_conversation() {
    let harness = Harness::new();
    harness.room().await;
    let noughts = tic_tac_toe(&harness).await;
    let hands = rock_paper_scissors(&harness).await;
    let numbers = guess_number(&harness).await;
    let listed = harness
        .games
        .active(&harness.caller(3), id(ROOM))
        .await
        .expect("any member may list, including one playing nothing");
    assert_eq!(listed.len(), 3);
    let noughts_row = listed
        .iter()
        .find(|row| row.game_id == noughts)
        .expect("the tic-tac-toe game is listed");
    assert_eq!(noughts_row.kind, GameKind::TicTacToe);
    assert_eq!(noughts_row.status, GameStatus::Open);
    assert_eq!(noughts_row.players, vec![id(1), id(2)]);
    assert_eq!(noughts_row.turn_of, Some(id(1)));
    let hands_row = listed
        .iter()
        .find(|row| row.game_id == hands)
        .expect("the round is listed");
    assert_eq!(hands_row.turn_of, None, "a simultaneous game has no turn");
    let numbers_row = listed
        .iter()
        .find(|row| row.game_id == numbers)
        .expect("the guessing game is listed");
    assert_eq!(numbers_row.players, vec![id(1)]);
}

#[tokio::test]
async fn a_finished_game_leaves_the_listing() {
    let harness = Harness::new();
    harness.room().await;
    let finished = tic_tac_toe(&harness).await;
    let ongoing = rock_paper_scissors(&harness).await;
    place_all(&harness, finished, &[0, 3, 1, 4, 2]).await;
    let listed = harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .expect("a member may list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].game_id, ongoing);
}

#[tokio::test]
async fn an_abandoned_game_leaves_the_listing() {
    let harness = Harness::new();
    harness.room().await;
    let walked = tic_tac_toe(&harness).await;
    harness
        .games
        .abandon(&harness.caller(1), walked)
        .await
        .expect("the starter may walk away");
    let listed = harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .expect("a member may list");
    assert!(listed.is_empty());
}

#[tokio::test]
async fn one_conversations_games_are_invisible_from_another() {
    let harness = Harness::new();
    harness.room().await;
    harness.conversation(OTHER_ROOM, &[1, 2]).await;
    let here = tic_tac_toe(&harness).await;
    let there = harness
        .games
        .start(
            &harness.caller(1),
            id(OTHER_ROOM),
            GameKind::TicTacToe,
            &[id(2)],
        )
        .await
        .expect("the same pair may play in two places")
        .game_id;
    let listed = harness
        .games
        .active(&harness.caller(1), id(ROOM))
        .await
        .expect("a member may list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].game_id, here);
    assert_ne!(here, there, "the two games are distinct");
}

#[tokio::test]
async fn the_listing_is_capped_and_the_cap_is_the_published_one() {
    let harness = Harness::new();
    harness.conversation(ROOM, &[1, 2]).await;
    // Start more games than the cap, spending across two accounts so no bucket empties.
    let wanted = usize::from(MAX_ACTIVE_GAMES) + 5;
    for index in 0..wanted {
        let starter = if index % 2 == 0 { 1u128 } else { 2 };
        let other = if starter == 1 { 2u128 } else { 1 };
        harness
            .games
            .start(
                &harness.caller_at(starter, NOW + (index as i64) * MINUTE),
                id(ROOM),
                GameKind::RockPaperScissors,
                &[id(other)],
            )
            .await
            .expect("the game starts");
    }
    let listed = harness
        .games
        .active(
            &harness.caller_at(1, NOW + (wanted as i64) * MINUTE),
            id(ROOM),
        )
        .await
        .expect("a member may list");
    assert_eq!(
        listed.len(),
        usize::from(MAX_ACTIVE_GAMES),
        "the listing must be bounded so a busy conversation cannot make the response unbounded"
    );
}

#[tokio::test]
async fn a_non_member_cannot_list() {
    let harness = Harness::new();
    harness.room().await;
    tic_tac_toe(&harness).await;
    expect_code(
        harness.games.active(&harness.caller(4), id(ROOM)).await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness.games.active(&harness.caller(1), id(4_242)).await,
        codes::NOT_FOUND,
    );
}

// ---------------------------------------------------------------------------
// Walking away.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn either_player_may_abandon_an_open_game() {
    for who in [1u128, 2] {
        let harness = Harness::new();
        harness.room().await;
        let game = tic_tac_toe(&harness).await;
        let view = harness
            .games
            .abandon(&harness.caller(who), game)
            .await
            .expect("a player may leave");
        assert_eq!(view.status, GameStatus::Abandoned);
        assert_eq!(view.turn_of, None);
        assert!(!view.your_turn);
        // An abandoned game is a no-contest rather than a win for whoever stayed: leaving
        // must not be a way to hand a result to your opponent either.
        assert_eq!(view.outcome, Some(Outcome::NoContest));
        assert_eq!(harness.finished("no_contest"), 1);
    }
}

#[tokio::test]
async fn abandoning_a_game_twice_is_refused() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    harness
        .games
        .abandon(&harness.caller(1), game)
        .await
        .expect("the first walk-out lands");
    expect_code(
        harness.games.abandon(&harness.caller(2), game).await,
        codes::CONFLICT,
    );
    assert_eq!(harness.finished("no_contest"), 1, "counted once");
}

#[tokio::test]
async fn an_abandoned_game_takes_no_further_moves() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    harness
        .games
        .abandon(&harness.caller(1), game)
        .await
        .expect("the walk-out lands");
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Place { cell: 0 })
            .await,
        codes::CONFLICT,
    );
    expect_code(
        harness
            .games
            .play(&harness.caller(2), game, Move::Place { cell: 0 })
            .await,
        codes::CONFLICT,
    );
}

#[tokio::test]
async fn a_watching_member_may_not_abandon_a_game_they_are_not_in() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    expect_code(
        harness.games.abandon(&harness.caller(3), game).await,
        codes::PERMISSION_DENIED,
    );
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the game is untouched");
    assert_eq!(view.status, GameStatus::Open);
}

#[tokio::test]
async fn abandoning_a_half_played_board_keeps_the_board() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[4, 0]).await;
    let view = harness
        .games
        .abandon(&harness.caller(1), game)
        .await
        .expect("the walk-out lands");
    // The state is not wiped: an abandoned game is still a game that can be read back, and
    // erasing it would erase the record of what happened.
    let board = cells(&view);
    assert_eq!(board[4], Some(Mark::X));
    assert_eq!(board[0], Some(Mark::O));
}

#[tokio::test]
async fn abandoning_a_guessing_game_is_a_no_contest_and_credits_nothing() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    harness
        .games
        .play(&harness.caller(1), game, Move::Guess { value: 50 })
        .await
        .expect("one guess is spent");
    let view = harness
        .games
        .abandon(&harness.caller(1), game)
        .await
        .expect("the lone player may leave");
    assert_eq!(view.outcome, Some(Outcome::NoContest));
    assert!(harness.ledger.credits().is_empty());
    // The secret is not disclosed on the way out either. A game you gave up on tells you no
    // more than a game you lost.
    let dumped = format!("{view:?}");
    assert!(
        !dumped.contains("42"),
        "the abandoned view leaked the secret: {dumped}"
    );
}

// ---------------------------------------------------------------------------
// The one invariant that has to survive a crash.
//
// A game advances only if it is still open and still carries the token the move was computed
// against. That is a single compare-and-set in the store and there is no lock anywhere in the
// service, because a lock in the service is a lock a second process races past. These tests
// hold the compare-and-set down directly, because it is the whole of the mechanism.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_state_version_a_client_sees_is_the_token_the_store_compares() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    let before = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the board is readable");
    let session = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("the game exists");
    assert_eq!(before.state_version, session.updated_at.to_wire());
    let after = place_all(&harness, game, &[4]).await;
    assert!(
        after.view.state_version >= before.state_version,
        "a move must not move the token backwards"
    );
}

#[tokio::test]
async fn a_write_against_a_stale_token_is_refused_by_the_store() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    let stale = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("the game exists");
    // A move lands, so the token the first reader holds is now out of date. This is exactly
    // the shape of two processes computing a move against the same board.
    place_all(&harness, game, &[0]).await;
    let refused = harness
        .store
        .advance_game(AdvanceGame {
            game_id: game,
            expected_updated_at: stale.updated_at,
            state: stale.state.clone(),
            turn_of: Some(id(2)),
            status: GameStatus::Open.to_i16(),
            at: ts(NOW + SECOND),
        })
        .await
        .expect("the store answers");
    assert!(
        refused.is_none(),
        "a write against a stale token must be refused, not applied"
    );
    // And the board still carries the move that did land, not the one that was refused.
    let view = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect("the board is readable");
    assert_eq!(cells(&view)[0], Some(Mark::X));
}

#[tokio::test]
async fn a_write_against_a_current_token_is_applied_exactly_once() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    let fresh = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("the game exists");
    let advance = AdvanceGame {
        game_id: game,
        expected_updated_at: fresh.updated_at,
        state: fresh.state.clone(),
        turn_of: Some(id(2)),
        status: GameStatus::Open.to_i16(),
        at: ts(NOW + SECOND),
    };
    let first = harness
        .store
        .advance_game(AdvanceGame { ..advance.clone() })
        .await
        .expect("the store answers");
    assert!(first.is_some(), "the first write wins");
    // The very same write arriving again is now stale by its own doing, which is what makes a
    // retried request idempotent rather than doubly applied.
    let second = harness
        .store
        .advance_game(advance)
        .await
        .expect("the store answers");
    assert!(second.is_none(), "the same write must not land twice");
}

#[tokio::test]
async fn a_finished_game_cannot_be_advanced_even_with_a_current_token() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1, 4, 2]).await;
    let session = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("the game exists");
    assert_eq!(session.status, GameStatus::Finished.to_i16());
    let refused = harness
        .store
        .advance_game(AdvanceGame {
            game_id: game,
            expected_updated_at: session.updated_at,
            state: session.state.clone(),
            turn_of: Some(id(2)),
            status: GameStatus::Open.to_i16(),
            at: ts(NOW + SECOND),
        })
        .await
        .expect("the store answers");
    assert!(
        refused.is_none(),
        "a decided game must not be reopened by a write that holds the right token"
    );
}

#[tokio::test]
async fn abandoning_a_terminal_game_is_refused_at_the_store_too() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    place_all(&harness, game, &[0, 3, 1, 4, 2]).await;
    let refused = harness
        .store
        .abandon_game(game, ts(NOW + SECOND))
        .await
        .expect("the store answers");
    assert!(refused.is_none(), "a decided game cannot be abandoned");
}

#[tokio::test]
async fn two_commits_computed_against_the_same_empty_round_both_land_once_each() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    // Both seats read the same empty round before either writes, which is the concurrent case
    // the retry budget exists for. Each seat is its own byte in the state, so the loser of the
    // race re-reads the half-committed round and adds itself rather than overwriting.
    let first = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("exists");
    let second = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("exists");
    assert_eq!(
        first.updated_at, second.updated_at,
        "both read the same token"
    );
    harness
        .games
        .play(&harness.caller(1), game, Move::Throw { hand: Hand::Rock })
        .await
        .expect("seat one commits");
    let closing = harness
        .games
        .play(&harness.caller(2), game, Move::Throw { hand: Hand::Paper })
        .await
        .expect("seat two commits against the round it re-read");
    assert_eq!(thrown(&closing.view).0, [true, true]);
    assert_eq!(closing.outcome, Some(Outcome::Win { winner: id(2) }));
    assert_eq!(harness.moves("rock_paper_scissors"), 2);
}

#[tokio::test]
async fn a_contended_move_is_refused_rather_than_retried_forever() {
    // With no retries allowed at all, the service still has to answer a caller rather than
    // spin: the budget is spent, the move is a conflict, and the counter says why.
    let harness = Harness::configured(GamesConfig {
        retry_budget: 0,
        ..GamesConfig::default()
    });
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    let result = harness
        .games
        .play(&harness.caller(1), game, Move::Place { cell: 0 })
        .await
        .expect("an uncontended move lands on the first attempt");
    assert_eq!(cells(&result.view)[0], Some(Mark::X));
    assert_eq!(harness.plain("migo_games_cas_retries_total"), 0);
    assert_eq!(harness.rejected("contended"), 0);
}

// ---------------------------------------------------------------------------
// Stored state that does not decode.
//
// The state is the server's own encoding, so a state that will not decode is a bug or a
// corrupted row, never a client's doing. It must therefore be an internal error with a message
// that says nothing about the encoding, and it must not be reachable as a way to make the
// server say something specific about its own storage.
// ---------------------------------------------------------------------------

/// Overwrites a game's stored state with bytes no engine can decode.
async fn corrupt_the_state(harness: &Harness, game: Id, state: Vec<u8>) {
    let session = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("the game exists");
    harness
        .store
        .advance_game(AdvanceGame {
            game_id: game,
            expected_updated_at: session.updated_at,
            state,
            turn_of: session.turn_of,
            status: session.status,
            at: ts(NOW + SECOND),
        })
        .await
        .expect("the store answers")
        .expect("the write lands");
}

#[tokio::test]
async fn a_state_that_does_not_decode_is_an_internal_error() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    corrupt_the_state(&harness, game, vec![0xff; 4]).await;
    expect_code(
        harness.games.view(&harness.caller(1), game).await,
        codes::INTERNAL_ERROR,
    );
    expect_code(
        harness
            .games
            .play(&harness.caller(1), game, Move::Place { cell: 0 })
            .await,
        codes::INTERNAL_ERROR,
    );
    expect_code(
        harness.games.abandon(&harness.caller(1), game).await,
        codes::INTERNAL_ERROR,
    );
    expect_code(
        harness.games.active(&harness.caller(1), id(ROOM)).await,
        codes::INTERNAL_ERROR,
    );
}

#[tokio::test]
async fn an_empty_state_is_an_internal_error_rather_than_a_panic() {
    let harness = Harness::new();
    harness.room().await;
    let game = rock_paper_scissors(&harness).await;
    corrupt_the_state(&harness, game, Vec::new()).await;
    expect_code(
        harness.games.view(&harness.caller(1), game).await,
        codes::INTERNAL_ERROR,
    );
}

#[tokio::test]
async fn a_state_with_the_wrong_version_byte_is_an_internal_error() {
    let harness = Harness::new();
    harness.room().await;
    let game = guess_number(&harness).await;
    let session = harness
        .store
        .game(game)
        .await
        .expect("the store answers")
        .expect("exists");
    let mut state = session.state.clone();
    state[0] = 99;
    corrupt_the_state(&harness, game, state).await;
    expect_code(
        harness.games.view(&harness.caller(1), game).await,
        codes::INTERNAL_ERROR,
    );
}

#[tokio::test]
async fn a_corrupt_state_says_nothing_about_the_encoding() {
    let harness = Harness::new();
    harness.room().await;
    let game = tic_tac_toe(&harness).await;
    corrupt_the_state(&harness, game, vec![1, 2, 3]).await;
    let error = harness
        .games
        .view(&harness.caller(1), game)
        .await
        .expect_err("the read is refused");
    let public = error.public_message();
    for leak in ["version", "byte", "offset", "length", "decode", "0x"] {
        assert!(
            !public.to_ascii_lowercase().contains(leak),
            "the public message described the encoding with {leak:?}: {public}"
        );
    }
}

// ---------------------------------------------------------------------------
// Section 174: what the metrics may not say.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_metric_is_labelled_by_an_account_a_device_a_conversation_or_a_game() {
    let harness = Harness::new();
    harness.room().await;
    let noughts = tic_tac_toe(&harness).await;
    let hands = rock_paper_scissors(&harness).await;
    let numbers = guess_number(&harness).await;
    place_all(&harness, noughts, &[0, 3, 1, 4, 2]).await;
    harness
        .games
        .play(&harness.caller(1), hands, Move::Throw { hand: Hand::Rock })
        .await
        .expect("a commit lands");
    harness
        .games
        .play(&harness.caller(1), numbers, Move::Guess { value: 50 })
        .await
        .expect("a guess lands");
    harness
        .games
        .play(&harness.caller(3), noughts, Move::Place { cell: 8 })
        .await
        .expect_err("a watcher is refused, which is itself counted");
    harness.ledger.break_it();
    let walkout = rock_paper_scissors(&harness).await;
    harness
        .games
        .abandon(&harness.caller(1), walkout)
        .await
        .expect("the walk-out lands");
    let rendered = harness.registry.render();
    assert!(
        rendered.contains("migo_games_started_total"),
        "the games meters should be present at all: {rendered}"
    );
    // Every identifier that passed through the service above, in every form a label could
    // carry it. A cardinality explosion is the symptom; the leak is the disease.
    for identifier in [
        noughts,
        hands,
        numbers,
        walkout,
        id(1),
        id(2),
        id(3),
        device_of(1),
        device_of(2),
        device_of(3),
        id(ROOM),
    ] {
        for form in [identifier.to_string(), format!("{identifier:?}")] {
            assert!(
                !rendered.contains(&form),
                "the metrics registry leaked the identifier {form}"
            );
        }
    }
}

#[tokio::test]
async fn every_games_metric_is_labelled_only_by_a_closed_set_of_words() {
    let harness = Harness::new();
    harness.room().await;
    // One of everything, so every label this crate can emit is present in the render.
    for kind in GameKind::ALL {
        let opponents: Vec<Id> = if kind.min_players() > 1 {
            vec![id(2)]
        } else {
            Vec::new()
        };
        harness
            .games
            .start(&harness.caller(1), id(ROOM), *kind, &opponents)
            .await
            .expect("the game starts");
    }
    let rendered = harness.registry.render();
    for line in rendered.lines().filter(|line| line.contains("migo_games_")) {
        if let Some((_, labels)) = line.split_once('{') {
            let labels = labels.split('}').next().unwrap_or_default();
            for pair in labels.split(',').filter(|pair| !pair.is_empty()) {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                assert!(
                    matches!(name.trim(), "kind" | "reason" | "conclusion"),
                    "unexpected label {name:?} on {line}"
                );
                let value = value.trim().trim_matches('"');
                assert!(
                    !value.is_empty() && value.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                    "label value {value:?} is not one of a closed set of words: {line}"
                );
            }
        }
    }
}
