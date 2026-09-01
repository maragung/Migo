//! The Wallet place: the MIG balance, the gift shop, the statement, progression, badges, and
//! the leaderboard — the caller's whole economy under one address.
//!
//! The coin is MIG. The balance leads; the statement states each line's signed amount from its
//! reason (the wire's amount is a magnitude); the shop states its prices before its recipients,
//! so the spend is agreed before the address is.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::model::{
    avax, navax, ChainNetwork, ChainTxRow, GiftRow, LeaderRow, LedgerRow, PreparedTx, Progression,
};
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
    /// The AVAX side: the account's first wallet on one network at a time.
    pub chain: ChainWallet,
}

/// The AVAX wallet surface's state (§184).
#[derive(Debug, Default)]
pub struct ChainWallet {
    /// The network the surface is on. Mainnet by default — the brief's default is for *display*,
    /// and the first send on mainnet says what mainnet means before the button unlocks.
    pub network: ChainNetwork,
    /// The wallet's EIP-55 address, once a read discovered it. `None` until then, and `None`
    /// forever on a device without the root — the read's error carries that sentence instead.
    pub address: Option<String>,
    /// The balance in wei, after the last refresh the user asked for.
    pub balance: Option<u128>,
    /// Why the last refresh could not answer. Stays on screen: "could not check" and "zero" are
    /// different facts, and only one of them should reassure anybody.
    pub error: Option<String>,
    /// Whether the send form is open.
    pub send_open: bool,
    /// The typed recipient, EIP-55 checked by the worker, not here.
    pub recipient: String,
    /// The typed amount, in AVAX.
    pub amount: String,
    /// The built transaction awaiting its confirmation, exactly as it was displayed.
    pub prepared: Option<PreparedTx>,
    /// Why nothing could be built. A refusal worth reading, from the one place that parsed.
    pub prepare_error: Option<String>,
    /// The acknowledgement on the first mainnet send: real money, said once, before the button
    /// that spends it unlocks.
    pub mainnet_acknowledged: bool,
    /// The in-flight send, from acceptance to its ending: the hash and the state ladder.
    pub tracking: Option<TrackingTx>,
    /// Why a broadcast was refused.
    pub send_error: Option<String>,
    /// Whether the Receive window is open.
    pub receive_open: bool,
    /// This account's tracked transactions, newest first.
    pub activity: Vec<ChainTxRow>,
}

/// One in-flight send as the surface shows it: the hash the user can find in any explorer, and
/// spec #41's own word for where the transaction stands.
#[derive(Debug, Clone)]
pub struct TrackingTx {
    pub tx_hash: String,
    pub state: String,
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

