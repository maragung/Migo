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

use egui::{Align, Color32, CornerRadius, Layout, Response, RichText, Sense, Ui};
use migo_core::Id;

use crate::model::{Presence, Relationship, RelationshipKind};
use crate::net::Command;
use crate::theme::{font, palette, space, text_style, Palette, Theme};
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
    /// Whether the search field is showing. The field lives behind the search icon in the
    /// header row until the icon is asked for — the header's room belongs to the two
    /// conversation doors, and the graph is searched rarely enough that a standing field
    /// spends most of its life as an empty box — and what is revealed is revealed focused,
    /// because the click that asked for the field asked to type in it. Survives frames, like
    /// every field state here, so a repaint never folds a search in progress; folding is the
    /// field's own doing (see [`search_done`]).
    pub search_open: bool,
    /// Whether the revealed search field should claim the keyboard focus this frame. Set by
    /// the reveal click, spent by the field the next time it draws — the same one-shot
    /// claim the new-group form's `claim_focus` is.
    pub search_focus: bool,
    /// The add-friend field's contents.
    pub add_input: String,
    /// The username typed into the new-chat field.
    pub new_peer: String,
    /// Whether the new-chat field is showing.
    pub composing_new: bool,
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
    /// Accounts this caller has blocked. Rendered with no action: the wire is set-only —
    /// there is no unblock opcode — so a block is the one row the pane is honest about
    /// being unable to undo from here.
    pub blocked: Vec<Relationship>,
    /// Accounts this caller has muted, each with its Unmute.
    pub muted: Vec<Relationship>,
    /// Edges this pane renders but offers no action for: follows, favourites, and kinds
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
            RelationshipKind::Block => out.blocked.push(entry.clone()),
            RelationshipKind::Mute => out.muted.push(entry.clone()),
            RelationshipKind::Unknown | RelationshipKind::Follow | RelationshipKind::Favorite => {
                out.others.push(entry.clone())
            }
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
pub fn show(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut FriendsState,
    chat: &mut crate::ui::chat::ChatState,
) {
    let column = 420.0_f32.min(ui.available_width() - space::XL * 2.0);

    egui::ScrollArea::vertical()
        .id_salt("friends-pane")
        .max_height(ui.available_height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::XL);
                ui.allocate_ui(egui::vec2(column, 0.0), |ui| {
                    header_row(ui, context, state, chat);
                    ui.add_space(space::MD);

                    // The new-conversation fold-outs, under the doors that opened them: the
                    // group form and the direct-chat field each stay only while they stand.
                    new_conversation_folds(ui, context, state, chat);
                    ui.add_space(space::SM);

                    add_row(ui, context, state);
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
                    // The two personal verdicts, after the people: a mute is a volume
                    // control with its own switch back, a block a door the wire will not
                    // reopen from here. Both sections state what they are; neither draws
                    // an action it cannot deliver.
                    section(ui, context, state, "Muted", &resolved.muted, None, false);
                    section(
                        ui,
                        context,
                        state,
                        "Blocked",
                        &resolved.blocked,
                        None,
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

/// The pane's header row: the pane's name at the left edge, and its search behind the search
/// icon inline with the two conversation doors at the right — the icon left of the new-chat
/// buttons, the field where the icon stood once it is asked for.
///
/// The search used to be a field of its own below the header, then a field always inline;
/// both spent the header's room on a box that is asked for rarely, so now the field appears
/// only when the icon is clicked — focused, because the click asked to type — and folds away
/// when the search is *finished*: the × beside the field clears a query that stands and folds
/// a field that has nothing left to clear, and losing focus with nothing asked folds it too
/// (see [`search_done`]). The filtering is unchanged: every section below still answers the
/// same needle, drawn from the same `state.search` the field writes.
fn header_row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut FriendsState,
    chat: &mut crate::ui::chat::ChatState,
) {
    let colors = palette(context.theme);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Friends")
                .font(egui::FontId::proportional(font::TITLE))
                .color(colors.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // The new-chat door, rightmost: the reference has no Chats tab, and a
            // conversation opens from wherever a person is found, as a closable tab of its
            // own — the same "New chat" the web client's friends panel offers.
            if ui.button("+ Chat").clicked() {
                state.composing_new = !state.composing_new;
            }
            // The new-group door, beside the new-chat one: a group conversation is the other
            // thing a friends list is for, and the web client's friends panel offers both.
            if ui.button("+ Group").clicked() {
                chat.new_group = Some(crate::ui::chat::NewGroupForm {
                    claim_focus: true,
                    ..Default::default()
                });
            }
            // The search, leftmost of the three: the magnifier until it is asked for, then
            // the field with the fold beside it. The × is one press per half of "finished" —
            // first the query, then the field — so a standing search is never thrown away by
            // a single click, and losing focus with nothing asked is the same finished state
            // the empty × reports.
            if state.search_open {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.search)
                        .hint_text("Search")
                        .desired_width(110.0),
                );
                if state.search_focus {
                    response.request_focus();
                    state.search_focus = false;
                }
                // Whether the field folds for losing focus is decided before the × below
                // runs: the × clears the query, and a query that was standing when the focus
                // left is not a finished search — the fold rule must not read the field the
                // × just emptied.
                let fold_on_blur = search_done(&state.search, response.lost_focus());
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("\u{2715}")
                                .font(egui::FontId::proportional(font::TINY))
                                .color(colors.text_muted),
                        )
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE),
                    )
                    .on_hover_text("Clear the search, then fold the field away")
                    .clicked()
                {
                    if search_done(&state.search, true) {
                        state.search_open = false;
                    } else {
                        state.search.clear();
                    }
                }
                if fold_on_blur {
                    state.search_open = false;
                }
            } else if search_button(ui, context.theme).clicked() {
                state.search_open = true;
                state.search_focus = true;
            }
        });
    });
    ui.add_space(space::XS);
    ui.label(
        RichText::new("Add someone by their account id, and talk when they accept.")
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
    );
}

