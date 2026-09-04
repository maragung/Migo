//! The Owner/CEO's management pane for the global admins: who may moderate every public room.
//!
//! # The surface is closed by construction, twice
//!
//! The banner menu only offers the place after the worker's standing check says the viewer is
//! the owner, and the server refuses every read and write here for anybody else — so a stale
//! client that kept the pane open after the owner designation moved draws the refusal, not a
//! silent blank. The pane never renders the list before it knows the viewer may see it,
//! because the management page's whole point is that its existence is not public information.
//!
//! # A revoke is a two-step action, always
//!
//! The row's first click asks, the second acts. An accidental click on a destructive action
//! must never move moderation away from a person in one step, and the second click must show
//! the name it is about to act on, so the wrong row cannot be confirmed by habit. The
//! asking state holds the row's id, not its index — the list can refresh between the two
//! clicks, and a confirm keyed to a position would act on whatever moved into it.

use egui::{Align, Layout, RichText, Ui};
use migo_core::Id;

use crate::model::AdminRow;
use crate::net::{AdminsAnswer, Command};
use crate::theme::{font, palette, space, text_style};
use crate::ui::widgets;
use crate::ui::Context;

/// Everything the admins pane holds between frames.
#[derive(Debug, Default)]
pub struct AdminsState {
    /// The standing-and-list answer, as last asked or answered. The pane asks on entry, so
    /// `Loading` — the default — is the honest first frame, never drawn as a claim about
    /// standing the pane has not checked.
    pub answer: AdminsAnswer,
    /// The grant form's draft username.
    pub draft: String,
    /// True while a grant is in flight, so the form's button cannot double-fire.
    pub granting: bool,
    /// The row a revoke was asked about, holding the id it will act on.
    revoking: Option<Id>,
    /// Why the last grant or revoke was refused, in the server's own words. Filed rather than
    /// toasted because it belongs beside the form or row that caused it.
    pub failure: Option<String>,
}

impl AdminsState {
    /// Files a refusal of a grant or revoke, keeping the pane as it stands.
    pub fn fail(&mut self, reason: String) {
        self.failure = Some(reason);
        self.granting = false;
    }

    /// Clears the in-flight grant mark when an answer arrives, whatever it was.
    pub fn settled(&mut self) {
        self.granting = false;
    }
}

/// Draws the admins pane.
///
/// Scrolls as one document, the profile pane's own shape, so a list long enough to push the
/// grant form off the bottom of the window pushes it into reach of the wheel instead of out
/// of the interface.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut AdminsState) {
    let column = 460.0_f32.min(ui.available_width() - space::XL * 2.0);

    egui::ScrollArea::vertical()
        .id_salt("admins-pane")
        .max_height(ui.available_height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::XL);
                ui.allocate_ui(egui::vec2(column, 0.0), |ui| {
                    widgets::header(
                        ui,
                        context.theme,
                        "Global Admins",
                        Some("Who moderates every public room"),
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

                    match state.answer.clone() {
                        AdminsAnswer::Loading => {
                            ui.spinner();
                        }
                        AdminsAnswer::Failed(reason) => {
                            ui.label(
                                RichText::new(format!("Admin list unavailable: {reason}"))
                                    .font(egui::FontId::proportional(font::SMALL))
                                    .color(palette(context.theme).warning),
                            );
                            ui.add_space(space::SM);
                            if ui.button("Retry").clicked() {
                                state.answer = AdminsAnswer::Loading;
                                context.issue(Command::Admins);
                            }
                        }
                        AdminsAnswer::Closed => {
                            ui.label(
                                RichText::new(
                                    "This page belongs to the Migo Owner/CEO. Your account \
                                     cannot open it.",
                                )
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(palette(context.theme).text_muted),
                            );
                        }
                        AdminsAnswer::Owner(rows) => {
                            grant_section(ui, context, state);
                            ui.add_space(space::LG);
                            list_section(ui, context, state, &rows);
                        }
                    }
                    ui.add_space(space::XL);
                });
            });
        });
}

