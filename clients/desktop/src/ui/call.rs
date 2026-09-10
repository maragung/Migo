//! The call overlay: the one call this device is in, drawn over everything.
//!
//! A call is not a window on the desktop — it is a foreground surface, anchored to the center
//! and drawn above the logout question's own foreground anchor is not (the logout gate still
//! closes the session, and the call goes with it). The shape follows the Android client's
//! `CallScreen`: peer, phase line, duration once media flows, and the two or three actions the
//! phase admits. Every action is a command — this screen owns no call state, it reads the
//! [`CallView`] the worker projected and hands intent back the same way every other screen does.
//!
//! Escape is the overlay's own reflex, handled here rather than by the shell: an incoming ring
//! declines, a live call ends, an ended call is dismissed. The shell's Escape handler (which
//! closes the top conversation window) stays quiet while the overlay is up, so the key cannot
//! do both at once.

use egui::{Align2, FontId, Order, RichText, Vec2};
use migo_core::Timestamp;

use crate::net::call::{CallPhase, CallView};
use crate::net::call_signal::{format_call_duration, media_kind_label};
use crate::net::Command;
use crate::theme::{font, palette, radius, space, Theme};
use crate::ui::widgets;

/// Draws the call overlay for the one call the worker says exists.
///
/// The peer's name is resolved by the caller (from the conversation cache, the same map the
/// chat header titles itself from) rather than looked up here, because this screen has no
/// conversation state of its own and the two surfaces should never disagree about who is who.
/// The id's tail stands in until a name arrives — the same fallback a chat window's title uses.
///
/// Commands are pushed into the shell's frame buffer rather than issued through a
/// [`crate::ui::Context`]: the overlay is drawn on the context, above every window, not inside
/// a screen's ui, so there is no context to hand it. The buffer is the same one the shell
/// drains after the frame.
pub fn overlay(
    ctx: &egui::Context,
    theme: Theme,
    view: &CallView,
    peer_name: &str,
    commands: &mut Vec<Command>,
) {
    let colors = palette(theme);

    // A call is a foreground question, so it carries no close (X) of its own: the phase's own
    // buttons — and Escape — are the whole answer set. Anchored rather than draggable for the
    // same reason the logout question is: a call that can be dragged away is a call that can
    // be forgotten while it is still running.
    egui::Window::new("Voice call")
        .id(egui::Id::new("migo-call-overlay"))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .order(Order::Foreground)
        .resizable(false)
        .collapsible(false)
        .min_width(340.0)
        .frame(
            egui::Frame::new()
                .fill(colors.surface)
                .stroke(egui::Stroke::new(1.0, colors.border))
                .corner_radius(egui::CornerRadius::same(radius::LG))
                .inner_margin(egui::Margin::same(space::MD as i8))
                .shadow(egui::Shadow {
                    offset: [0, 6],
                    blur: 24,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(120),
                }),
        )
        .show(ctx, |ui| {
            ui.set_width(340.0);
            ui.vertical_centered(|ui| {
                ui.add_space(space::SM);
                widgets::avatar(ui, theme, peer_name, 72.0);
                ui.add_space(space::SM);
                ui.label(
                    RichText::new(widgets::elide(peer_name, 24))
                        .font(FontId::proportional(font::TITLE))
                        .color(colors.text)
                        .strong(),
                );
                // The phase line: what is true of the call right now, in the caller's or the
                // callee's own words. A muted call says so under the phase, because a mute the
                // speaker cannot see is a mute the speaker argues about.
                let line = status_line(view);
                ui.label(
                    RichText::new(line)
                        .font(FontId::proportional(font::BODY))
                        .color(colors.text_muted),
                );
                if view.muted && view.phase != CallPhase::Ended {
                    ui.add_space(space::XS);
                    widgets::pill(ui, "Muted", colors.banner_ink, colors.banner_b);
                }
                ui.add_space(space::MD);

                // The actions the phase admits, and nothing more: a ring that can be answered,
                // a live call that can be muted and hung up, an ended call that can be closed.
                match view.phase {
                    CallPhase::Ringing if !view.outgoing => {
                        ui.horizontal_centered(|ui| {
                            if widgets::primary_button(ui, theme, "Accept", true).clicked() {
                                commands.push(Command::AcceptCall);
                            }
                            ui.add_space(space::SM);
                            if danger_button(ui, theme, "Decline").clicked() {
                                commands.push(Command::DeclineCall);
                            }
                        });
                    }
                    CallPhase::Ringing => {
                        if danger_button(ui, theme, "Cancel").clicked() {
                            commands.push(Command::EndCall);
                        }
                    }
                    CallPhase::Connecting | CallPhase::Connected | CallPhase::Reconnecting => {
                        ui.horizontal_centered(|ui| {
                            if widgets::ghost_button(
                                ui,
                                theme,
                                if view.muted { "Unmute" } else { "Mute" },
                            )
                            .clicked()
                            {
                                commands.push(Command::ToggleCallMute);
                            }
                            ui.add_space(space::SM);
                            if danger_button(ui, theme, "Hang up").clicked() {
                                commands.push(Command::EndCall);
                            }
                        });
                    }
                    CallPhase::Ended => {
                        if widgets::primary_button(ui, theme, "Close", true).clicked() {
                            commands.push(Command::DismissCall);
                        }
                    }
                }
                ui.add_space(space::SM);
            });
        });

    // Escape answers the overlay the way the phase reads: decline a ring, end a live call,
    // dismiss an ended one. The shell's own Escape reflex is held quiet while the overlay
    // exists, so the key does one thing per press.
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        match view.phase {
            CallPhase::Ringing if !view.outgoing => commands.push(Command::DeclineCall),
            CallPhase::Ringing
            | CallPhase::Connecting
            | CallPhase::Connected
            | CallPhase::Reconnecting => commands.push(Command::EndCall),
            CallPhase::Ended => commands.push(Command::DismissCall),
        }
    }
}