/// The collapsed search: the magnifier on a quiet clickable box, standing where the field
/// will.
///
/// Drawn the way the account bar's bell button is — the same 26px box, the same hover wash —
/// and painted with the strip's own magnifier ([`widgets::place_icon`]'s Search), so the
/// reveal reads as one of the header's quiet controls rather than as a new thing.
fn search_button(ui: &mut Ui, theme: Theme) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), Sense::click());
    let fill = if response.hovered() {
        Color32::from_black_alpha(60)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    }
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                rect.center() - egui::vec2(10.0, 10.0),
                egui::vec2(20.0, 20.0),
            ))
            .layout(Layout::left_to_right(Align::Center)),
    );
    widgets::place_icon(&mut inner, theme, crate::ui::Place::Search, false);
    response
}

/// Whether the search is finished: nothing asked, and the field dismissed.
///
/// Pure, because "only a dismissed empty field folds" is the whole contract and two callers
/// ask it — the × beside the field, and the field losing focus — and a rule that lives in two
/// places is a rule that drifts. A query that stands keeps the field open however the
/// dismissal arrives; the × clears the words first and folds on the next press, so one click
/// never throws away a search.
fn search_done(query: &str, dismissed: bool) -> bool {
    dismissed && query.trim().is_empty()
}

/// The new-conversation fold-outs: the group form and the direct-chat field, each drawn only
/// while its door left it open.
fn new_conversation_folds(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut FriendsState,
    chat: &mut crate::ui::chat::ChatState,
) {
    // The form is taken out of the chat state for the draw, so the form's own submit and
    // cancel — which write that same Option — never fight the borrow the draw holds. A form
    // that survives the draw goes back; a submitted or cancelled one never comes back.
    if let Some(mut form) = chat.new_group.take() {
        let outcome = new_group_form(ui, context, state, &mut form);
        chat.new_group = match outcome {
            GroupFormOutcome::Open => Some(form),
            GroupFormOutcome::Cancelled => None,
            GroupFormOutcome::Create => {
                context.issue(Command::CreateGroup {
                    members: form.picked.clone(),
                    title: form.title.trim().to_owned(),
                });
                None
            }
        };
    }
    if state.composing_new {
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut state.new_peer)
                    .hint_text("account id")
                    .desired_width(ui.available_width() - 84.0),
            );
            let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Go").clicked() || submitted) && !state.new_peer.trim().is_empty() {
                context.issue(Command::StartDirect {
                    username: state.new_peer.trim().to_owned(),
                });
                state.new_peer.clear();
                state.composing_new = false;
            }
        });
    }
}

