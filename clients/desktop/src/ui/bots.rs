//! The Bots window: the accounts this person runs.
//!
//! §41 asks for a bot surface a developer can build against, and this client had none of it: the
//! four management calls exist on the wire and nothing here asked them. This pane is that surface,
//! and it is the third of three — the web window and the Android panel draw the same four controls
//! with the same words for the same permission, because a person who set a bot's permissions on one
//! of them has to recognise the list on the others.
//!
//! Three things here are shaped by the wire rather than by taste:
//!
//!   * **A token is shown once.** The node stores a keyed tag, not the token, so a reply that is
//!     lost is a credential that is lost. The token therefore never enters the list — it appears in
//!     one card above everything, with the copy button and the warning — and rotating again is what
//!     a person does when they missed it, rather than a "show token" that would have to be absent
//!     to be honest.
//!   * **Permissions are replaced, never merged.** The editor holds a whole set and Save sends that
//!     whole set, which is what makes two windows open on the same bot agree rather than interleave
//!     into a union neither person chose.
//!   * **A paused bot is drawn from the flag the node sent.** Both tagged fields may be absent on a
//!     node that predates them, and this pane says "this server did not say" rather than drawing an
//!     active bot holding nothing — the difference between an old node and an off bot is one a
//!     management screen has to keep.
//!
//! Rotation takes two clicks, the admins pane's own rule: the old token stops working the moment
//! the node answers, and a running bot broken by a mis-click is a bot nobody can bring back
//! without the new token.

use egui::{Align, Layout, RichText, Ui};
use migo_core::Id;

use crate::net::Command;
use crate::theme::{font, palette, space, text_style};
use crate::ui::widgets;
use crate::ui::Context;

/// How many bot notices to keep; the stream is unbounded, the pane is not.
const MAX_NOTICES: usize = 5;

/// One permission the wire names, with the two lines a person deciding reads.
///
/// The slugs are the wire's own vocabulary — the closed set §41 fixes — and they never change, but
/// nobody granting authority should have to read `send_announcements` and guess at blast radius.
/// The label is the verb and the hint is what holding it lets a bot do; the web and Android pickers
/// use the same two lines, spelled from the same list.
pub struct Scope {
    pub slug: &'static str,
    label: &'static str,
    hint: &'static str,
}

/// Every permission this build can describe, in the order the pickers show them.
pub const SCOPES: [Scope; 6] = [
    Scope {
        slug: "read_messages",
        label: "Read messages",
        hint: "See the content of messages it is a member of.",
    },
    Scope {
        slug: "send_messages",
        label: "Send messages",
        hint: "Post messages as itself, on the ordinary messaging path.",
    },
    Scope {
        slug: "moderate",
        label: "Moderate",
        hint: "Act on members and content in the rooms it belongs to.",
    },
    Scope {
        slug: "manage_games",
        label: "Manage games",
        hint: "Start, join, and end games in its conversations.",
    },
    Scope {
        slug: "read_members",
        label: "Read members",
        hint: "See who is in its conversations and rooms.",
    },
    Scope {
        slug: "send_announcements",
        label: "Send announcements",
        hint: "Post to a room regardless of who has muted it.",
    },
];

/// What a slug means, in the words a person deciding would use.
///
/// An unknown slug is shown as itself rather than dropped: the node is the authority on which slugs
/// exist, and hiding one it accepts would be this pane pretending a permission it cannot describe
/// is not there.
#[must_use]
pub fn scope_label(slug: &str) -> &str {
    SCOPES
        .iter()
        .find(|scope| scope.slug == slug)
        .map_or(slug, |scope| scope.label)
}

/// The hint line under a slug's label, or one honest sentence for a slug this build does not name.
#[must_use]
fn scope_hint(slug: &str) -> &str {
    SCOPES
        .iter()
        .find(|scope| scope.slug == slug)
        .map_or("A permission this build does not describe.", |scope| {
            scope.hint
        })
}

/// A token the node minted, on its way to the person who has to save it.
///
/// Built by the shell from [`crate::net::Event::BotChanged`] and never inferred: the pane does not
/// assume a rotation produced one, it shows the one that arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct BotReveal {
    pub bot_id: Id,
    pub name: String,
    pub token: String,
    /// Whether this came from a rotation rather than a register, which changes one word.
    pub rotated: bool,
}

