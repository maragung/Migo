//! The friends pane: the social graph, its pending requests, and the people search.
//!
//! # The graph belongs to the server
//!
//! Nothing here mutates local state to reflect an action. Accepting a request sends
//! [`Command::RespondFriend`] and waits: the pane redraws from the [`Event::Relationships`] that
//! follows the server's acknowledgement, not from an optimistic edit. A friendship is a fact
//! about two accounts and the server is the only witness both of them share, so a client that
//! patched its own copy would show a friendship the other party has not agreed to yet — the one
//! lie a friends list must never tell.
//!
//! # Why presence is a map and not a field on the row
//!
//! Presence arrives on its own schedule: seeded by a profile fetch, corrected by presence
//! events, and lost never — an account that stops being watched keeps its last known state
//! until the graph says otherwise. Holding it in a map keyed by id keeps those two arrivals
//! independent of the relationship list, so a presence event for someone whose entry has not
//! loaded yet is stored rather than dropped, and a list refresh does not blank every dot on
//! screen while the next fetch is in flight.

use std::collections::HashMap;

use egui::{Align, Color32, Layout, RichText, Ui};
use migo_core::Id;

use crate::model::{Presence, Relationship, RelationshipKind};
use crate::net::Command;
use crate::theme::{font, palette, space, text_style, Palette};
use crate::ui::widgets;
use crate::ui::Context;

/// Everything the friends pane holds between frames.
#[derive(Default)]
pub struct FriendsState {
    /// The social graph as the worker last reduced it.
    pub entries: Vec<Relationship>,
    /// Display names for the accounts in the graph. Filled by the same profile fetches the chat
    /// titles use, merged rather than replaced so a name already learned survives a refresh.
    pub names: HashMap<Id, String>,
    /// Last-known presence per account.
    pub presence: HashMap<Id, Presence>,
    /// The search field's contents.
    pub search: String,
    /// The add-friend field's contents.
    pub add_input: String,
}

impl FriendsState {
    /// Replaces the graph, keeping names and presence: both describe accounts, not edges, and
    /// an edge that vanished does not make a name wrong.
    pub fn set_relationships(&mut self, entries: Vec<Relationship>) {
        self.entries = entries;
    }

    /// Records one account's presence.
    pub fn set_presence(&mut self, user_id: Id, state: Presence) {
        self.presence.insert(user_id, state);
    }

    /// Merges freshly fetched names, keeping entries that already resolved.
    ///
    /// The same folding rule the chat's name cache uses: a profile answer that omits someone
    /// the pane already names must not blank the name it was asked to confirm.
    pub fn merge_names(&mut self, incoming: HashMap<Id, String>) {
        for (id, name) in incoming {
            if !name.is_empty() {
                self.names.insert(id, name);
            }
        }
    }
}

/// The three sections the pane draws, resolved out of state before any widget runs.
///
/// Owned because the Accept/Decline buttons mutate state through commands, and the row loop must
/// not borrow what a handler is about to change.
#[derive(Debug, Default, PartialEq)]
pub struct Sections {
    pub friends: Vec<Relationship>,
    pub incoming: Vec<Relationship>,
    pub outgoing: Vec<Relationship>,
    /// Edges this pane renders but offers no action for: follows, favourites, blocks, and kinds
    /// a newer server knows about that this build files under [`RelationshipKind::Unknown`].
    pub others: Vec<Relationship>,
}

/// Files the graph into sections, keeping only what the search string admits.
///
/// Pure, so the grouping and the filter are testable without a window: `query` matches an
/// account when it appears in the display name (case-insensitively) or in the id's text form,
/// because a paste of someone's full id is exactly the case where their name is unknown.
pub fn sections(entries: &[Relationship], query: &str, names: &HashMap<Id, String>) -> Sections {
    let needle = query.trim().to_ascii_lowercase();
    let mut out = Sections::default();
    for entry in entries {
        if !needle.is_empty() {
            let name = names.get(&entry.user_id);
            if !matches_query(
                &needle,
                name.map(String::as_str).unwrap_or(""),
                entry.user_id,
            ) {
                continue;
            }
        }
        match entry.kind {
            RelationshipKind::Friend => out.friends.push(entry.clone()),
            RelationshipKind::PendingIncoming => out.incoming.push(entry.clone()),
            RelationshipKind::PendingOutgoing => out.outgoing.push(entry.clone()),
            RelationshipKind::Unknown
            | RelationshipKind::Follow
            | RelationshipKind::Block
            | RelationshipKind::Favorite => out.others.push(entry.clone()),
        }
    }
    out
}

/// Whether a search needle admits an account.
///
/// The needle must already be lowercased — [`sections`] does that once for the whole list — and
/// the name is lowercased here, so the one allocation happens per row rather than per row per
/// character of the query.
fn matches_query(needle: &str, name: &str, id: Id) -> bool {
    if needle.is_empty() {
        return true;
    }
    name.to_ascii_lowercase().contains(needle) || id.to_text().to_ascii_lowercase().contains(needle)
}