/// The line under the peer's name: the phase in words, or the duration once media flows.
///
/// The duration freezes at the ended timestamp rather than running on — an ended call that
/// kept ticking would be a clock lying about a call that is over. The reason line on an ended
/// call comes from the view (`line`), which the worker composed from the wire's end reason or
/// the invite status.
fn status_line(view: &CallView) -> String {
    match view.phase {
        CallPhase::Ringing if view.outgoing => "Calling…".to_owned(),
        CallPhase::Ringing => {
            format!("Incoming {}", media_kind_label(view.kind))
        }
        CallPhase::Connecting => "Connecting…".to_owned(),
        CallPhase::Connected => format_call_duration(elapsed_ms(view)),
        CallPhase::Reconnecting => "Reconnecting…".to_owned(),
        CallPhase::Ended => view.line.clone().unwrap_or_else(|| "Call ended".to_owned()),
    }
}

/// How long media has flowed, or flowed before the call ended — milliseconds since the
/// connection's own zero, frozen at the end when there is one.
fn elapsed_ms(view: &CallView) -> u64 {
    let Some(start) = view.started_at else {
        return 0;
    };
    let end = view.ended_at.unwrap_or_else(Timestamp::now);
    (end.as_unix_ms() - start.as_unix_ms()).max(0) as u64
}

/// The red action: decline, cancel, hang up. The palette's danger fill with the surface's own
/// ink, the same shape [`widgets::primary_button`] gives the orange one.
fn danger_button(ui: &mut egui::Ui, theme: Theme, text: &str) -> egui::Response {
    let colors = palette(theme);
    ui.scope(|ui| {
        {
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = colors.danger;
            w.inactive.bg_stroke = egui::Stroke::NONE;
            w.hovered.weak_bg_fill = colors.danger;
            w.hovered.bg_stroke = egui::Stroke::NONE;
            w.active.weak_bg_fill = colors.danger;
            w.active.bg_stroke = egui::Stroke::NONE;
        }
        let width = ui.available_width();
        ui.add(
            egui::Button::new(
                RichText::new(text)
                    .font(FontId::proportional(font::SUBTITLE))
                    .color(colors.text_on_accent),
            )
            .corner_radius(egui::CornerRadius::same(radius::MD))
            .min_size(Vec2::new(width.min(150.0), 40.0)),
        )
    })
    .inner
}