/// One notice a bot published about itself, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct BotNotice {
    pub bot_id: Id,
    pub event: String,
}

/// Everything the Bots pane holds between frames.
#[derive(Debug, Default)]
pub struct BotsState {
    /// The list, or `None` for "not read yet" — which is not the same as an empty list, and the
    /// pane draws the difference rather than showing a confident zero.
    pub listed: Option<Vec<migo_protocol::BotView>>,
    /// The one-time token card, when a register or a rotation just landed.
    pub reveal: Option<BotReveal>,
    /// The register form's handle.
    pub draft_username: String,
    /// The register form's display name.
    pub draft_name: String,
    /// True while a call is in flight, so the form's button cannot double-fire.
    pub busy: bool,
    /// The row a rotation was asked about; the second click acts on the id it holds rather than on
    /// a row position, because the list can change between the two clicks.
    confirming: Option<Id>,
    /// The row whose permission editor is open, beside the set it opened with. One at a time, so
    /// two editors cannot both be saved into the same bot from the same frame.
    editing: Option<(Id, Vec<String>)>,
    /// The last few things bots reported about themselves, newest first.
    pub notices: Vec<BotNotice>,
    /// Why the last call was refused, in the server's own words, when it was filed here.
    pub failure: Option<String>,
}

impl BotsState {
    /// Files a fresh list.
    pub fn file_list(&mut self, bots: Vec<migo_protocol::BotView>) {
        self.listed = Some(bots);
        self.busy = false;
    }

    /// Folds one changed bot back into the list, and opens the reveal when a token came with it.
    ///
    /// The row the node answered with replaces the one held rather than triggering a second read:
    /// the call already returned what it changed, and a re-read would be a round trip spent to
    /// learn what is in hand — one that could race the very answer it meant to confirm.
    pub fn file_changed(&mut self, view: migo_protocol::BotView, token: Option<String>) {
        self.busy = false;
        self.confirming = None;
        self.editing = None;
        self.failure = None;
        let bot_id = view.bot_id;
        let name = view.name.clone();
        // Whether this client already knew the bot is what tells a register from a rotation, and
        // the card says which. Read before the fold, because the fold is what makes it known.
        let known = self
            .listed
            .as_ref()
            .is_some_and(|rows| rows.iter().any(|row| row.bot_id == bot_id));
        match self.listed.as_mut() {
            Some(rows) => {
                if let Some(row) = rows.iter_mut().find(|row| row.bot_id == bot_id) {
                    *row = view;
                } else {
                    rows.push(view);
                }
            }
            // A change that arrives before the first list did is the whole list as far as this
            // pane knows: the row is real, and the next read settles the rest.
            None => self.listed = Some(vec![view]),
        }
        if let Some(token) = token {
            self.reveal = Some(BotReveal {
                bot_id,
                name,
                token,
                rotated: known,
            });
        }
    }

    /// Appends a bot's own report, newest first and capped.
    pub fn file_notice(&mut self, bot_id: Id, event: String) {
        self.notices.insert(0, BotNotice { bot_id, event });
        self.notices.truncate(MAX_NOTICES);
    }

    /// Files a refusal beside the form that caused it, and hands every control back.
    ///
    /// A refused call changed nothing, so nothing in the pane's own state moves with it — but the
    /// controls do, and that is the whole reason this exists: the four management calls leave the
    /// form and the row's levers disabled while their ask is out, and a refusal is an answer that
    /// will never be followed by a change. A pane that only learned of changes would disable its
    /// buttons for the rest of the session the first time a server said no.
    pub fn fail(&mut self, reason: String) {
        self.busy = false;
        self.confirming = None;
        self.editing = None;
        self.failure = Some(reason);
    }

    /// Puts the revealed token away. The value is gone from this client the moment it is called.
    pub fn dismiss_reveal(&mut self) {
        self.reveal = None;
    }
}