            // The AVAX side: one network at a time, balance by explicit refresh, and a send that
            // runs as one screen — form, full transaction, confirmation, honest tracking.
            ui.add_space(space::LG);
            widgets::subheader(ui, context.theme, "AVAX ON AVALANCHE");
            chain_panel(ui, context, &mut state.chain);

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

/// The AVAX panel: network, address, balance by refresh, the send and receive doors, tracking,
/// and the tracked-transaction list. One network at a time because one transaction is signed for
/// one chain, and a surface that mixed them would be the confusion §44 exists to prevent.
fn chain_panel(ui: &mut Ui, context: &mut Context<'_>, chain: &mut ChainWallet) {
    let colors = palette(context.theme);

    // The network: two names, no URLs. Switching clears what the other network's RPC said,
    // because a balance from one chain is a lie beside another's name.
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        for (option, label) in [
            (ChainNetwork::Mainnet, "Mainnet"),
            (ChainNetwork::Fuji, "Fuji (testnet)"),
        ] {
            if ui
                .selectable_label(chain.network == option, label)
                .clicked()
                && chain.network != option
            {
                chain.network = option;
                chain.balance = None;
                chain.error = None;
                chain.prepared = None;
                chain.prepare_error = None;
                chain.send_error = None;
            }
        }
    });

    // The address: EIP-55, the form a person can check a character of, with a copy button
    // because nobody retypes forty-two characters without introducing a typo.
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        ui.label(
            RichText::new(
                chain
                    .address
                    .clone()
                    .unwrap_or_else(|| "wallet 0's address appears after a refresh".to_owned()),
            )
            .font(FontId::monospace(font::SMALL))
            .color(colors.text),
        );
        if let Some(address) = chain.address.clone() {
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(address);
            }
        }
    });

    // The balance: a pull, never a poll. The refresh is a button because a balance that moves on
    // its own is a balance the surface is polling behind the user's back.
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        let shown = chain
            .balance
            .map(|wei| format!("{} AVAX", avax(wei)))
            .unwrap_or_else(|| "—".to_owned());
        ui.label(
            RichText::new(shown)
                .font(FontId::proportional(font::SUBTITLE))
                .color(colors.text)
                .strong(),
        );
        if ui.button("Refresh").clicked() {
            chain.balance = None;
            chain.error = None;
            context.issue(Command::ChainBalance {
                network: chain.network,
            });
        }
        ui.add_space(space::SM);
        if ui.button("Receive").clicked() {
            chain.receive_open = true;
        }
        ui.add_space(space::SM);
        if ui.button("Send AVAX").clicked() {
            chain.send_open = true;
        }
    });
    if let Some(error) = chain.error.clone() {
        ui.horizontal(|ui| {
            ui.add_space(space::MD);
            ui.label(
                RichText::new(error)
                    .font(FontId::proportional(font::SMALL))
                    .color(colors.danger),
            );
        });
    }

    // The in-flight send, said plainly: acceptance is not confirmation, and the ladder is
    // spec #41's own words.
    if let Some(tracking) = chain.tracking.clone() {
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            ui.add_space(space::MD);
            let confirmed = tracking.state == "CONFIRMED";
            ui.label(
                RichText::new(format!("{} · {}", tracking.state, tracking.tx_hash))
                    .font(FontId::proportional(font::SMALL))
                    .color(if confirmed {
                        colors.positive
                    } else {
                        colors.text_muted
                    }),
            );
        });
    }

    // The tracked transactions: what was sent, to whom, for what ceiling of fee, and where the
    // tracker left it — newest first, like every other list on this surface.
    if !chain.activity.is_empty() {
        ui.add_space(space::MD);
        ui.horizontal(|ui| {
            ui.add_space(space::MD);
            ui.label(
                RichText::new("TRACKED TRANSACTIONS")
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
        });
        for row in &chain.activity {
            chain_line(ui, context.theme, row);
        }
    }

    chain_receive_window(ui, context.theme, chain);
    chain_send_window(ui, context, chain);
}

