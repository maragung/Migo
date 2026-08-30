//! The GAMES application opcodes: starting, watching, and leaving a game, and the
//! catalogue to build a menu from.
//!
//! Four opcodes, each a thin translation from a wire frame onto one
//! [`Referee`](migo_games::traits::Referee) method. The service owns every rule —
//! conversation membership, the engine's legality checks, the compare-and-set that
//! beats replays, the rate charge — so these handlers only build the
//! [`Caller`](migo_games::Caller), decode the body with [`from_frame`], await the
//! service, and [`reply`](ClientContext::reply). `GAME_ACTION` (176) stays inline in
//! `dispatch.rs`, where its move translation and event publication live; these four
//! are the ones whose whole job is the projection onto [`GameViewWire`].
//!
//! # Opcode → method map
//!
//! | Opcode          | Wire payload      | Service method       | Response               |
//! |-----------------|-------------------|----------------------|------------------------|
//! | `GAME_START`    | `GameStart`       | `Referee::start`     | `GameViewWire`         |
//! | `GAME_VIEW`     | `GameId`          | `Referee::view`      | `GameViewWire`         |
//! | `GAME_ABANDON`  | `GameId`          | `Referee::abandon`   | `Acknowledged`         |
//! | `GAME_CATALOGUE`| `GiftCatalogueReq`| `Referee::catalogue` | `GameCatalogueResponse`|
//!
//! `GAME_CATALOGUE` reuses the empty `GiftCatalogueReq` body the registry gave it; an
//! empty request is an empty request, and the IDL froze before a second one was worth
//! its own struct.
//!
//! # Why start and abandon publish nothing
//!
//! `start` and `abandon` return a view and no deltas, and the house rule is that the
//! return type decides: a payload is answered, a fanout is published, and a method
//! that returns only the payload has said the conversation hears nothing. A player
//! who wants the fresh state has it in the reply; a spectator asks `GAME_VIEW`. That
//! is the service's decision, not this module's, and the one place a frame is
//! published for a game remains `GAME_ACTION`, whose service method does return
//! deltas.

use migo_core::Error;
use migo_games::model::Feedback;
use migo_games::{Caller as GameCaller, GameKind, GameView, Hand, Mark, Render, SharedReferee};
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, Acknowledged, Frame, GameCatalogueEntry, GameCatalogueResponse, GameId,
    GameStart, GameViewWire, GiftCatalogueReq,
};

/// Starts a game in a conversation and replies with the opening view.
///
/// The wire names the game by slug and the domain by enum, so the handler owns one
/// closed mapping — three names, nothing optional, the same narrowness the
/// `GAME_ACTION` move translation holds, because every string accepted here is a
/// string every client must produce identically. An unknown slug is the client's fault
/// (`VALIDATION_FAILED`), never a guess.
///
/// The wire names no opponents, so none are passed: `start` receives an empty list and
/// the service's own player-count rule does the rest. In this build that means
/// `GAME_START` can open the single-player guessing game and nothing else — a
/// two-player kind is refused with "wrong number of players", which is the honest
/// answer for a request that cannot say who the other player is. Inventing an opponent
/// here (the other member, the first member) would be this module deciding who gets
/// pulled into a game, which is a rule, and rules are the service's.
pub(crate) async fn handle_game_start(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedReferee,
) -> Result<(), Error> {
    let caller = GameCaller {
        account_id: ctx.identity().account_id(),
        device_id: ctx.identity().device_id(),
        tier: ctx.identity().tier,
        now: ctx.now(),
        request_id: None,
    };
    let request: GameStart = from_frame(frame).map_err(fault::from_wire)?;
    let kind = kind_of_slug(&request.slug)?;
    let view = svc
        .start(&caller, request.conversation_id, kind, &[])
        .await?;
    ctx.reply(&wire_view(&view))
}

/// Reads one game as the caller is allowed to see it and replies with the view.
///
/// The service redacts per viewer and answers `NOT_FOUND` for a game in a conversation
/// the caller cannot see (section 48); the handler projects whatever survived.
pub(crate) async fn handle_game_view(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedReferee,
) -> Result<(), Error> {
    let caller = GameCaller {
        account_id: ctx.identity().account_id(),
        device_id: ctx.identity().device_id(),
        tier: ctx.identity().tier,
        now: ctx.now(),
        request_id: None,
    };
    let request: GameId = from_frame(frame).map_err(fault::from_wire)?;
    let view = svc.view(&caller, request.game_id).await?;
    ctx.reply(&wire_view(&view))
}

