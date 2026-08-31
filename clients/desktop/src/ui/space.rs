//! The Feed place: the activity stream.
//!
//! The stream is the account's own activity — the notification inbox (durable, server-ordered)
//! and the wallet's statement (gifts, stakes, payouts) merged newest first, with a category filter
//! over the merged rows. The rows themselves are always the durable record: a pushed notification
//! is the cue to re-read, never a row, because the push is droppable by design.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::model::{ActivityCategory, ActivityRow, AlertRow, LedgerRow};
use crate::net::Command;
use crate::theme::{font, palette, space, Theme};
use crate::ui::{widgets, Context};

/// The place's state: the filter and the merged rows.
#[derive(Debug, Default)]
pub struct SpaceState {
    /// Which category the filter shows; `None` is all of them.
    pub filter: Option<ActivityCategory>,
}

/// Draws the Feed place — the reference's Feed tab, carrying the activity stream this client
/// has always called Space.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut SpaceState, rows: &[ActivityRow]) {
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::header(
            ui,
            context.theme,
            "Feed",
            Some("Your activity, newest first"),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui.button("Refresh").clicked() {
                context.issue(Command::Notifications);
                context.issue(Command::Wallet);
            }
        });
    });
    ui.add_space(space::SM);

    // The filter row: one chip per category, plus the "all" chip.
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        if chip(ui, context.theme, "All", state.filter.is_none()) {
            state.filter = None;
        }
        for category in [
            ActivityCategory::Social,
            ActivityCategory::Rooms,
            ActivityCategory::Games,
            ActivityCategory::Economy,
        ] {
            if chip(
                ui,
                context.theme,
                category.label(),
                state.filter == Some(category),
            ) {
                state.filter = Some(category);
            }
        }
    });
    ui.add_space(space::SM);
    widgets::divider(ui, context.theme);

    let filtered: Vec<&ActivityRow> = rows
        .iter()
        .filter(|row| state.filter.is_none_or(|filter| filter == row.category))
        .collect();
    if filtered.is_empty() {
        widgets::empty_state(
            ui,
            context.theme,
            if rows.is_empty() {
                "No activity yet"
            } else {
                "Nothing in this category"
            },
            "Your stream fills as your friends, rooms, and games move.",
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(space::XS);
            for row in filtered {
                activity_row(ui, context.theme, row);
            }
            ui.add_space(space::XL);
        });
}

/// One stream row: the headline, the category, and the age.
///
/// The key names the row for egui's identity machinery — two rows with one key are one row, which
/// is exactly the promise [`rebuild`]'s dedupe makes on the data side.
fn activity_row(ui: &mut Ui, theme: Theme, row: &ActivityRow) {
    let colors = palette(theme);
    let width = ui.available_width();
    let id = egui::Id::new(&row.key);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::hover());
    let _ = (response, id);
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(space::MD, space::XS * 0.5)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    // The category glyph: a small tinted dot rather than an icon font — one hue per category, the
    // same assignment the label carries.
    let hue = match row.category {
        ActivityCategory::Social => colors.accent,
        ActivityCategory::Rooms => colors.positive,
        ActivityCategory::Games => colors.warning,
        ActivityCategory::Economy => colors.verified,
    };
    let (dot, _) = inner.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    inner.painter().circle_filled(dot.center(), 4.0, hue);
    inner.add_space(space::SM);
    inner.label(
        RichText::new(widgets::elide(&row.title, 64))
            .font(FontId::proportional(font::BODY))
            .color(colors.text),
    );
    inner.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(
            RichText::new(crate::model::clock(row.at))
                .font(FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
        ui.label(
            RichText::new(row.category.label())
                .font(FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
    });
}

/// A filter chip: marked while it is the held filter.
fn chip(ui: &mut Ui, theme: Theme, label: &str, selected: bool) -> bool {
    let colors = palette(theme);
    let response = ui.add(
        egui::Button::new(
            RichText::new(label)
                .font(FontId::proportional(font::SMALL))
                .color(if selected {
                    colors.accent
                } else {
                    colors.text_muted
                }),
        )
        .fill(if selected {
            colors.surface_selected
        } else {
            egui::Color32::TRANSPARENT
        }),
    );
    response.clicked()
}

/// Rebuilds the merged stream from its durable halves: the inbox and the statement.
///
/// A notification and a ledger line can describe the same gift — the wire gives them different ids
/// and different words, so both stand: one is the social fact, one is the money fact. The key is
/// the row's stable identity, so a re-read's duplicate rows are dropped rather than drawn twice.
/// Newest first, capped at a screen's worth, because a stream grows downward, not forever.
pub fn rebuild(alerts: &[AlertRow], ledger: &[LedgerRow]) -> Vec<ActivityRow> {
    let mut rows: Vec<ActivityRow> = Vec::with_capacity(alerts.len() + ledger.len());
    let mut seen = std::collections::HashSet::new();
    for alert in alerts {
        let key = format!("alert-{}", alert.id);
        if !seen.insert(key.clone()) {
            continue;
        }
        rows.push(ActivityRow {
            key,
            category: alert_category(&alert.kind),
            title: alert
                .title
                .clone()
                .unwrap_or_else(|| crate::model::spaced_words(&alert.kind)),
            at: alert.at,
        });
    }
    for entry in ledger {
        let key = format!("ledger-{}-{}", entry.at.as_unix_ms(), entry.reason);
        if !seen.insert(key.clone()) {
            continue;
        }
        let sign = if entry.credit { "+" } else { "-" };
        rows.push(ActivityRow {
            key,
            category: ActivityCategory::Economy,
            title: format!(
                "{} {}{} MIG",
                crate::model::spaced_words(&entry.reason),
                sign,
                entry.amount
            ),
            at: entry.at,
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.at.as_unix_ms()));
    rows.truncate(80);
    rows
}

/// The category an inbox kind belongs to, from the closed server vocabulary.
fn alert_category(kind: &str) -> ActivityCategory {
    if kind.contains("friend") {
        ActivityCategory::Social
    } else if kind.contains("gift") || kind.contains("coin") || kind.contains("ledger") {
        ActivityCategory::Economy
    } else if kind.contains("game") {
        ActivityCategory::Games
    } else if kind.contains("room") {
        ActivityCategory::Rooms
    } else {
        ActivityCategory::Social
    }
}
