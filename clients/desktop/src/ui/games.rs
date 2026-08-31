//! The Games tab: the catalogue the server referees, as a destination of its own.
//!
//! The reference's Games tab is an arcade with a dice table; this build's wire honestly offers
//! something narrower. Games are room-scoped and server-authoritative: they are *started inside
//! a conversation*, and the desktop client carries no board of its own — it lists the catalogue
//! and says plainly where the play happens. The list is the games crate's own fixed numbering,
//! the same three the web client's launcher offers, so the tab never names a game the server
//! cannot referee.

use egui::{RichText, Ui};

use crate::theme::{font, palette, space};
use crate::ui::widgets;
use crate::ui::Context;

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

/// Draws the Games place.
pub fn show(ui: &mut Ui, context: &Context<'_>) {
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
                        "Open a conversation from the Chats tab and start one from its header — \
                         the game plays out in the thread, and this desktop build lists while the \
                         web client plays.",
                    )
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
                );
            },
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
