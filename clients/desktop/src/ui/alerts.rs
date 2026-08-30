//! The Alerts place: the durable notification inbox and its read state.
//!
//! The live push stream is droppable by design, so this pane treats it only as a hint to re-read —
//! the rows are the source of truth, and they survive the recipient being offline. A row carries
//! no message content by construction (the server has no plaintext to put there); rendering is
//! kind, and time.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::model::AlertRow;
use crate::net::Command;
use crate::theme::{font, palette, space, Theme};
use crate::ui::{widgets, Context};

/// The place's state.
#[derive(Debug, Default)]
pub struct AlertsState {
    /// The inbox page, newest first as the server ordered it.
    pub items: Vec<AlertRow>,
    /// True once a read has landed.
    pub loaded: bool,
}

/// Draws the Alerts place.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut AlertsState) {
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::header(ui, context.theme, "Alerts", Some("Your notification inbox"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui
                .add_enabled(!state.items.is_empty(), egui::Button::new("Mark all read"))
                .clicked()
            {
                if let Some(newest) = state.items.iter().map(|item| item.at.as_unix_ms()).max() {
                    context.issue(Command::AcknowledgeAlerts {
                        through_unix_ms: newest,
                    });
                }
            }
            if ui.button("Refresh").clicked() {
                context.issue(Command::Notifications);
            }
        });
    });
    ui.add_space(space::SM);
    widgets::divider(ui, context.theme);

    if state.items.is_empty() {
        widgets::empty_state(
            ui,
            context.theme,
            if state.loaded {
                "You are all caught up"
            } else {
                "Loading…"
            },
            if state.loaded {
                "A mention, a friend request, a gift — it will land here."
            } else {
                ""
            },
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(space::XS);
            for alert in &state.items {
                alert_row(ui, context.theme, alert);
            }
            ui.add_space(space::SM);
            ui.add_space(space::MD);
            ui.label(
                RichText::new(
                    "Notifications carry no message text: the server never has any to show.",
                )
                .font(FontId::proportional(font::TINY))
                .color(palette(context.theme).text_muted),
            );
            ui.add_space(space::XL);
        });
}

/// One inbox row: the headline (the kind's own words, or the server's title) and the time.
fn alert_row(ui: &mut Ui, theme: Theme, alert: &AlertRow) {
    let colors = palette(theme);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::hover());
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(space::MD, space::XS * 0.5)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    inner.label(
        RichText::new(widgets::elide(
            &alert
                .title
                .clone()
                .unwrap_or_else(|| crate::model::spaced_words(&alert.kind)),
            60,
        ))
        .font(FontId::proportional(font::BODY))
        .color(colors.text),
    );
    inner.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(
            RichText::new(crate::model::clock(alert.at))
                .font(FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
    });
}