/// Draws the Bots place.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut BotsState) {
    let column = 520.0_f32.min(ui.available_width() - space::XL * 2.0);

    egui::ScrollArea::vertical()
        .id_salt("bots-pane")
        .max_height(ui.available_height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::XL);
                ui.allocate_ui(egui::vec2(column, 0.0), |ui| {
                    widgets::header(
                        ui,
                        context.theme,
                        "Bots",
                        Some("The accounts you run, and the token each signs in with"),
                    );
                    ui.add_space(space::MD);
                    ui.label(
                        RichText::new(
                            "A bot is an account you run. It signs in with a token, speaks as \
                             itself in the conversations it joins, and holds exactly the \
                             permissions you give it \u{2014} which start at none.",
                        )
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(palette(context.theme).text_muted),
                    );
                    ui.add_space(space::LG);

                    if let Some(reason) = &state.failure {
                        ui.label(
                            RichText::new(reason.clone())
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(palette(context.theme).warning),
                        );
                        ui.add_space(space::SM);
                    }

                    // The reveal, above everything: a token that has to be scrolled to is a token
                    // somebody rotates again for no reason.
                    reveal_card(ui, context, state);

                    register_form(ui, context, state);

                    if let Some(rows) = state.listed.clone() {
                        ui.add_space(space::LG);
                        notices(ui, context, state, &rows);
                        list(ui, context, state, &rows);
                    } else {
                        ui.add_space(space::LG);
                        if state.busy {
                            ui.spinner();
                        } else {
                            ui.label(
                                RichText::new(
                                    "The list is not loaded. This server has not been asked \
                                     yet, or the answer did not come.",
                                )
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(palette(context.theme).text_muted),
                            );
                            ui.add_space(space::SM);
                            if ui.button("Load bots").clicked() {
                                state.busy = true;
                                context.issue(Command::BotList);
                            }
                        }
                    }
                    ui.add_space(space::XL);
                });
            });
        });
}

/// The one-time token card.
fn reveal_card(ui: &mut Ui, context: &mut Context<'_>, state: &mut BotsState) {
    let Some(reveal) = state.reveal.clone() else {
        return;
    };
    let colors = palette(context.theme);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .stroke(egui::Stroke::new(1.0, colors.accent))
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(if reveal.rotated {
                    format!("New token for {}", reveal.name)
                } else {
                    format!("Token for {}", reveal.name)
                })
                .font(egui::FontId::proportional(font::SUBTITLE))
                .color(colors.text)
                .strong(),
            );
            ui.add_space(space::XS);
            ui.label(
                RichText::new(
                    "This is the only time it is shown. The server stores a tag of it, not the \
                     token, so nobody \u{2014} including this app \u{2014} can print it again. \
                     Put it somewhere safe now; if you lose it, rotate to mint another.",
                )
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
            );
            ui.add_space(space::SM);
            ui.label(
                RichText::new(&reveal.token)
                    .font(egui::FontId::monospace(font::SMALL))
                    .color(colors.text),
            );
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, context.theme, "Copy token", true).clicked() {
                    ui.ctx().copy_text(reveal.token.clone());
                }
                if widgets::ghost_button(ui, context.theme, "I have saved it").clicked() {
                    state.dismiss_reveal();
                }
            });
        });
    ui.add_space(space::SM);
}

/// The register form: a handle and a display name, and nothing else.
fn register_form(ui: &mut Ui, context: &mut Context<'_>, state: &mut BotsState) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Register a bot");
    widgets::field(
        ui,
        context.theme,
        "Handle",
        &mut state.draft_username,
        false,
        "lowercase letters, digits, dots and underscores",
    );
    widgets::field(
        ui,
        context.theme,
        "Display name",
        &mut state.draft_name,
        false,
        "what people see beside its messages",
    );
    let ready = !state.draft_username.trim().is_empty() && !state.draft_name.trim().is_empty();
    if widgets::primary_button(ui, context.theme, "Create bot", ready && !state.busy)
        .on_hover_text(
            "The handle is the account it signs in with and has to be free; the display name is \
             what people read.",
        )
        .clicked()
    {
        context.issue(Command::BotRegister {
            username: state.draft_username.trim().to_owned(),
            display_name: state.draft_name.trim().to_owned(),
        });
        state.busy = true;
        state.failure = None;
    }
    ui.add_space(space::XS);
    ui.label(
        RichText::new("The token is shown once, right after this, and never again.")
            .text_style(crate::theme::named(text_style::CAPTION))
            .color(colors.text_muted),
    );
}

