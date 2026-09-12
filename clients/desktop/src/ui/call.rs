//! The call overlay: the one call this device is in, drawn over everything.
//!
//! A call is not a window on the desktop — it is a foreground surface, anchored to the center
//! and drawn above the logout question's own foreground anchor is not (the logout gate still
//! closes the session, and the call goes with it). The shape follows the Android client's
//! `CallScreen`: peer, phase line, duration once media flows, and the two or three actions the
//! phase admits. Every action is a command — this screen owns no call state, it reads the
//! [`CallView`] the worker projected and hands intent back the same way every other screen does.
//!
//! A video call carries a stage: the remote's decoded picture, drawn where the avatar sits
//! while audio-only, hidden again when the stream stops or the call ends. The frames arrive
//! on the worker's video pump, parked in the shared slot the view carries; this screen polls
//! the slot every frame and re-uploads the newest picture — the desktop's answer to the web
//! overlay's `<video>` element, minus the self-view (this build sends no camera of its own).
//!
//! Escape is the overlay's own reflex, handled here rather than by the shell: an incoming ring
//! declines, a live call ends, an ended call is dismissed. The shell's Escape handler (which
//! closes the top conversation window) stays quiet while the overlay is up, so the key cannot
//! do both at once.

use egui::{Align2, FontId, Order, RichText, Vec2};
use migo_core::Timestamp;

use crate::net::call::{CallPhase, CallView};
use crate::net::call_signal::{format_call_duration, media_kind_label};
use crate::net::call_video::VideoFrame;
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
    // be forgotten while it is still running. The title names the kind the call carries — a
    // video invite that answers as audio-and-their-picture is still honest about being video.
    egui::Window::new(media_kind_label(view.kind))
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
                // The video stage replaces the avatar while the remote's picture flows — the
                // same trade the web overlay makes (`showVideos` hides the avatar block) — and
                // falls back to the avatar for audio calls, for the phases before media, and
                // for a video call whose stream has not produced a frame yet.
                let show_video =
                    matches!(view.phase, CallPhase::Connected | CallPhase::Reconnecting)
                        && view.video.is_some();
                let mut video_shown = false;
                if show_video {
                    if let Some(frame) = latest_frame(&view.video) {
                        draw_video_stage(ui, theme, &frame);
                        // The picture is live media: the next frame is a fraction of a second
                        // away, and a repaint that waits for input would hold the last frame
                        // until the mouse moves.
                        ui.ctx().request_repaint();
                        video_shown = true;
                    }
                }
                if !video_shown {
                    widgets::avatar(ui, theme, peer_name, 72.0);
                }
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

/// The newest decoded frame of the remote's video, cloned out of the shared slot. A brief
/// lock per frame — the pump holds it only for its own swap, so this never waits on a decode.
fn latest_frame(slot: &Option<crate::net::call_video::VideoSlot>) -> Option<VideoFrame> {
    let slot = slot.as_ref()?;
    let guard = slot.lock().ok()?;
    guard.clone()
}

/// Draws one frame of the remote's video: a persistent texture re-uploaded per frame,
/// letterboxed to a stage the overlay's width can hold. Aspect is the frame's own — a sender
/// in portrait must not be stretched to fill a landscape box — and the cap bounds both the
/// stage and the upload's cost on a frame rate this screen never asked to hit.
fn draw_video_stage(ui: &mut egui::Ui, theme: Theme, frame: &VideoFrame) {
    let colors = palette(theme);
    // One persistent texture, re-uploaded per frame. It lives in the context's memory under a
    // fixed id rather than this screen's state, the same way egui keeps its own widgets'
    // transient state: the overlay is a function of the view, and a texture is exactly the
    // kind of thing the doc for that memory says to wrap and keep out of the data.
    let mut texture = ui.ctx().memory_mut(|memory| {
        memory
            .data
            .get_temp_mut_or_insert_with(egui::Id::new("migo-call-video"), || {
                ui.ctx().load_texture(
                    "migo-call-video",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                )
            })
            .clone()
    });
    texture.set(
        egui::ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.rgba,
        ),
        egui::TextureOptions::LINEAR,
    );

    // The stage: the overlay's width minus its margins, capped at 4:3-ish height so a tall
    // frame cannot push the actions off the screen. The frame is fitted inside at its own
    // aspect — the letterbox is the stage's fill, not the picture's stretch.
    let stage_width = 340.0 - 2.0 * space::MD;
    let stage_height = (stage_width * 3.0 / 4.0).min(300.0);
    // The frame's own pixels, as floats for the fit below. `as` rather than `From` because
    // the standard library deliberately refuses a lossless `From<u32> for f32`.
    let (fw, fh) = (frame.width.max(1) as f32, frame.height.max(1) as f32);
    let scale = (stage_width / fw).min(stage_height / fh);
    let (w, h) = (fw * scale, fh * scale);

    // The letterbox is the stage's fill, not the picture's stretch: the frame sits centered
    // at its own aspect, and the stage's margins are what the phase line and actions keep.
    egui::Frame::new()
        .fill(colors.surface)
        .corner_radius(egui::CornerRadius::same(radius::MD))
        .inner_margin(egui::Margin::same(space::XS as i8))
        .show(ui, |ui| {
            ui.set_width(stage_width);
            ui.set_height(stage_height);
            ui.centered_and_justified(|ui| {
                ui.image((texture.id(), egui::vec2(w, h)));
            });
        });
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