/// Draws the friends pane.
///
/// The whole pane scrolls rather than only the list: a graph with a hundred edges and three
/// pending requests is one document about one account, and clipping the bottom of it would hide
/// the Accept button a request is waiting on.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut FriendsState) {
    let column = 420.0_f32.min(ui.available_width() - space::XL * 2.0);

    egui::ScrollArea::vertical()
        .id_salt("friends-pane")
        .max_height(ui.available_height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::XL);
                ui.allocate_ui(egui::vec2(column, 0.0), |ui| {
                    widgets::header(
                        ui,
                        context.theme,
                        "Friends",
                        Some("Add someone by their account id, and talk when they accept."),
                    );
                    ui.add_space(space::LG);

                    add_row(ui, context, state);
                    ui.add_space(space::SM);
                    search_row(ui, state);
                    ui.add_space(space::LG);

                    if state.entries.is_empty() {
                        widgets::empty_state(
                            ui,
                            context.theme,
                            "No friends yet",
                            "Paste someone's account id above to send a request.",
                        );
                        return;
                    }

                    let resolved = sections(&state.entries, &state.search, &state.names);
                    section(
                        ui,
                        context,
                        state,
                        "Requests",
                        &resolved.incoming,
                        None,
                        true,
                    );
                    section(ui, context, state, "Sent", &resolved.outgoing, None, false);
                    // The live count, not the row count: "Friends · 3 online" answers the question
                    // the pane exists for, which is "who is around right now".
                    let online = resolved
                        .friends
                        .iter()
                        .filter(|entry| {
                            state
                                .presence
                                .get(&entry.user_id)
                                .is_some_and(|presence| presence.is_online())
                        })
                        .count();
                    let online_meta = (online > 0).then(|| format!("{online} online"));
                    section(
                        ui,
                        context,
                        state,
                        "Friends",
                        &resolved.friends,
                        online_meta.as_deref(),
                        false,
                    );
                    section(ui, context, state, "Others", &resolved.others, None, false);
                });
            });
        });
}

/// The add-friend field and its button.
fn add_row(ui: &mut Ui, context: &mut Context<'_>, state: &mut FriendsState) {
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.add_input)
                .hint_text("account id")
                .desired_width(ui.available_width() - 84.0),
        );
        let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("Add").clicked() || submitted) && !state.add_input.trim().is_empty() {
            context.issue(Command::AddFriend {
                user_id: state.add_input.trim().to_owned(),
            });
            state.add_input.clear();
        }
    });
}

/// The search field.
fn search_row(ui: &mut Ui, state: &mut FriendsState) {
    ui.add(
        egui::TextEdit::singleline(&mut state.search)
            .hint_text("Search")
            .desired_width(f32::INFINITY),
    );
}

/// One titled group of rows.
///
/// `with_actions` draws Accept/Decline on each row — only the incoming requests have them,
/// because acting on anything else is not something this pane offers.
fn section(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut FriendsState,
    title: &str,
    entries: &[Relationship],
    meta: Option<&str>,
    with_actions: bool,
) {
    if entries.is_empty() {
        return;
    }
    let colors = palette(context.theme);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text_muted),
        );
        if let Some(meta) = meta {
            widgets::pill(ui, meta, colors.text_muted, colors.surface_raised);
        }
    });
    ui.add_space(space::XS);

    let mut actions: Vec<(Id, bool)> = Vec::new();
    for entry in entries {
        row(ui, context, state, entry, with_actions, &mut actions);
        ui.add_space(space::XS);
    }
    for (user_id, accept) in actions {
        context.issue(Command::RespondFriend { user_id, accept });
    }
    ui.add_space(space::SM);
}

/// One account row: avatar, name, presence dot, and the action buttons when there are any.
fn row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &FriendsState,
    entry: &Relationship,
    with_actions: bool,
    actions: &mut Vec<(Id, bool)>,
) {
    let colors = palette(context.theme);
    let name = state
        .names
        .get(&entry.user_id)
        .cloned()
        .unwrap_or_else(|| crate::model::short_id(entry.user_id));

    ui.horizontal(|ui| {
        widgets::avatar(ui, context.theme, &name, 30.0);
        ui.add_space(space::SM);
        ui.label(
            RichText::new(widgets::elide(&name, 30))
                .font(egui::FontId::proportional(font::BODY))
                .color(colors.text),
        );
        let presence = state.presence.get(&entry.user_id).copied();
        if let Some(presence) = presence {
            presence_dot(ui, presence, colors);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if with_actions {
                if widgets::primary_button(ui, context.theme, "Accept", true).clicked() {
                    actions.push((entry.user_id, true));
                }
                ui.add_space(space::XS);
                if widgets::ghost_button(ui, context.theme, "Decline").clicked() {
                    actions.push((entry.user_id, false));
                }
            } else if entry.kind == RelationshipKind::PendingOutgoing {
                widgets::pill(ui, "waiting", colors.text_muted, colors.surface_raised);
            }
        });
    });
}