/// What bots have reported about themselves since this window opened.
fn notices(ui: &mut Ui, context: &Context<'_>, state: &BotsState, rows: &[migo_protocol::BotView]) {
    if state.notices.is_empty() {
        return;
    }
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Recent activity");
    for notice in &state.notices {
        ui.label(
            RichText::new(format!(
                "{}: {}",
                name_of(rows, notice.bot_id),
                crate::model::spaced_words(&notice.event)
            ))
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
    }
    ui.add_space(space::LG);
}

/// The bots this account runs, one row each, every row carrying its own three controls.
fn list(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut BotsState,
    rows: &[migo_protocol::BotView],
) {
    widgets::subheader(ui, context.theme, "Your bots");
    if rows.is_empty() {
        widgets::empty_state(
            ui,
            context.theme,
            "No bots yet",
            "Register one above and give it the permissions it needs \u{2014} nothing more.",
        );
        return;
    }

    // The frame's outcomes, applied after the loop so no row widget reads state that a click
    // earlier in the same frame has already moved: `ask` arms a rotation, `confirm` acts on the
    // armed row, `pause` and `edit` and `save` are the other three levers.
    let mut ask: Option<Id> = None;
    let mut confirm: Option<Id> = None;
    let mut pause: Option<(Id, bool)> = None;
    let mut edit: Option<Option<(Id, Vec<String>)>> = None;
    let mut save: Option<(Id, Vec<String>)> = None;
    for row in rows {
        bot_row(
            ui,
            context,
            row,
            state,
            &mut ask,
            &mut confirm,
            &mut pause,
            &mut edit,
            &mut save,
        );
        ui.add_space(space::XS);
    }
    if let Some(bot_id) = ask {
        state.confirming = Some(bot_id);
    } else if confirm.is_some() {
        state.confirming = None;
    }
    if let Some(bot_id) = confirm {
        state.failure = None;
        context.issue(Command::BotRotate { bot_id });
    }
    if let Some((bot_id, paused)) = pause {
        context.issue(Command::BotPause { bot_id, paused });
    }
    if let Some(next) = edit {
        state.editing = next;
    }
    if let Some((bot_id, scopes)) = save {
        context.issue(Command::BotScopes { bot_id, scopes });
        state.busy = true;
    }
}

/// One bot: what it is called, whether it is paused, what it may do, and the levers.
#[allow(clippy::too_many_arguments)]
fn bot_row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    row: &migo_protocol::BotView,
    state: &BotsState,
    ask: &mut Option<Id>,
    confirm: &mut Option<Id>,
    pause: &mut Option<(Id, bool)>,
    edit: &mut Option<Option<(Id, Vec<String>)>>,
    save: &mut Option<(Id, Vec<String>)>,
) {
    let colors = palette(context.theme);
    let editing = matches!(&state.editing, Some((bot_id, _)) if *bot_id == row.bot_id);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .stroke(egui::Stroke::new(1.0, colors.border))
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                widgets::bot_badge(ui, context.theme, Some(row.bot_id), false);
                ui.label(
                    RichText::new(widgets::elide(&row.name, 40))
                        .font(egui::FontId::proportional(font::BODY))
                        .color(colors.text)
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // The flag the node sent, and an honest third word for a node that sent
                    // neither: absent is "this build cannot tell", not "off".
                    let (word, ink) = match row.paused {
                        Some(true) => ("Paused", colors.text_muted),
                        Some(false) => ("Active", colors.accent),
                        None => ("State unknown", colors.text_muted),
                    };
                    widgets::pill(ui, word, ink, colors.surface);
                });
            });
            ui.add_space(space::XS);
            ui.label(
                RichText::new(match &row.scopes {
                    None => "This server did not say what this bot may do.".to_owned(),
                    Some(scopes) if scopes.is_empty() => "No permissions.".to_owned(),
                    Some(scopes) => scopes
                        .iter()
                        .map(|slug| scope_label(slug))
                        .collect::<Vec<_>>()
                        .join(" \u{00B7} "),
                })
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
            );
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                let label = if row.paused == Some(true) {
                    "Resume"
                } else {
                    "Pause"
                };
                if widgets::ghost_button(ui, context.theme, label).clicked() {
                    // `paused != true` rather than `paused == false`: a node that predates the
                    // field is treated as an active bot, which is what it was before pausing
                    // existed, and the call carries the flag either way.
                    *pause = Some((row.bot_id, row.paused != Some(true)));
                }
                let editor = if editing {
                    "Close permissions"
                } else {
                    "Permissions"
                };
                if widgets::ghost_button(ui, context.theme, editor).clicked() {
                    *edit = Some(if editing {
                        None
                    } else {
                        Some((row.bot_id, row.scopes.clone().unwrap_or_default()))
                    });
                }
                let asking = state.confirming == Some(row.bot_id);
                let rotate = if asking {
                    "Rotate now \u{2014} the old token stops working"
                } else {
                    "New token"
                };
                let response = widgets::ghost_button(ui, context.theme, rotate);
                let response = if asking {
                    response.on_hover_text("Click again to mint a new token.")
                } else {
                    response.on_hover_text(
                        "Mints a token that replaces the old one immediately. Clicks ask first.",
                    )
                };
                if response.clicked() {
                    if asking {
                        *confirm = Some(row.bot_id);
                    } else {
                        *ask = Some(row.bot_id);
                    }
                }
            });
            if editing {
                if let Some((_, chosen)) = state.editing.clone() {
                    scope_editor(ui, context, row, &chosen, state.busy, edit, save);
                }
            }
        });
}