/// The new-group form: a title, the friends to pick as founding members, and a manual
/// account-id field for the person the list does not show. The members picked here are the
/// group's *other* members — the server adds the caller and names them its founder.
/// What became of the new-group form in one draw: still open, dismissed, or submitted. The
/// caller owns the consequence — the form is taken out of the chat state for the draw, so
/// what happens to it afterwards is one decision in one place.
enum GroupFormOutcome {
    Open,
    Cancelled,
    Create,
}

fn new_group_form(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &FriendsState,
    form: &mut crate::ui::chat::NewGroupForm,
) -> GroupFormOutcome {
    let colors = palette(context.theme);
    // Deferred, past the borrows above: the create is issued after the form has finished
    // drawing, the same patience every typed field in this client is given.
    let mut submitted = false;
    let mut closed = false;
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
        .show(ui, |ui| {
            let title_response = widgets::field(
                ui,
                context.theme,
                "Group name",
                &mut form.title,
                false,
                "Weekend plans",
            );
            if form.claim_focus {
                title_response.request_focus();
                form.claim_focus = false;
            }
            ui.label(
                RichText::new("Pick the founding members")
                    .text_style(crate::theme::named(crate::theme::text_style::OVERLINE))
                    .color(colors.text_muted),
            );
            ui.add_space(space::XS);
            // The friends who are already picked, as removable chips: a pick is reversible
            // until the create, the same way every other form here is. The removal is
            // deferred past the iteration, because a chip cannot unbutton itself out of the
            // list it is drawn from.
            if !form.picked.is_empty() {
                let mut unpick: Option<usize> = None;
                ui.horizontal_wrapped(|ui| {
                    for (index, picked) in form.picked.iter().enumerate() {
                        let name = state
                            .names
                            .get(picked)
                            .cloned()
                            .unwrap_or_else(|| crate::model::short_id(*picked));
                        if ui
                            .add(
                                egui::Button::new(format!("{name} \u{2715}"))
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                            )
                            .clicked()
                        {
                            unpick = Some(index);
                        }
                    }
                });
                if let Some(index) = unpick {
                    form.picked.remove(index);
                }
                ui.add_space(space::XS);
            }
            // The friends list as toggleable rows: a friend already picked is a chip above,
            // so the row below only offers the ones not yet picked.
            let friends: Vec<crate::model::Relationship> = state
                .entries
                .iter()
                .filter(|entry| entry.kind == crate::model::RelationshipKind::Friend)
                .filter(|entry| !form.picked.contains(&entry.user_id))
                .cloned()
                .collect();
            if friends.is_empty() {
                ui.label(
                    RichText::new("Every friend is already picked — or there are none yet.")
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
            }
            let mut add_pick: Option<migo_core::Id> = None;
            for friend in &friends {
                let name = state
                    .names
                    .get(&friend.user_id)
                    .cloned()
                    .unwrap_or_else(|| crate::model::short_id(friend.user_id));
                ui.horizontal(|ui| {
                    widgets::avatar(ui, context.theme, &name, 22.0);
                    ui.label(
                        RichText::new(name)
                            .font(egui::FontId::proportional(font::SMALL))
                            .color(colors.text),
                    );
                    if ui
                        .add(
                            egui::Button::new("Add")
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                        )
                        .clicked()
                    {
                        add_pick = Some(friend.user_id);
                    }
                });
            }
            if let Some(pick) = add_pick {
                form.picked.push(pick);
            }
            // The manual field, for the account id the friends list cannot name.
            let manual_response = ui.add(
                egui::TextEdit::singleline(&mut form.manual)
                    .hint_text("or paste an account id")
                    .desired_width(ui.available_width() - 96.0),
            );
            let manual_submitted =
                manual_response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Add by id").clicked() || manual_submitted)
                && !form.manual.trim().is_empty()
            {
                if let Ok(id) = migo_core::Id::parse(form.manual.trim()) {
                    if !form.picked.contains(&id) {
                        form.picked.push(id);
                    }
                    form.manual.clear();
                }
            }
            ui.add_space(space::SM);
            // The create, held back until the form has somebody in it: the server would only
            // refuse with "a conversation needs somebody other than its creator", and a
            // refusal the person can see coming is kinder than one that arrives.
            let can_create = !form.picked.is_empty() && !form.title.trim().is_empty();
            if widgets::primary_button(ui, context.theme, "Create group", can_create).clicked() {
                submitted = true;
            }
            if ui.button("Cancel").clicked() {
                closed = true;
            }
        });
    if submitted {
        GroupFormOutcome::Create
    } else if closed {
        GroupFormOutcome::Cancelled
    } else {
        GroupFormOutcome::Open
    }
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
    let mut message: Option<Id> = None;
    let mut unmutes: Vec<Id> = Vec::new();
    for entry in entries {
        row(
            ui,
            context,
            state,
            entry,
            with_actions,
            &mut actions,
            &mut message,
            &mut unmutes,
        );
        ui.add_space(space::XS);
    }
    for (user_id, accept) in actions {
        context.issue(Command::RespondFriend { user_id, accept });
    }
    // A Message click asked for a thread: the create's answer opens the tab (see
    // `Event::ConversationCreated`), the same open-on-create the web panel does.
    if let Some(peer) = message {
        context.issue(Command::StartDirectById { peer });
    }
    // An Unmute click clears the personal mute: the wire's own word for it is the same
    // opcode with the switch off.
    for user_id in unmutes {
        context.issue(Command::MuteUser { user_id, on: false });
    }
    ui.add_space(space::SM);
}

/// One account row: avatar, name, presence dot, and the action buttons when there are any.
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &FriendsState,
    entry: &Relationship,
    with_actions: bool,
    actions: &mut Vec<(Id, bool)>,
    message: &mut Option<Id>,
    unmutes: &mut Vec<Id>,
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
            } else if entry.kind == RelationshipKind::Friend {
                // The reference has no Chats tab: a thread opens from the person, so a friend's
                // row carries the Message action the web panel's rows carry.
                if widgets::ghost_button(ui, context.theme, "Message").clicked() {
                    *message = Some(entry.user_id);
                }
            } else if entry.kind == RelationshipKind::Mute {
                // A volume control, so the row carries its own switch back: Unmute is the
                // same opcode with the flag off, and no confirmation is owed — the choice
                // never told the other person anything in the first place.
                if widgets::ghost_button(ui, context.theme, "Unmute").clicked() {
                    unmutes.push(entry.user_id);
                }
            } else if entry.kind == RelationshipKind::PendingOutgoing {
                widgets::pill(ui, "waiting", colors.text_muted, colors.surface_raised);
            } else if entry.kind == RelationshipKind::Block {
                // Set-only on the wire: no unblock opcode exists, so the pill states the
                // fact rather than offering a switch this client cannot deliver.
                widgets::pill(ui, "blocked", colors.text_muted, colors.surface_raised);
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

    /// The field folds only on a dismissed empty query: words standing keep it open however
    /// the dismissal arrives, and only whitespace counts as nothing. Pinned so the × and the
    /// lost-focus rule — two callers of one rule — cannot drift into two different ideas of
    /// "finished".
    #[test]
    fn the_search_folds_only_on_a_dismissed_empty_query() {
        assert!(search_done("", true));
        assert!(search_done("   ", true));
        assert!(!search_done("rina", true));
        assert!(!search_done("", false));
        assert!(!search_done("rina", false));
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
            Relationship {
                user_id: id(7),
                kind: RelationshipKind::Mute,
            },
        ];
        let split = sections(&entries, "", &HashMap::new());
        assert_eq!(split.friends.len(), 1);
        assert_eq!(split.incoming.len(), 1);
        assert_eq!(split.outgoing.len(), 1);
        // A block files under its own section, and a mute under its.
        assert_eq!(split.blocked.len(), 1);
        assert_eq!(split.muted.len(), 1);
        assert_eq!(split.others.len(), 2);
        // Every entry lands somewhere, so nothing is silently dropped.
        assert_eq!(
            split.friends.len()
                + split.incoming.len()
                + split.outgoing.len()
                + split.blocked.len()
                + split.muted.len()
                + split.others.len(),
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