/// The presence dot and its word.
///
/// The colour mapping lives here rather than in the theme because it is a *meaning*, not a
/// surface: online is the positive green everywhere in the product, busy is the danger red,
/// away the warning amber, and offline the muted grey of anything not currently true. An
/// unobserved account draws no dot at all — "offline" is a claim, and this client has not made
/// it.
fn presence_dot(ui: &mut Ui, presence: Presence, colors: Palette) {
    let label = presence.label();
    if label.is_empty() {
        return;
    }
    let color = presence_color(presence, colors);
    let diameter = 8.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, color);
    ui.add_space(space::XS);
    ui.label(
        RichText::new(label)
            .font(egui::FontId::proportional(font::TINY))
            .color(colors.text_muted),
    );
}

/// The colour a presence state draws in. Pure, so the mapping is pinned by a test.
///
/// Green is reserved for `Online` alone — it is the colour the whole product uses for "this
/// thing is live right now", and spending it on "away" would blunt it.
pub(crate) fn presence_color(presence: Presence, colors: Palette) -> Color32 {
    match presence {
        Presence::Online => colors.positive,
        Presence::Away => colors.warning,
        Presence::Busy => colors.danger,
        Presence::Offline | Presence::Unknown => colors.text_muted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn id(n: u8) -> Id {
        Id::from_bytes([n; 16])
    }

    fn named(names: &[(u8, &str)]) -> HashMap<Id, String> {
        names
            .iter()
            .map(|(n, name)| (id(*n), (*name).to_owned()))
            .collect()
    }

    #[test]
    fn sections_files_each_kind_exactly_once() {
        let entries = vec![
            Relationship {
                user_id: id(1),
                kind: RelationshipKind::Friend,
            },
            Relationship {
                user_id: id(2),
                kind: RelationshipKind::PendingIncoming,
            },
            Relationship {
                user_id: id(3),
                kind: RelationshipKind::PendingOutgoing,
            },
            Relationship {
                user_id: id(4),
                kind: RelationshipKind::Block,
            },
            Relationship {
                user_id: id(5),
                kind: RelationshipKind::Follow,
            },
            Relationship {
                user_id: id(6),
                kind: RelationshipKind::Unknown,
            },
        ];
        let split = sections(&entries, "", &HashMap::new());
        assert_eq!(split.friends.len(), 1);
        assert_eq!(split.incoming.len(), 1);
        assert_eq!(split.outgoing.len(), 1);
        assert_eq!(split.others.len(), 3);
        // Every entry lands somewhere, so nothing is silently dropped.
        assert_eq!(
            split.friends.len() + split.incoming.len() + split.outgoing.len() + split.others.len(),
            entries.len()
        );
    }

    #[test]
    fn search_matches_names_case_insensitively_and_ids_by_text() {
        let names = named(&[(1, "Rina"), (2, "jo")]);
        // The needle arrives lowercased from `sections`; the name is matched without case.
        assert!(matches_query("rina", names.get(&id(1)).unwrap(), id(1)));
        assert!(!matches_query("rina", names.get(&id(2)).unwrap(), id(2)));
        // An id pasted whole matches even when the name is unknown.
        assert!(matches_query(
            &id(7).to_text().to_ascii_lowercase(),
            "",
            id(7)
        ));
        // The empty needle admits everyone.
        assert!(matches_query("", "", id(9)));
    }

    #[test]
    fn search_normalises_the_query_before_matching() {
        let entries = vec![Relationship {
            user_id: id(1),
            kind: RelationshipKind::Friend,
        }];
        let names = named(&[(1, "Rina")]);
        // Typed with the caps lock on, still finds her.
        let split = sections(&entries, "  RINA ", &names);
        assert_eq!(split.friends.len(), 1);
    }

    #[test]
    fn search_filters_the_sections() {
        let entries = vec![
            Relationship {
                user_id: id(1),
                kind: RelationshipKind::Friend,
            },
            Relationship {
                user_id: id(2),
                kind: RelationshipKind::Friend,
            },
        ];
        let names = named(&[(1, "Rina"), (2, "Jonah")]);
        let split = sections(&entries, "rina", &names);
        assert_eq!(split.friends.len(), 1);
        assert_eq!(split.friends[0].user_id, id(1));
    }

    #[test]
    fn presence_colours_are_meaningful_not_decorative() {
        let colors = palette(Theme::Dark);
        assert_eq!(presence_color(Presence::Online, colors), colors.positive);
        assert_eq!(presence_color(Presence::Busy, colors), colors.danger);
        assert_eq!(presence_color(Presence::Away, colors), colors.warning);
        assert_eq!(presence_color(Presence::Offline, colors), colors.text_muted);
    }
}