/// One tracked transaction, in the order a person checks it: what happened to it, for how much,
/// to whom, on which chain, and — once the receipt answered — in which block for how much gas.
fn chain_line(ui: &mut Ui, theme: Theme, row: &ChainTxRow) {
    let colors = palette(theme);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        let (word, tone) = match row.outcome.as_str() {
            "CONFIRMED" => ("confirmed", colors.positive),
            "REVERTED" => ("reverted", colors.danger),
            "DROPPED" => ("dropped", colors.danger),
            "EXPIRED" => ("expired", colors.warning),
            _ => (row.outcome.as_str(), colors.text_muted),
        };
        ui.label(
            RichText::new(format!("-{} AVAX", avax(row.value_wei)))
                .font(FontId::proportional(font::BODY))
                .color(colors.text),
        );
        ui.label(
            RichText::new(format!("to {}…", &row.to[..10.min(row.to.len())]))
                .font(FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        ui.label(
            RichText::new(&row.network)
                .font(FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            ui.label(
                RichText::new(crate::model::clock(row.at))
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
            // The fee reads as a ceiling until the receipt replaces it with the gas actually
            // spent — a confirmed spend should never overstate what it cost.
            let fee = match (row.gas_used, row.block) {
                (Some(gas), Some(_)) => format!("fee {gas} gas"),
                _ => format!("fee ≤ {} nAVAX", navax(row.fee_wei)),
            };
            ui.label(
                RichText::new(fee)
                    .font(FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
            if let Some(block) = row.block {
                ui.label(
                    RichText::new(format!("block {block}"))
                        .font(FontId::proportional(font::TINY))
                        .color(colors.text_muted),
                );
                ui.add_space(space::SM);
            }
            // The hash the user can find in any explorer — shortened here, whole in the copy
            // that matters: a click copies the full `0x…` string.
            let hash = row.tx_hash.clone();
            let short = format!("{}…", &hash[..14.min(hash.len())]);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(short)
                            .font(FontId::monospace(font::TINY))
                            .color(colors.text_muted),
                    )
                    .small(),
                )
                .on_hover_text("Copy the transaction hash")
                .clicked()
            {
                ui.ctx().copy_text(hash);
            }
            ui.add_space(space::SM);
            ui.label(
                RichText::new(word)
                    .font(FontId::proportional(font::SMALL))
                    .color(tone)
                    .strong(),
            );
        });
    });
}

/// The Receive window: the address, and nothing else. Receiving AVAX has no flow to run — the
/// address is the whole of it — so the window is a copy target and a closing truth.
fn chain_receive_window(ui: &mut Ui, theme: Theme, chain: &mut ChainWallet) {
    let colors = palette(theme);
    if chain.receive_open {
        let mut close = false;
        egui::Window::new("Receive AVAX")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                let address = chain.address.clone().unwrap_or_default();
                ui.label(
                    RichText::new(format!(
                        "Your address on {} — the same address on every EVM network.",
                        chain.network.label()
                    ))
                    .font(FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
                );
                ui.add_space(space::SM);
                ui.label(
                    RichText::new(&address)
                        .font(FontId::monospace(font::BODY))
                        .color(colors.text),
                );
                ui.add_space(space::SM);
                ui.horizontal(|ui| {
                    if !address.is_empty() && ui.button("Copy").clicked() {
                        ui.ctx().copy_text(address.clone());
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            chain.receive_open = false;
        }
    }
    let _ = theme;
}

/// The Send window: one screen, run in the order spec #40 fixes — form, full transaction,
/// confirmation, and only then a signature. The confirm button sends back exactly the prepared
/// struct it is displaying, and the worker re-derives every field from it, so what is signed is
/// what was shown.
fn chain_send_window(ui: &mut Ui, context: &mut Context<'_>, chain: &mut ChainWallet) {
    let colors = palette(context.theme);
    if !chain.send_open {
        return;
    }
    let mut close = false;
    egui::Window::new(format!("Send AVAX — {}", chain.network.label()))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            if chain.tracking.is_some() {
                // A send is in flight: no second send is composed on top of it, because two
                // sends composed blind is how a nonce replaces its sibling.
                ui.label(
                    RichText::new("A send is already being tracked; wait for its ending.")
                        .font(FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
                if ui.button("Close").clicked() {
                    close = true;
                }
                return;
            }

            let Some(tx) = chain.prepared.clone() else {
                // The form: recipient and amount, nothing else. The estimate and the confirmation
                // follow, so the form never previews a number the chain has not been asked for.
                ui.label(
                    RichText::new("Recipient (EIP-55 checked)")
                        .font(FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut chain.recipient)
                        .hint_text("0x…")
                        .desired_width(360.0)
                        .font(egui::TextStyle::Monospace),
                );
                ui.add_space(space::SM);
                ui.label(
                    RichText::new("Amount (AVAX)")
                        .font(FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut chain.amount)
                        .hint_text("1.5")
                        .desired_width(360.0),
                );
                if let Some(error) = chain.prepare_error.clone() {
                    ui.add_space(space::SM);
                    ui.label(
                        RichText::new(error)
                            .font(FontId::proportional(font::SMALL))
                            .color(colors.danger),
                    );
                }
                if let Some(error) = chain.send_error.clone() {
                    ui.add_space(space::SM);
                    ui.label(
                        RichText::new(error)
                            .font(FontId::proportional(font::SMALL))
                            .color(colors.danger),
                    );
                }
                ui.add_space(space::SM);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let ready =
                        !chain.recipient.trim().is_empty() && !chain.amount.trim().is_empty();
                    if ui
                        .add_enabled(ready, egui::Button::new("Build the transaction"))
                        .clicked()
                    {
                        chain.prepare_error = None;
                        chain.send_error = None;
                        let (network, recipient, amount) =
                            (chain.network, chain.recipient.clone(), chain.amount.clone());
                        context.issue(Command::ChainPrepare {
                            network,
                            recipient,
                            amount_avax: amount,
                        });
                    }
                });
                return;
            };

            // The full transaction, every line the signature will cover: from, to, value, fee,
            // gas, nonce, chain, network (spec #40 — "Sign data?" alone is never all a user sees).
            egui::Grid::new("prepared-transaction")
                .num_columns(2)
                .spacing([space::LG, space::XS])
                .show(ui, |ui| {
                    prepared_line(ui, context.theme, "From", &tx.from, true);
                    prepared_line(ui, context.theme, "To", &tx.to, true);
                    prepared_line(
                        ui,
                        context.theme,
                        "Amount",
                        &format!("{} AVAX", avax(tx.value_wei)),
                        false,
                    );
                    prepared_line(
                        ui,
                        context.theme,
                        "Max fee",
                        &format!(
                            "{} nAVAX (≤ {} per gas × {})",
                            navax(tx.max_fee_per_gas * tx.gas_limit as u128),
                            navax(tx.max_fee_per_gas),
                            tx.gas_limit
                        ),
                        false,
                    );
                    prepared_line(
                        ui,
                        context.theme,
                        "Max priority fee",
                        &format!("{} nAVAX per gas", navax(tx.max_priority_fee_per_gas)),
                        false,
                    );
                    prepared_line(ui, context.theme, "Nonce", &tx.nonce.to_string(), false);
                    prepared_line(
                        ui,
                        context.theme,
                        "Chain",
                        &format!(
                            "{} (id {})",
                            tx.network.label(),
                            tx.network.network().chain_id
                        ),
                        false,
                    );
                });

            // Mainnet is real money, and the first send on it says so before the button unlocks.
            let mut acknowledged = chain.mainnet_acknowledged;
            if tx.network == ChainNetwork::Mainnet {
                ui.add_space(space::SM);
                ui.checkbox(
                    &mut acknowledged,
                    "This is mainnet AVAX — real money, sent to the address above, not reversible.",
                );
                chain.mainnet_acknowledged = acknowledged;
            }
            if let Some(error) = chain.send_error.clone() {
                ui.add_space(space::SM);
                ui.label(
                    RichText::new(error)
                        .font(FontId::proportional(font::SMALL))
                        .color(colors.danger),
                );
            }
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                if ui.button("Back").clicked() {
                    chain.prepared = None;
                    chain.prepare_error = None;
                }
                let confirmed = tx.network != ChainNetwork::Mainnet || chain.mainnet_acknowledged;
                if ui
                    .add_enabled(confirmed, egui::Button::new("Confirm and send"))
                    .clicked()
                {
                    chain.send_error = None;
                    context.issue(Command::ChainSend { tx });
                }
            });
        });
    if close {
        chain.send_open = false;
        chain.prepared = None;
        chain.prepare_error = None;
        chain.send_error = None;
        chain.mainnet_acknowledged = false;
    }
}

/// One line of the prepared-transaction display: label, value, and monospace for the addresses a
/// person might check character by character.
fn prepared_line(ui: &mut Ui, theme: Theme, label: &str, value: &str, monospace: bool) {
    let colors = palette(theme);
    ui.label(
        RichText::new(label)
            .font(FontId::proportional(font::SMALL))
            .color(colors.text_muted),
    );
    ui.label(
        RichText::new(value.to_owned())
            .font(if monospace {
                FontId::monospace(font::SMALL)
            } else {
                FontId::proportional(font::BODY)
            })
            .color(colors.text),
    );
    ui.end_row();
}