/// The grant form: a username and a button that stays disabled until the name is something.
///
/// The gate lives in the markup, not only in the handler, so a refactor of the handler cannot
/// ship a form that sends empty requests.
fn grant_section(ui: &mut Ui, context: &mut Context<'_>, state: &mut AdminsState) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Appoint");
    ui.label(
        RichText::new(
            "Global admins moderate every public room. Appointing and revoking is the \
             Owner/CEO\u{2019}s alone.",
        )
        .font(egui::FontId::proportional(font::SMALL))
        .color(colors.text_muted),
    );
    ui.add_space(space::SM);
    widgets::field(
        ui,
        context.theme,
        "Username to appoint",
        &mut state.draft,
        false,
        "username",
    );
    ui.add_space(space::XS);
    let ready = !state.draft.trim().is_empty();
    if widgets::primary_button(ui, context.theme, "Appoint", ready && !state.granting)
        .on_hover_text("Appoints by username; a repeated appointment keeps the original grant.")
        .clicked()
    {
        context.issue(Command::GrantAdmin {
            username: state.draft.trim().to_owned(),
        });
        state.granting = true;
        state.failure = None;
    }
}

/// The current admins, one row each, every row carrying its revoke.
fn list_section(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut AdminsState,
    rows: &[AdminRow],
) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Current admins");
    if rows.is_empty() {
        ui.label(
            RichText::new("No global admins yet.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        return;
    }

    // Two slots for the frame's outcomes, applied after the loop so the row widgets finish
    // their frame before the state they read changes: `ask` arms a row's confirm (first
    // click), `confirm` acts on the armed row (second click).
    let mut ask: Option<Id> = None;
    let mut confirm: Option<Id> = None;
    for row in rows {
        admin_row(ui, context, row, state.revoking, &mut ask, &mut confirm);
        ui.add_space(space::XS);
    }
    if let Some(account_id) = ask {
        state.revoking = Some(account_id);
    } else if confirm.is_some() {
        state.revoking = None;
    }
    if let Some(account_id) = confirm {
        state.failure = None;
        context.issue(Command::RevokeAdmin { account_id });
    }
}

/// One admin row: the name, when the grant happened, and the revoke.
///
/// The revoke asks first: the button's first click arms the confirm ("Confirm — name"), and
/// only a second click on that same row acts. Clicking any other row re-arms it, so a confirm
/// never acts on a row the user cannot currently see named.
#[allow(clippy::too_many_arguments)]
fn admin_row(
    ui: &mut Ui,
    context: &Context<'_>,
    row: &AdminRow,
    revoking: Option<Id>,
    ask: &mut Option<Id>,
    confirm: &mut Option<Id>,
) {
    let colors = palette(context.theme);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(widgets::elide(&row.username, 40))
                            .font(egui::FontId::proportional(font::BODY))
                            .color(colors.text),
                    );
                    ui.label(
                        RichText::new(format!(
                            "global admin \u{00B7} appointed {}",
                            crate::model::date(row.granted_at)
                        ))
                        .text_style(crate::theme::named(text_style::CAPTION))
                        .color(colors.text_muted),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let asking = revoking == Some(row.account_id);
                    let label = if asking {
                        format!("Confirm \u{2014} {}", row.username)
                    } else {
                        "Revoke".to_owned()
                    };
                    let button = widgets::ghost_button(ui, context.theme, &label);
                    let response = if asking {
                        button.on_hover_text("Click again to take the appointment away.")
                    } else {
                        button.on_hover_text(
                            "Takes global moderation away from this account. Clicks ask first.",
                        )
                    };
                    if response.clicked() {
                        if asking {
                            *confirm = Some(row.account_id);
                        } else {
                            *ask = Some(row.account_id);
                        }
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_pane_is_loading_not_closed() {
        // The pane asks on entry, so the honest first frame is "asked, not answered" — never
        // "not yours to open", which would claim a standing the pane has not checked.
        assert_eq!(AdminsState::default().answer, AdminsAnswer::Loading);
    }
}