/// Abandons an open game the caller is playing and acknowledges.
///
/// The service ends the game with no winner and no reward — a forfeit pays nobody, so
/// that abandoning cannot be farmed — and returns the final view, which the registry's
/// `Acknowledged` answer leaves nowhere to go. The caller knows what it asked for; the
/// other player learns the game is over by the next `GAME_VIEW`, and publishing a
/// frame here would be this module overruling a service method that returned no deltas.
pub(crate) async fn handle_game_abandon(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedReferee,
) -> Result<(), Error> {
    let caller = GameCaller {
        account_id: ctx.identity().account_id(),
        device_id: ctx.identity().device_id(),
        tier: ctx.identity().tier,
        now: ctx.now(),
        request_id: None,
    };
    let request: GameId = from_frame(frame).map_err(fault::from_wire)?;
    let _view = svc.abandon(&caller, request.game_id).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Lists the games this node can play.
///
/// `catalogue` is synchronous and uncharged — the list is fixed in code, not a store
/// read — so this is the one handler here with nothing to await. Each entry carries
/// the slug `GAME_START` accepts and the player counts a client needs to know whether
/// a game is even startable through this build's wire.
pub(crate) async fn handle_game_catalogue(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedReferee,
) -> Result<(), Error> {
    let _request: GiftCatalogueReq = from_frame(frame).map_err(fault::from_wire)?;
    let games = svc
        .catalogue()
        .into_iter()
        .map(|info| GameCatalogueEntry {
            slug: info.slug.to_string(),
            // The widening casts are safe by construction: the kinds and player counts
            // are small non-negative constants this crate fixed at compile time.
            kind: info.kind.to_i16() as u32,
            min_players: u32::from(info.min_players),
            max_players: u32::from(info.max_players),
        })
        .collect();
    ctx.reply(&GameCatalogueResponse { games })
}

/// The [`GameKind`] a catalogue slug names.
///
/// The slug is the kind's stable machine name — the same string the catalogue lists
/// and the metric label uses — so the mapping is total over exactly those names and
/// refuses everything else, for the same reason the move translation does: a
/// permissive parser would make "which spellings work" a property of this function
/// rather than of the protocol.
fn kind_of_slug(slug: &str) -> Result<GameKind, Error> {
    match slug {
        "tic_tac_toe" => Ok(GameKind::TicTacToe),
        "rock_paper_scissors" => Ok(GameKind::RockPaperScissors),
        "guess_number" => Ok(GameKind::GuessNumber),
        _ => Err(fault::validation("slug", "unknown game")),
    }
}

/// Projects a domain [`GameView`] onto the wire struct.
///
/// `kind` and `status` carry the same integers the store persists, because the IDL
/// defines no enums for them and inventing a second numbering here would give one game
/// two identities on the wire. `your_turn` is always present — the server computed it,
/// so absent would be the one thing it is not, a server that could not say — and
/// `turn_of` maps straight through, `None` for a finished game or one awaiting
/// simultaneous commits.
///
/// The board string's grammar is per kind and documented on [`wire_board`].
fn wire_view(view: &GameView) -> GameViewWire {
    GameViewWire {
        game_id: view.game_id,
        // Widening casts of the kind and status discriminants — 0..=2 by construction,
        // never negative — so the cast cannot wrap.
        kind: view.kind.to_i16() as u32,
        conversation_id: view.conversation_id,
        status: view.status.to_i16() as u32,
        players: view.players.clone(),
        state_version: view.state_version,
        board: wire_board(&view.render),
        turn_of: view.turn_of,
        your_turn: Some(view.your_turn),
    }
}

/// Renders the redacted [`Render`] as the compact text line the wire's `board` field
/// carries.
///
/// The IDL gives `board` one string for three boards, and this build has no serde on
/// this path, so each kind renders to a short line a client parses per kind:
///
/// * **tic-tac-toe** — exactly nine characters, `X`, `O`, or `.` for an empty cell,
///   row-major.
/// * **rock-paper-scissors** — `<seat0>-vs-<seat1>`, each seat being `waiting` (nothing
///   committed), `committed` (locked in, contents hidden), or the hand itself once
///   both are down and the engine has revealed them. The seat that has committed is
///   fair to show; the hand inside it is not, and the render type has no field for it.
/// * **guess-the-number** — `<low>-<high>:<remaining>` followed by one ` <guess>:<feedback>`
///   per guess, the feedback being `lower`, `higher`, or `correct`. The hidden number
///   appears in no part of the line, because it appears in no field of the render.
///
/// Nothing in any line is secret: the render type cannot carry what the viewer may not
/// see, so neither can the string built from it.
fn wire_board(render: &Render) -> String {
    match render {
        Render::TicTacToe { cells } => cells
            .iter()
            .map(|cell| match cell {
                Some(Mark::X) => 'X',
                Some(Mark::O) => 'O',
                None => '.',
            })
            .collect(),
        Render::RockPaperScissors { committed, reveal } => {
            let seat = |index: usize, hand: Option<Hand>| match hand {
                Some(hand) => hand_word(hand).to_string(),
                None if committed[index] => "committed".to_string(),
                None => "waiting".to_string(),
            };
            let (first, second) = match reveal {
                Some(hands) => (Some(hands[0]), Some(hands[1])),
                None => (None, None),
            };
            format!("{}-vs-{}", seat(0, first), seat(1, second))
        }
        Render::GuessNumber {
            low,
            high,
            remaining,
            guesses,
        } => {
            let mut out = format!("{low}-{high}:{remaining}");
            for guess in guesses {
                out.push_str(&format!(
                    " {}:{}",
                    guess.value,
                    match guess.feedback {
                        Feedback::Lower => "lower",
                        Feedback::Higher => "higher",
                        Feedback::Correct => "correct",
                    }
                ));
            }
            out
        }
    }
}

/// The word for a revealed hand. One place, so the reveal and the catalogue agree on
/// spelling.
fn hand_word(hand: Hand) -> &'static str {
    match hand {
        Hand::Rock => "rock",
        Hand::Paper => "paper",
        Hand::Scissors => "scissors",
    }
}