/// The permission picker: a whole set, saved whole.
///
/// A bot whose scopes the node did not report opens with nothing ticked and a sentence saying so,
/// because a picker that silently ticks nothing under a server that reported nothing is a picker
/// that lies about what the save will replace.
///
/// Every tick is handed back out through [`edit`] the moment it happens, and the set drawn next
/// frame comes from the state rather than from this function's own memory. That is not a
/// formality: a tick kept in a local would be rebuilt from the state on the very next frame and
/// silently undone, so a picker that looked live would save an empty set.
fn scope_editor(
    ui: &mut Ui,
    context: &Context<'_>,
    row: &migo_protocol::BotView,
    chosen: &[String],
    busy: bool,
    edit: &mut Option<Option<(Id, Vec<String>)>>,
    save: &mut Option<(Id, Vec<String>)>,
) {
    let colors = palette(context.theme);
    let mut ticked: Vec<String> = chosen.to_vec();
    ui.add_space(space::SM);
    if row.scopes.is_none() {
        ui.label(
            RichText::new(
                "This server did not report what the bot holds, so nothing is ticked. Saving \
                 replaces whatever it holds with exactly what is ticked here.",
            )
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
    }
    for scope in SCOPES {
        let mut on = ticked.iter().any(|slug| slug == scope.slug);
        let before = on;
        ui.horizontal(|ui| {
            ui.checkbox(&mut on, "");
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(scope.label)
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text),
                );
                ui.label(
                    RichText::new(scope.hint)
                        .text_style(crate::theme::named(text_style::CAPTION))
                        .color(colors.text_muted),
                );
            });
        });
        if on != before {
            ticked.retain(|slug| slug != scope.slug);
            if on {
                ticked.push(scope.slug.to_owned());
            }
            // Stored in the vocabulary's own order rather than in tick order, so the set the
            // state holds is the set the save will send and the list draws back unchanged.
            *edit = Some(Some((
                row.bot_id,
                SCOPES
                    .iter()
                    .map(|scope| scope.slug.to_owned())
                    .filter(|slug| ticked.iter().any(|held| held == slug))
                    .collect(),
            )));
        }
    }
    ui.add_space(space::XS);
    if widgets::primary_button(ui, context.theme, "Save permissions", !busy)
        .on_hover_text(
            "Replaces the bot's permissions with exactly this set. Disabled while an earlier \
             call to this server is still out.",
        )
        .clicked()
    {
        // Sent in the vocabulary's own order, so what leaves is what the list showed whatever
        // order the ticks happened in.
        *save = Some((row.bot_id, ticked));
    }
}

