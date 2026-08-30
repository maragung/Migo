//! The Home place: the realtime dashboard.
//!
//! Home is a glance, not a destination — every block states a fact the session already knows or
//! can read in one round trip, and every row is a door into the place that owns it. The blocks are
//! compact by contract: one row per fact, no card chrome around a single number.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::model::{AlertRow, LeaderRow, PersonRow, RoomRow};
use crate::net::Command;
use crate::theme::{font, palette, space, Theme};
use crate::ui::chat::ChatState;
use crate::ui::{widgets, Context, Place};

/// The dashboard's read-only view of everything the other places hold.
///
/// Home owns no state of its own: the conversations are the chat's, the rooms the directory's,
/// the people the search's, the wallet's facts the wallet's. A dashboard that kept its own copies
/// would be a second source of every truth on screen.
pub struct HomeData<'a> {
    pub rooms: &'a [RoomRow],
    pub people: &'a [PersonRow],
    pub alerts: &'a [AlertRow],
    pub leaders: &'a [LeaderRow],
    pub coins: Option<u64>,
}

/// Draws the Home place.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, chat: &mut ChatState, data: HomeData<'_>) {
    let colors = palette(context.theme);
    let me = context.account.map(|account| account.account_id);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The hero: who you are, what you have, and where you stand.
            ui.add_space(space::MD);
            ui.horizontal(|ui| {
                ui.add_space(space::MD);
                let name = context
                    .account
                    .map(|account| account.username.as_str())
                    .unwrap_or("You");
                widgets::avatar(ui, context.theme, name, 44.0);
                ui.add_space(space::SM);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(name)
                            .font(FontId::proportional(font::TITLE))
                            .color(colors.text)
                            .strong(),
                    );
                    ui.label(
                        RichText::new("Welcome back")
                            .font(FontId::proportional(font::SMALL))
                            .color(colors.text_muted),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(space::MD);
                    if let Some(coins) = data.coins {
                        widgets::pill(
                            ui,
                            &format!("MIG {coins}"),
                            colors.accent,
                            colors.surface_selected,
                        );
                    }
                });
            });

            // The quick actions: the three moves a session most often starts with.
            ui.add_space(space::MD);
            ui.horizontal(|ui| {
                ui.add_space(space::MD);
                if ui.button("Search").clicked() {
                    context.go_place(Place::Search);
                }
                if ui.button("Browse rooms").clicked() {
                    context.go_place(Place::Rooms);
                }
                if ui.button("Wallet").clicked() {
                    context.go_place(Place::Wallet);
                }
            });

            // Recent chats: the conversation list's own top. The rows are copied out before the
            // loop because opening one needs the chat state mutably, and the iterator's borrow
            // would otherwise outlive the click it exists to serve.
            if !chat.conversations.is_empty() {
                let recent: Vec<(migo_core::Id, String, Option<String>, u32, bool)> = chat
                    .conversations
                    .iter()
                    .take(4)
                    .map(|conversation| {
                        (
                            conversation.conversation_id,
                            conversation.display_title(me.unwrap_or_default(), &chat.names),
                            conversation.preview.clone(),
                            conversation.unread,
                            conversation.encrypted,
                        )
                    })
                    .collect();
                section(ui, context.theme, "RECENT CHATS", "All chats", || {
                    context.go_place(Place::Chat);
                });
                for (conversation_id, title, preview, unread, encrypted) in recent {
                    if widgets::conversation_row(
                        ui,
                        context.theme,
                        widgets::RowContent {
                            title: &title,
                            preview: preview.as_deref(),
                            time: None,
                            unread,
                            selected: false,
                            encrypted,
                        },
                    )
                    .clicked()
                    {
                        crate::ui::chat::open(context, chat, conversation_id);
                        context.go_place(Place::Chat);
                    }
                }
            }

            // Trending rooms: the directory's liveliest page, offered for a join.
            if !data.rooms.is_empty() {
                section(ui, context.theme, "TRENDING ROOMS", "All rooms", || {
                    context.go_place(Place::Rooms);
                });
                for room in data.rooms.iter().take(5) {
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD);
                        widgets::avatar(ui, context.theme, &room.name, 28.0);
                        ui.add_space(space::SM);
                        ui.label(
                            RichText::new(widgets::elide(&room.name, 28))
                                .font(FontId::proportional(font::BODY))
                                .color(colors.text),
                        );
                        ui.label(
                            RichText::new(format!("{} online", room.online_count))
                                .font(FontId::proportional(font::SMALL))
                                .color(colors.text_muted),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(space::MD);
                            if ui.button("Join").clicked() {
                                context.issue(Command::JoinRoom {
                                    room_id: room.room_id,
                                });
                            }
                        });
                    });
                }
            }

            // People to meet: the graph's own recommendations.
            if !data.people.is_empty() {
                section(ui, context.theme, "PEOPLE TO MEET", "Friends", || {
                    context.go_place(Place::Friends);
                });
                for person in data.people.iter().take(4) {
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD);
                        widgets::avatar(ui, context.theme, &person.display_name, 28.0);
                        ui.add_space(space::SM);
                        ui.label(
                            RichText::new(&person.display_name)
                                .font(FontId::proportional(font::BODY))
                                .color(colors.text),
                        );
                        ui.label(
                            RichText::new(format!("@{}", person.username))
                                .font(FontId::proportional(font::SMALL))
                                .color(colors.text_muted),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(space::MD);
                            if ui.button("Message").clicked() {
                                context.issue(Command::StartDirectById {
                                    peer: person.account_id,
                                });
                            }
                        });
                    });
                }
            }

            // The alerts digest.
            if !data.alerts.is_empty() {
                section(ui, context.theme, "ALERTS", "View all", || {
                    context.go_place(Place::Alerts);
                });
                for alert in data.alerts.iter().take(4) {
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD);
                        ui.label(
                            RichText::new(widgets::elide(
                                &alert
                                    .title
                                    .clone()
                                    .unwrap_or_else(|| crate::model::spaced_words(&alert.kind)),
                                56,
                            ))
                            .font(FontId::proportional(font::BODY))
                            .color(colors.text),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(space::MD);
                            ui.label(
                                RichText::new(crate::model::clock(alert.at))
                                    .font(FontId::proportional(font::TINY))
                                    .color(colors.text_muted),
                            );
                        });
                    });
                }
            }

            // The leaderboard's top three.
            if !data.leaders.is_empty() {
                section(ui, context.theme, "TOP XP", "Leaderboard", || {
                    context.go_place(Place::Wallet);
                });
                for leader in data.leaders.iter().take(3) {
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD);
                        ui.label(
                            RichText::new(format!("#{}", leader.position))
                                .font(FontId::proportional(font::SMALL))
                                .color(colors.text_muted),
                        );
                        ui.label(
                            RichText::new(format!("Level {} · {} XP", leader.level, leader.xp))
                                .font(FontId::proportional(font::SMALL))
                                .color(colors.text),
                        );
                    });
                }
            }

            ui.add_space(space::XL);
        });
}

/// One dashboard section heading with its one action.
///
/// The closure runs on the action's click; it exists because every section's action is "go to the
/// place that owns this", and the heading is where that offer belongs.
fn section(ui: &mut Ui, theme: Theme, label: &str, action: &str, on_action: impl FnOnce()) {
    let colors = palette(theme);
    ui.add_space(space::LG);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        ui.label(
            RichText::new(label)
                .font(FontId::proportional(font::TINY))
                .color(colors.text_muted)
                .strong(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(action)
                            .font(FontId::proportional(font::SMALL))
                            .color(colors.accent),
                    )
                    .fill(egui::Color32::TRANSPARENT),
                )
                .clicked()
            {
                on_action();
            }
        });
    });
    ui.add_space(space::XS);
}
