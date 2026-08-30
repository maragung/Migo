//! The Rooms place: the public directory and the way in.
//!
//! Rows state the facts a join decision needs — name, topic, members, live online count — and
//! nothing else. The query is submitted, not per keystroke: every character would be a round trip
//! against a rate-limited endpoint, and the user who typed three letters has not yet asked a
//! question.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::model::RoomRow;
use crate::net::Command;
use crate::theme::{font, palette, space};
use crate::ui::{widgets, Context};

/// The place's state: the directory page and the query that produced it.
#[derive(Debug, Default)]
pub struct RoomsState {
    /// The held page; empty before the first read lands.
    pub rooms: Vec<RoomRow>,
    /// The query text, submitted on Enter or the Search button.
    pub query: String,
    /// True once a read has landed, so an empty list can say "none" rather than "loading".
    pub loaded: bool,
}

/// Draws the Rooms place.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut RoomsState) {
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::header(
            ui,
            context.theme,
            "Rooms",
            Some("Public rooms on this server"),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui.button("Refresh").clicked() {
                context.issue(Command::Rooms {
                    query: state.query.clone(),
                });
            }
        });
    });
    ui.add_space(space::SM);

    // The query row: the field, then the submit.
    let mut submitted = false;
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.query)
                .hint_text("search rooms")
                .desired_width(ui.available_width() - space::XL * 2.5 - space::MD),
        );
        submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Search").clicked() {
            submitted = true;
        }
    });
    if submitted {
        context.issue(Command::Rooms {
            query: state.query.clone(),
        });
    }
    ui.add_space(space::SM);
    widgets::divider(ui, context.theme);

    if state.rooms.is_empty() {
        if state.loaded {
            widgets::empty_state(
                ui,
                context.theme,
                if state.query.trim().is_empty() {
                    "No public rooms yet"
                } else {
                    "No rooms matched"
                },
                "Rooms others open will appear here.",
            );
        } else {
            widgets::empty_state(ui, context.theme, "Loading rooms…", "");
        }
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(space::XS);
            for room in &state.rooms {
                room_row(ui, context, room);
            }
            ui.add_space(space::XL);
        });
}

/// One directory row: the facts of a join decision, and the way in.
///
/// The whole row is the click target and the Join button exists for discoverability; both queue
/// the same command, and the conversation the join reply names is opened by the event that
/// follows, not by this row.
fn room_row(ui: &mut Ui, context: &mut Context<'_>, room: &RoomRow) {
    let colors = palette(context.theme);
    let height = 58.0;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let background = if response.hovered() {
        colors.surface_hover
    } else {
        egui::Color32::TRANSPARENT
    };
    if background != egui::Color32::TRANSPARENT {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(crate::theme::radius::MD),
            background,
        );
    }

    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(space::MD, space::XS)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    widgets::avatar(&mut inner, context.theme, &room.name, 36.0);
    inner.add_space(space::SM);
    inner.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(widgets::elide(&room.name, 30))
                    .font(FontId::proportional(font::BODY))
                    .color(colors.text)
                    .strong(),
            );
            if room.verified {
                ui.label(
                    RichText::new("\u{2713}")
                        .font(FontId::proportional(font::TINY))
                        .color(colors.accent),
                )
                .on_hover_text("Verified room");
            }
        });
        let mut counts = format!(
            "{} members · {} online",
            room.member_count, room.online_count
        );
        if let Some(category) = &room.category {
            counts.push_str(&format!(" · {category}"));
        }
        ui.label(
            RichText::new(counts)
                .font(FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        if let Some(topic) = &room.topic {
            ui.label(
                RichText::new(widgets::elide(topic, 44))
                    .font(FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        }
    });

    let mut join = false;
    inner.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui
            .add(egui::Button::new(
                RichText::new("Join")
                    .font(FontId::proportional(font::BODY))
                    .color(colors.text_on_accent),
            ))
            .clicked()
        {
            join = true;
        }
    });

    if join || response.clicked() {
        context.issue(Command::JoinRoom {
            room_id: room.room_id,
        });
    }
}