/// A bot's display name from the list in hand, or a neutral stand-in when the list lacks it.
fn name_of(rows: &[migo_protocol::BotView], bot_id: Id) -> String {
    rows.iter()
        .find(|row| row.bot_id == bot_id)
        .map_or_else(|| "A bot".to_owned(), |row| row.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(
        bot_id: Id,
        name: &str,
        paused: Option<bool>,
        scopes: Option<Vec<String>>,
    ) -> migo_protocol::BotView {
        migo_protocol::BotView {
            bot_id,
            name: name.to_owned(),
            token: None,
            paused,
            scopes,
        }
    }

    /// The pane's first frame is "not read yet", never an empty list: the two look identical and
    /// only one of them is a claim about the account.
    #[test]
    fn a_fresh_pane_has_not_read_the_list() {
        assert!(BotsState::default().listed.is_none());
    }

    /// A change folds into the row it names and leaves every other row alone.
    #[test]
    fn a_changed_bot_replaces_its_own_row() {
        let first = Id::from_bytes([1; 16]);
        let second = Id::from_bytes([2; 16]);
        let mut state = BotsState::default();
        state.file_list(vec![
            view(first, "one", Some(false), Some(Vec::new())),
            view(second, "two", Some(false), Some(Vec::new())),
        ]);

        state.file_changed(view(second, "two renamed", Some(true), None), None);

        let rows = state.listed.clone().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "one");
        assert_eq!(rows[1].name, "two renamed");
        assert_eq!(rows[1].paused, Some(true));
        assert!(
            state.reveal.is_none(),
            "a reply with no token reveals nothing"
        );
    }

    /// A token that arrives opens the reveal, and one that does not leaves the pane's hand alone:
    /// the difference has to come from the wire, because the node is the only thing that knows
    /// whether the call minted anything.
    ///
    /// Whether the pane already knew the bot is what tells a register from a rotation — the card
    /// says a different word for each — so both are pinned here rather than one.
    #[test]
    fn a_token_opens_the_reveal_and_the_card_knows_which_call_made_it() {
        let bot_id = Id::from_bytes([3; 16]);
        let mut state = BotsState::default();
        let mut minted = view(bot_id, "scribe", Some(false), Some(Vec::new()));
        minted.token = Some("migo_bot_secret".to_owned());

        // No list yet, so this bot is new to the pane: a register.
        state.file_changed(minted.clone(), Some("migo_bot_secret".to_owned()));
        let reveal = state.reveal.clone().expect("a token was minted");
        assert_eq!(reveal.bot_id, bot_id);
        assert_eq!(reveal.token, "migo_bot_secret");
        assert!(
            !reveal.rotated,
            "a bot the pane had never seen was registered"
        );

        // The same answer a second time: the row is in the list now, so this is a rotation.
        state.dismiss_reveal();
        state.file_changed(minted, Some("migo_bot_second".to_owned()));
        let reveal = state.reveal.clone().expect("a second token was minted");
        assert!(reveal.rotated, "a bot already in the list was rotated");

        state.dismiss_reveal();
        assert!(state.reveal.is_none());
        assert_eq!(
            state.listed.as_ref().map(Vec::len),
            Some(1),
            "a rotation replaces its row rather than adding one"
        );
    }

    /// The notice ring keeps the newest few and drops the rest, newest first.
    #[test]
    fn notices_are_capped_newest_first() {
        let bot_id = Id::from_bytes([4; 16]);
        let mut state = BotsState::default();
        for index in 0..(MAX_NOTICES + 3) {
            state.file_notice(bot_id, format!("event_{index}"));
        }
        assert_eq!(state.notices.len(), MAX_NOTICES);
        assert_eq!(
            state.notices[0].event,
            format!("event_{}", MAX_NOTICES + 2),
            "the newest notice is first"
        );
    }

    /// Every slug §41 fixes is described here, and a slug this build does not know is shown as
    /// itself rather than dropped.
    #[test]
    fn the_scope_vocabulary_is_the_wires_and_unknown_slugs_survive() {
        assert_eq!(SCOPES.len(), 6);
        assert_eq!(scope_label("send_announcements"), "Send announcements");
        assert_eq!(scope_label("teleport"), "teleport");
        assert!(!scope_hint("teleport").is_empty());
    }
}
