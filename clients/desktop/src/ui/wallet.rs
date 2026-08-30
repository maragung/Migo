//! The Wallet place: the MIG balance, the gift shop, the statement, progression, badges, and
//! the leaderboard — the caller's whole economy under one address.
//!
//! The coin is MIG. The balance leads; the statement states each line's signed amount from its
//! reason (the wire's amount is a magnitude); the shop states its prices before its recipients,
//! so the spend is agreed before the address is.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::model::{GiftRow, LeaderRow, LedgerRow, Progression};
use crate::net::Command;
use crate::theme::{font, palette, space, Theme};
use crate::ui::{widgets, Context};

/// The place's state.
#[derive(Debug, Default)]
pub struct WalletState {
    pub coins: Option<u64>,
    pub points: Option<u64>,
    pub ledger: Vec<LedgerRow>,
    pub progression: Option<Progression>,
    pub badges: Vec<String>,
    pub leaders: Vec<LeaderRow>,
    pub gifts: Vec<GiftRow>,
    /// The gift being addressed, if any: the picker is open.
    pub picking: Option<GiftRow>,
    /// The typed recipient of the picked gift.
    pub recipient: String,
}

/// Draws the Wallet place.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut WalletState) {
    let colors = palette(context.theme);
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::header(
            ui,
            context.theme,
            "Wallet",
            Some("Your Migo coins, gifts, and standing"),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui.button("Refresh").clicked() {
                context.issue(Command::Wallet);
            }
        });
    });
    ui.add_space(space::SM);
    widgets::divider(ui, context.theme);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The balance: the two facts, coins first, side by side.
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                ui.add_space(space::MD);
                fact_card(
                    ui,
                    context.theme,
                    "MIG COINS",
                    &state
                        .coins
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "…".into()),
                    true,
                );
                ui.add_space(space::SM);
                fact_card(
                    ui,
                    context.theme,
                    "POINTS",
                    &state
                        .points
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "…".into()),
                    false,
                );
            });

            // Progression: the level and the bar behind it.
            if let Some(progression) = state.progression {
                ui.add_space(space::LG);
                widgets::subheader(ui, context.theme, &format!("LEVEL {}", progression.level));
                let width = ui.available_width() - space::MD * 2.0;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(crate::theme::radius::FULL),
                    colors.surface_hover,
                );
                let filled = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(width * progression.fraction(), rect.height()),
                );
                ui.painter().rect_filled(
                    filled,
                    egui::CornerRadius::same(crate::theme::radius::FULL),
                    colors.accent,
                );
                ui.label(
                    RichText::new(format!(
                        "{} / {} XP",
                        progression.xp_into_level, progression.xp_for_next_level
                    ))
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
                );
            }

            // Badges: the honours, one chip each.
            if !state.badges.is_empty() {
                ui.add_space(space::LG);
                widgets::subheader(ui, context.theme, "BADGES");
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(space::MD);
                    for badge in &state.badges {
                        widgets::pill(
                            ui,
                            &crate::model::spaced_words(badge),
                            colors.text,
                            colors.surface_hover,
                        );
                        ui.add_space(space::XS);
                    }
                });
            }

            // The gift shop: price stated before recipient, per the place's own rule.
            if !state.gifts.is_empty() {
                ui.add_space(space::LG);
                widgets::subheader(ui, context.theme, "SEND A GIFT");
                for gift in state.gifts.clone() {
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD);
                        ui.label(
                            RichText::new(format!(
                                "{} · {} MIG · {}",
                                gift.name, gift.price, gift.category
                            ))
                            .font(FontId::proportional(font::BODY))
                            .color(colors.text),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(space::MD);
                            if ui.button("Send").clicked() {
                                state.picking = Some(gift);
                                state.recipient.clear();
                            }
                        });
                    });
                }
            }

            // The statement: one line per transaction, newest first.
            if !state.ledger.is_empty() {
                ui.add_space(space::LG);
                widgets::subheader(ui, context.theme, "RECENT ACTIVITY");
                for entry in &state.ledger {
                    ledger_line(ui, context.theme, entry);
                }
            }

            // The leaderboard.
            if !state.leaders.is_empty() {
                ui.add_space(space::LG);
                widgets::subheader(ui, context.theme, "LEADERBOARD");
                for leader in &state.leaders {
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD);
                        ui.label(
                            RichText::new(format!("#{}", leader.position))
                                .font(FontId::proportional(font::SMALL))
                                .color(colors.text_muted),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} · Level {} · {} XP",
                                crate::model::short_id(leader.account_id),
                                leader.level,
                                leader.xp
                            ))
                            .font(FontId::proportional(font::SMALL))
                            .color(colors.text),
                        );
                    });
                }
            }

            ui.add_space(space::XL);
        });

    // The recipient flow: the picked gift, its price restated, and the address.
    if let Some(gift) = state.picking.clone() {
        let mut close = false;
        let mut send = false;
        egui::Window::new(format!("Send {}", gift.name))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.label(
                    RichText::new(format!("{} MIG", gift.price))
                        .font(FontId::proportional(font::SUBTITLE))
                        .color(colors.accent),
                );
                ui.add_space(space::SM);
                ui.label(
                    RichText::new("The recipient's account id:")
                        .font(FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut state.recipient)
                        .hint_text("account id")
                        .desired_width(320.0),
                );
                ui.add_space(space::SM);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let parsed = migo_core::Id::parse(state.recipient.trim()).ok();
                    if ui
                        .add_enabled(parsed.is_some(), egui::Button::new("Send"))
                        .clicked()
                    {
                        if let Some(peer) = parsed {
                            context.issue(Command::SendGift {
                                sku: gift.sku.clone(),
                                recipient: peer,
                            });
                            send = true;
                        }
                    }
                });
            });
        if close || send {
            state.picking = None;
            state.recipient.clear();
        }
    }
}

/// One of the balance's fact cards.
fn fact_card(ui: &mut Ui, theme: Theme, label: &str, value: &str, emphasise: bool) {
    let colors = palette(theme);
    egui::Frame::new()
        .fill(if emphasise {
            colors.surface_selected
        } else {
            colors.surface_hover
        })
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::LG))
        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(180.0);
            ui.label(
                RichText::new(label)
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
            ui.label(
                RichText::new(value)
                    .font(FontId::proportional(font::DISPLAY))
                    .color(if emphasise {
                        colors.accent
                    } else {
                        colors.text
                    })
                    .strong(),
            );
        });
}

/// One statement line: reason, signed amount, the balance after, and when.
fn ledger_line(ui: &mut Ui, theme: Theme, entry: &LedgerRow) {
    let colors = palette(theme);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        ui.label(
            RichText::new(crate::model::spaced_words(&entry.reason))
                .font(FontId::proportional(font::BODY))
                .color(colors.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            ui.label(
                RichText::new(crate::model::clock(entry.at))
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
            ui.label(
                RichText::new(format!("balance {}", entry.balance_after))
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
            ui.add_space(space::SM);
            let signed = if entry.credit {
                format!("+{}", entry.amount)
            } else {
                format!("-{}", entry.amount)
            };
            ui.label(
                RichText::new(signed)
                    .font(FontId::proportional(font::SUBTITLE))
                    .color(if entry.credit {
                        colors.positive
                    } else {
                        colors.text
                    })
                    .strong(),
            );
        });
    });
}
