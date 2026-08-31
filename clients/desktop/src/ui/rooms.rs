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

/// The place's state: the directory page, the query that produced it, and the joined set.
#[derive(Debug, Default)]
pub struct RoomsState {
    /// The held page; empty before the first read lands.
    pub rooms: Vec<RoomRow>,
    /// The query text, submitted on Enter or the Search button.
    pub query: String,
    /// True once a read has landed, so an empty list can say "none" rather than "loading".
    pub loaded: bool,
    /// Room id → conversation id for every room this session entered (joined or created). The
    /// rows offer Open and Leave through it; the app fills it from the join events.
    pub joined: std::collections::HashMap<migo_core::Id, migo_core::Id>,
    /// The Create Room form, when it is open.
    pub creating: Option<CreateRoomForm>,
}

/// The Create Room form's fields, held across frames while the window is open.
#[derive(Debug, Default)]
pub struct CreateRoomForm {
    pub slug: String,
    pub name: String,
    pub managed: bool,
    pub topic: String,
    /// True once the user has typed in the slug field; the name stops suggesting past it.
    pub slug_touched: bool,
}

/// Draws the Rooms place.
pub fn show(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut RoomsState,
    chat: &mut crate::ui::chat::ChatState,
) {
    let colors = palette(context.theme);
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
            if ui.button("+ New room").clicked() {
                state.creating = Some(CreateRoomForm::default());
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

    let rows: Vec<crate::model::RoomRow> = state.rooms.clone();
    let joined = state.joined.clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(space::XS);
            for room in &rows {
                room_row(ui, context, chat, room, joined.get(&room.room_id).copied());
            }
            ui.add_space(space::XL);
        });

    // The Create Room form, as a centred window while it is open.
    if let Some(form) = state.creating.as_mut() {
        let mut submitted = false;
        let mut close = false;
        egui::Window::new("Create a room")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    if ui.selectable_label(!form.managed, "Public").clicked() {
                        form.managed = false;
                    }
                    if ui.selectable_label(form.managed, "Managed").clicked() {
                        form.managed = true;
                    }
                });
                ui.label(
                    RichText::new(if form.managed {
                        "A room under server moderation."
                    } else {
                        "A community room — anyone can join."
                    })
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
                );
                ui.add_space(space::SM);
                widgets::field(
                    ui,
                    context.theme,
                    "Name",
                    &mut form.name,
                    false,
                    "Late night talks",
                );
                // The slug follows the name until it is edited: the suggestion is the common
                // case, and the address is permanent in a way the name is not.
                if !form.slug_touched && !form.name.is_empty() {
                    form.slug = slug_suggestion(&form.name);
                }
                let slug_response = widgets::field(
                    ui,
                    context.theme,
                    "Address (permanent)",
                    &mut form.slug,
                    false,
                    "late-night-talks",
                );
                if slug_response.changed() {
                    form.slug = form.slug.to_lowercase();
                    form.slug_touched = true;
                }
                widgets::field(
                    ui,
                    context.theme,
                    "Topic (optional)",
                    &mut form.topic,
                    false,
                    "",
                );
                let slug_ok = form.slug.trim().len() > 1
                    && form
                        .slug
                        .trim()
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    && form
                        .slug
                        .trim()
                        .starts_with(|c: char| c.is_ascii_alphanumeric());
                if ui
                    .add_enabled(
                        !form.name.trim().is_empty() && slug_ok,
                        egui::Button::new("Create room"),
                    )
                    .clicked()
                {
                    submitted = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        if submitted {
            if let Some(form) = state.creating.take() {
                let topic = form.topic.trim().to_owned();
                context.issue(Command::CreateRoom {
                    slug: form.slug.trim().to_owned(),
                    name: form.name.trim().to_owned(),
                    managed: form.managed,
                    topic: (!topic.is_empty()).then_some(topic),
                });
            }
        } else if close {
            state.creating = None;
        }
    }
}

/// The slug a name suggests: lowercase, spaces to hyphens, everything else stripped.
///
/// Pure, so the suggestion is testable; a suggestion is only ever a starting point — the field
/// stays editable, because the name can change and the slug cannot.
pub fn slug_suggestion(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_dash = false;
    for character in name.trim().chars() {
        if character.is_ascii_alphanumeric() {
            out.extend(character.to_lowercase());
            pending_dash = true;
        } else if pending_dash && (character.is_whitespace() || character == '-') {
            out.push('-');
            pending_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// One directory row: the facts of a join decision, and the way in.
///
/// A room this session has entered offers Open and Leave instead of Join — the second join would
/// be a round trip to learn what the shell already knows. The whole row joins on click; the
/// buttons exist for discoverability and for the two actions a click cannot mean.
fn room_row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    chat: &mut crate::ui::chat::ChatState,
    room: &RoomRow,
    joined_conversation: Option<migo_core::Id>,
) {
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
    let mut open = false;
    let mut leave = false;
    inner.with_layout(
        Layout::right_to_left(Align::Center),
        |ui| match joined_conversation {
            Some(conversation_id) => {
                if ui.button("Leave").clicked() {
                    leave = true;
                }
                if ui
                    .add(egui::Button::new(
                        RichText::new("Open")
                            .font(FontId::proportional(font::BODY))
                            .color(colors.text_on_accent),
                    ))
                    .clicked()
                {
                    open = true;
                }
                let _ = conversation_id;
            }
            None => {
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
            }
        },
    );

    if join || response.clicked() {
        context.issue(Command::JoinRoom {
            room_id: room.room_id,
        });
    }
    if open {
        if let Some(conversation_id) = joined_conversation {
            crate::ui::chat::open(context, chat, conversation_id);
            context.go_place(crate::ui::Place::Chat);
        }
    }
    if leave {
        context.issue(Command::LeaveRoom {
            room_id: room.room_id,
        });
    }
}
