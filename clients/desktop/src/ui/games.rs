//! The Games window: the catalogue the server referees, and the live stream of the games the
//! watched conversations play.
//!
//! The reference's Games tab is an arcade with a dice table; this build's wire honestly offers
//! something narrower. Games are room-scoped and server-authoritative: they are *started inside
//! a conversation*, and this desktop client's wire has no game move of its own — no start, no
//! play — so the window lists the catalogue and carries the feed of published deltas instead:
//! every `GAME_EVENT` the conversations this client watches produce is appended as a line the
//! moment the referee publishes it, from another member's start to the finish an abandon is.
//! The list is the games crate's own fixed numbering, the same three the web client's launcher
//! offers, so the window never names a game the server cannot referee.
//!
//! The feed is arrival-ordered and capped; a game's own order is its `state_version`, which each
//! row carries, and a move's substance is a GAME_VIEW ask this pane deliberately does not make —
//! the feed says that the game moved, never what the board now holds.

use egui::{RichText, Ui};

use migo_core::{Id, Timestamp};

use crate::theme::{font, palette, space};
use crate::ui::widgets;
use crate::ui::Context;

/// How many feed rows to keep; the stream is unbounded, the pane is not.
const MAX_ROWS: usize = 60;

/// One game the server referees, as the tab shows it.
struct CatalogueEntry {
    name: &'static str,
    players: &'static str,
}

/// The catalogue this build's server can referee (the games crate fixes the kinds in code).
const CATALOGUE: [CatalogueEntry; 3] = [
    CatalogueEntry {
        name: "Tic-tac-toe",
        players: "2 players",
    },
    CatalogueEntry {
        name: "Rock paper scissors",
        players: "2 players",
    },
    CatalogueEntry {
        name: "Guess the number",
        players: "1 player",
    },
];

/// The place's state.
#[derive(Debug, Default)]
pub struct GamesState {
    /// The live feed: published game events for conversations this client watches, newest last.
    pub rows: Vec<GameRow>,
}

impl GamesState {
    /// Appends a published delta, dropping a redelivery and trimming to the newest.
    ///
    /// The row's `key` deduplicates: the transport resuming its queue may redeliver an event, and
    /// it is built from everything the wire puts on the line, so two rows that agree on it are
    /// the same event heard twice.
    pub fn push(&mut self, row: GameRow) {
        if self.rows.iter().any(|existing| existing.key == row.key) {
            return;
        }
        self.rows.push(row);
        let excess = self.rows.len().saturating_sub(MAX_ROWS);
        self.rows.drain(0..excess);
    }
}

/// One line of the live feed: a published game event, as the wire put it.
#[derive(Debug, Clone, PartialEq)]
pub struct GameRow {
    /// The deduplication key, built from everything the wire puts on the line.
    pub key: String,
    /// The conversation the game played in (the wire's `room_id`, which names this conversation).
    pub conversation_id: Id,
    pub game_id: Id,
    /// The event's name: `started`, `moved`, `turn_changed`, `finished`.
    pub event: String,
    /// Who the event is about: the mover, the player whose turn it now is, the winner.
    pub actor_id: Option<Id>,
    /// The board version every event of one move shares.
    pub state_version: u64,
    /// Arrival time, for display only — the wire carries no game clock a client may quote.
    pub at: Timestamp,
}

/// Draws the Games place.
pub fn show(ui: &mut Ui, context: &Context<'_>, state: &mut GamesState) {
    let colors = palette(context.theme);
    ui.add_space(space::LG);
    ui.horizontal(|ui| {
        ui.add_space(space::LG);
        widgets::header(
            ui,
            context.theme,
            "Games",
            Some("Refereed by the server, played inside a conversation."),
        );
    });
    ui.add_space(space::LG);

    ui.horizontal(|ui| {
        ui.add_space(space::LG);
        ui.allocate_ui_with_layout(
            egui::vec2(
                (ui.available_width() - space::LG * 2.0).max(200.0),
                ui.available_height(),
            ),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                for entry in CATALOGUE {
                    card(ui, context, entry);
                    ui.add_space(space::SM);
                }
                ui.add_space(space::MD);
                ui.label(
                    RichText::new(
                        "Games start inside a conversation and play out in its thread. This \
                         desktop build's wire carries no game move of its own, so there is \
                         nothing to start here — but every game the conversations you watch \
                         play is published as it moves, and the feed below is that stream, \
                         live.",
                    )
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
                );
                ui.add_space(space::MD);
                feed(ui, context, state);
            },
        );
    });
}

/// Draws the live feed: the published deltas this session has heard, newest last.
fn feed(ui: &mut Ui, context: &Context<'_>, state: &mut GamesState) {
    let colors = palette(context.theme);
    ui.label(
        RichText::new("Live activity")
            .font(egui::FontId::proportional(font::BODY))
            .color(colors.text)
            .strong(),
    );
    ui.add_space(space::XS);
    if state.rows.is_empty() {
        ui.label(
            RichText::new(
                "Nothing yet. A game started or moved in a conversation you have open will \
                 land here the moment the server publishes it.",
            )
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(240.0)
        .show(ui, |ui| {
            ui.add_space(space::XS);
            for row in &state.rows {
                feed_row(ui, context.theme, row);
            }
            ui.add_space(space::SM);
        });
}

/// One feed row: the event in plain words, the game it moved, and the time it arrived.
fn feed_row(ui: &mut Ui, theme: crate::theme::Theme, row: &GameRow) {
    let colors = palette(theme);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::hover());
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(space::SM, space::XS * 0.25)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    inner.label(
        RichText::new(format!(
            "{} · game {}{}",
            crate::model::spaced_words(&row.event),
            crate::model::short_id(row.game_id),
            row.actor_id
                .map(|actor| format!(" · by {}", crate::model::short_id(actor)))
                .unwrap_or_default(),
        ))
        .font(egui::FontId::proportional(font::SMALL))
        .color(colors.text),
    );
    inner.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(
            RichText::new(crate::model::clock(row.at))
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
    });
}

/// One catalogue card: the game's name and the player range the server allows.
fn card(ui: &mut Ui, context: &Context<'_>, entry: CatalogueEntry) {
    let colors = palette(context.theme);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .stroke(egui::Stroke::new(1.0, colors.border))
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::TAB))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(entry.name)
                    .font(egui::FontId::proportional(font::SUBTITLE))
                    .color(colors.text)
                    .strong(),
            );
            ui.add_space(space::XS);
            ui.label(
                RichText::new(entry.players)
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        });
}
