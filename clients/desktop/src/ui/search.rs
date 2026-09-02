//! The Search place: one box, everything it can honestly find.
//!
//! People and rooms answer on the wire (username prefixes and room names); conversations answer
//! locally, from the list the session already holds. The query is submitted — Enter or the button —
//! rather than per keystroke: every character would be a round trip against a rate-limited
//! endpoint, and the user who typed three letters has not yet asked a question.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::model::{PersonRow, RoomRow};
use crate::net::Command;
use crate::theme::{font, palette, space};
use crate::ui::chat::ChatState;
use crate::ui::{widgets, Context};

/// The place's state.
#[derive(Debug, Default)]
pub struct SearchState {
    /// The query text.
    pub query: String,
    /// The people answer, or `None` before the first query.
    pub people: Option<Vec<PersonRow>>,
    /// The rooms answer, or `None` before the first query.
    pub rooms: Option<Vec<RoomRow>>,
    /// The people answer from the graph's own suggestions, shown before the first query.
    pub suggestions: Vec<PersonRow>,
    /// True while a query is in flight.
    pub busy: bool,
}

/// Draws the Search place.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut SearchState, chat: &mut ChatState) {
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::header(
            ui,
            context.theme,
            "Search",
            Some("People, rooms, and your chats"),
        );
        if state.busy {
            ui.spinner();
        }
    });
    ui.add_space(space::SM);

    let mut submitted = false;
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.query)
                .hint_text("a username, a room name, a chat title")
                .desired_width(ui.available_width() - space::XL * 2.5 - space::MD),
        );
        submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Search").clicked() {
            submitted = true;
        }
    });
    if submitted {
        let query = state.query.trim().to_owned();
        if query.is_empty() {
            state.people = None;
            state.rooms = None;
        } else {
            state.busy = true;
            context.issue(Command::SearchPeople { query });
            context.issue(Command::Rooms {
                query: state.query.clone(),
            });
        }
    }
    ui.add_space(space::SM);
    widgets::divider(ui, context.theme);

    let query = state.query.trim();
    if query.is_empty() {
        // The pre-query state: the graph's own suggestions, offered as doors.
        if state.suggestions.is_empty() {
            widgets::empty_state(
                ui,
                context.theme,
                "Find people and rooms",
                "Search by username prefix or room name.",
            );
        } else {
            widgets::subheader(ui, context.theme, "PEOPLE TO MEET");
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for person in &state.suggestions {
                        person_row(ui, context, person);
                    }
                });
        }
        return;
    }

    // The local half: conversations whose title matches. Copied out rather than held as borrows,
    // because opening a hit needs the chat state mutably and the iterator's borrow would otherwise
    // outlive the click it exists to serve.
    let me = context
        .account
        .map(|account| account.account_id)
        .unwrap_or_default();
    let matches: Vec<(migo_core::Id, String, Option<String>, u32, bool)> = chat
        .conversations
        .iter()
        .filter(|conversation| {
            conversation
                .display_title(me, &chat.names)
                .to_lowercase()
                .contains(&query.to_lowercase())
        })
        .map(|conversation| {
            (
                conversation.conversation_id,
                conversation.display_title(me, &chat.names),
                conversation.preview.clone(),
                conversation.unread,
                conversation.encrypted,
            )
        })
        .collect();

    let nothing = matches.is_empty()
        && state.people.as_ref().is_none_or(Vec::is_empty)
        && state.rooms.as_ref().is_none_or(Vec::is_empty);

    if nothing {
        widgets::empty_state(
            ui,
            context.theme,
            &format!("Nothing found for \u{201C}{query}\u{201D}"),
            "Try a username, a room name, or a chat title.",
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if !matches.is_empty() {
                widgets::subheader(ui, context.theme, "CHATS");
                for (conversation_id, title, preview, unread, encrypted) in &matches {
                    if widgets::conversation_row(
                        ui,
                        context.theme,
                        widgets::RowContent {
                            title,
                            preview: preview.as_deref(),
                            time: None,
                            unread: *unread,
                            selected: false,
                            encrypted: *encrypted,
                        },
                    )
                    .clicked()
                    {
                        crate::ui::chat::open(context, chat, *conversation_id);
                    }
                }
                ui.add_space(space::SM);
            }

            let people = state.people.clone().unwrap_or_default();
            if !people.is_empty() {
                widgets::subheader(ui, context.theme, "PEOPLE");
                for person in &people {
                    person_row(ui, context, person);
                }
                ui.add_space(space::SM);
            }

            let rooms = state.rooms.clone().unwrap_or_default();
            if !rooms.is_empty() {
                widgets::subheader(ui, context.theme, "ROOMS");
                for room in &rooms {
                    if ui
                        .button(RichText::new(format!(
                            "{}   {} online",
                            room.name, room.online_count
                        )))
                        .clicked()
                    {
                        context.issue(Command::JoinRoom {
                            room_id: room.room_id,
                        });
                    }
                }
            }
            ui.add_space(space::XL);
        });
}

/// One person row: the name, the handle, and the two doors a stranger is offered.
fn person_row(ui: &mut Ui, context: &mut Context<'_>, person: &PersonRow) {
    let colors = palette(context.theme);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::avatar(ui, context.theme, &person.display_name, 32.0);
        ui.add_space(space::SM);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(&person.display_name)
                    .font(FontId::proportional(font::BODY))
                    .color(colors.text)
                    .strong(),
            );
            let mut handle = format!("@{}", person.username);
            if person.mutual_friends > 0 {
                handle.push_str(&format!(" · {} mutual", person.mutual_friends));
            }
            ui.label(
                RichText::new(handle)
                    .font(FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui.button("Message").clicked() {
                context.issue(Command::StartDirectById {
                    peer: person.account_id,
                });
            }
            if ui.button("Add").clicked() {
                context.issue(Command::AddFriend {
                    user_id: person.account_id.to_text(),
                });
            }
        });
    });
}
