//! The shared visual vocabulary: every recurring shape lives here once.
//!
//! The point is not deduplication for its own sake. A chat window has perhaps a dozen distinct
//! visual ideas — a bubble, a row in a list, a pill, a field, a button — and each appears in several
//! places. If each place draws its own, the twelve ideas quietly become thirty, and a change to the
//! bubble corner radius becomes a hunt. Defining them once means the window looks designed rather
//! than assembled, and it means [`crate::theme`] is genuinely the only place a colour is chosen.
//!
//! Nothing in this module reads application state. Each function takes exactly what it draws, so any
//! of them can be dropped into a new screen without dragging a context along.

use egui::{
    Align, Color32, CornerRadius, FontId, Layout, Response, RichText, Sense, Stroke, Ui, Vec2,
};

use crate::theme::{font, palette, radius, space, text_style, Theme};

/// A heading with optional secondary text beneath it.
pub fn header(ui: &mut Ui, theme: Theme, title: &str, subtitle: Option<&str>) {
    let colors = palette(theme);
    ui.label(
        RichText::new(title)
            .font(FontId::proportional(font::TITLE))
            .color(colors.text),
    );
    if let Some(subtitle) = subtitle {
        ui.add_space(space::XS);
        ui.label(
            RichText::new(subtitle)
                .font(FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
    }
}

/// A small label above one group of related settings.
///
/// Quieter than [`header`] on purpose: a settings pane with four headers would look like four
/// screens stapled together, while four overlines read as one screen with four parts.
pub fn subheader(ui: &mut Ui, theme: Theme, text: &str) {
    let colors = palette(theme);
    ui.label(
        RichText::new(text)
            .text_style(crate::theme::named(text_style::OVERLINE))
            .color(colors.text_muted),
    );
}

/// The brand mark: a filled diamond in the accent colour.
///
/// Painted rather than typed, because the character it stands in for (`U+25C6`) is carried by
/// none of the proportional font stack's faces — a mark that renders as tofu on a stock install
/// is worse than no mark. A painted shape is also sized to the bar rather than to a font.
pub fn brand_mark(ui: &mut Ui, theme: Theme) {
    let colors = palette(theme);
    let side = 12.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let center = rect.center();
    let half = side / 2.0;
    let points = vec![
        egui::pos2(center.x, center.y - half),
        egui::pos2(center.x + half, center.y),
        egui::pos2(center.x, center.y + half),
        egui::pos2(center.x - half, center.y),
    ];
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        colors.accent,
        egui::Stroke::NONE,
    ));
}

/// One tab of the top navigation bar: a text button that stays marked while its pane is open.
///
/// `badge` is the count drawn at the tab's corner — the unread total on Chat, zero elsewhere.
/// Returns whether it was clicked. The open tab is marked two ways on purpose: an accent-tinted
/// surface, and a two-pixel accent underline — the one marker that survives colour-blind
/// viewing of a neon palette. A solid accent fill is deliberately avoided: a bar of neon cyan
/// across the top of the window would out-shout the pane it points at.
pub fn tab_button(ui: &mut Ui, theme: Theme, label: &str, selected: bool, badge: u32) -> bool {
    let colors = palette(theme);
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(font::BODY),
        if selected {
            colors.accent
        } else {
            colors.text_muted
        },
    );
    let padding = Vec2::new(space::LG, space::SM);
    let size = galley.size() + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let background = if selected {
        colors.surface_selected
    } else if response.hovered() {
        colors.surface_hover
    } else {
        Color32::TRANSPARENT
    };
    if background != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::SM), background);
    }
    if selected {
        let underline = egui::Rect::from_min_max(
            egui::pos2(rect.left() + space::SM, rect.bottom() - 2.0),
            egui::pos2(rect.right() - space::SM, rect.bottom()),
        );
        ui.painter()
            .rect_filled(underline, CornerRadius::ZERO, colors.accent);
    }
    let at = rect.center() - galley.size() / 2.0;
    ui.painter().galley(at, galley, colors.text);
    if badge > 0 {
        // Overlaid on the tab's corner rather than given its own slot: a badge that changes
        // the bar's layout as the count appears and disappears would make every tab shift,
        // and a moving target is the one thing a navigation bar must not be.
        let text = if badge > 99 {
            "99+".to_owned()
        } else {
            badge.to_string()
        };
        let mini = ui.painter().layout_no_wrap(
            text,
            FontId::proportional(font::TINY),
            colors.text_on_accent,
        );
        let padding = Vec2::new(space::XS, space::XS * 0.5);
        let size = mini.size() + padding * 2.0;
        let top_right = rect.right_top() + Vec2::new(space::XS, -space::XS);
        let badge_rect = egui::Rect::from_min_size(top_right - egui::vec2(size.x, 0.0), size);
        ui.painter()
            .rect_filled(badge_rect, CornerRadius::same(radius::FULL), colors.accent);
        ui.painter()
            .galley(badge_rect.min + padding, mini, colors.text_on_accent);
    }
    response.clicked()
}

/// A hairline separator that respects the palette instead of egui's default grey.
pub fn divider(ui: &mut Ui, theme: Theme) {
    let colors = palette(theme);
    let height = 1.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.border);
}

/// Small capsule of text: a state, a count, a label.
pub fn pill(ui: &mut Ui, text: &str, foreground: Color32, background: Color32) -> Response {
    let font = FontId::proportional(font::TINY);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, foreground);
    let padding = Vec2::new(space::SM, space::XS * 0.75);
    let size = galley.size() + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::FULL), background);
    ui.painter().galley(rect.min + padding, galley, foreground);
    response
}

/// The unread-count badge on a conversation row.
///
/// Counts above 99 render as `99+`. The exact number stops being useful long before that, and an
/// unbounded badge would push the timestamp out of the row.
pub fn unread_badge(ui: &mut Ui, theme: Theme, count: u32) {
    if count == 0 {
        return;
    }
    let colors = palette(theme);
    let text = if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    };
    pill(ui, &text, colors.text_on_accent, colors.accent);
}

/// A coloured dot plus a word, for the connection state in the top bar.
pub fn status_dot(ui: &mut Ui, theme: Theme, color: Color32, label: &str) {
    let colors = palette(theme);
    let diameter = 8.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, color);
    ui.add_space(space::XS);
    ui.label(
        RichText::new(label)
            .text_style(crate::theme::named(text_style::CAPTION))
            .color(colors.text_muted),
    );
}

/// A circular monogram standing in for an avatar.
///
/// The colour is derived from the seed text rather than drawn at random, so the same person keeps the
/// same colour across restarts and across the conversation list and the chat header. A random colour
/// per frame would flicker; a random colour per session would break the recognition that makes an
/// avatar worth drawing at all.
pub fn avatar(ui: &mut Ui, theme: Theme, seed: &str, diameter: f32) {
    let colors = palette(theme);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let background = tint(seed, theme);
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, background);

    let initial = seed
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned());
    let font = FontId::proportional(diameter * 0.42);
    let galley = ui
        .painter()
        .layout_no_wrap(initial, font, colors.text_on_accent);
    let at = rect.center() - galley.size() / 2.0;
    ui.painter().galley(at, galley, colors.text_on_accent);
}

/// A stable hue for a name.
///
/// FNV-1a over the bytes, then a fixed palette of hues. A hash rather than an index because the seed
/// is an account id or a display name, not a position in a list — the same person must get the same
/// colour whatever order the list happens to arrive in.
fn tint(seed: &str, theme: Theme) -> Color32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in seed.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Six hues, chosen to stay legible against white text in both themes.
    const DARK: [Color32; 6] = [
        Color32::from_rgb(0x3B, 0x6E, 0xD8),
        Color32::from_rgb(0x2F, 0x8F, 0x6B),
        Color32::from_rgb(0x9A, 0x5B, 0xD6),
        Color32::from_rgb(0xC2, 0x6A, 0x3A),
        Color32::from_rgb(0xC0, 0x4A, 0x6E),
        Color32::from_rgb(0x2E, 0x7F, 0x99),
    ];
    const LIGHT: [Color32; 6] = [
        Color32::from_rgb(0x2F, 0x5C, 0xC4),
        Color32::from_rgb(0x24, 0x7A, 0x57),
        Color32::from_rgb(0x82, 0x46, 0xBE),
        Color32::from_rgb(0xAA, 0x55, 0x28),
        Color32::from_rgb(0xA8, 0x38, 0x5A),
        Color32::from_rgb(0x21, 0x69, 0x82),
    ];
    let table = if theme.is_dark() { DARK } else { LIGHT };
    table[(hash % table.len() as u32) as usize]
}

/// A single-line text field with a label above it.
///
/// `secret` masks the content. It is a display measure only: the string still lives in the widget's
/// own buffer, which is why passphrases are moved out of the UI and into the worker as soon as they
/// are submitted rather than held on a screen struct.
pub fn field(
    ui: &mut Ui,
    theme: Theme,
    label: &str,
    value: &mut String,
    secret: bool,
    hint: &str,
) -> Response {
    let colors = palette(theme);
    ui.label(
        RichText::new(label)
            .text_style(crate::theme::named(text_style::OVERLINE))
            .color(colors.text_muted),
    );
    ui.add_space(space::XS);
    let response = ui.add(
        egui::TextEdit::singleline(value)
            .password(secret)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8)),
    );
    ui.add_space(space::MD);
    response
}

/// The one prominent action on a screen.
pub fn primary_button(ui: &mut Ui, theme: Theme, text: &str, enabled: bool) -> Response {
    let colors = palette(theme);
    // Scoped so the three accent shades apply to this button and nothing after it.
    //
    // `Button::fill` is one colour for every state, so a filled button gets no hover and no press
    // feedback — which reads as disabled, on the one control the user is most likely to aim at.
    // Writing the shades into the widget visuals instead lets egui pick the right one itself.
    ui.scope(|ui| {
        {
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = if enabled {
                colors.accent
            } else {
                colors.surface_hover
            };
            w.inactive.bg_stroke = Stroke::NONE;
            w.hovered.weak_bg_fill = colors.accent_hover;
            w.hovered.bg_stroke = Stroke::NONE;
            w.active.weak_bg_fill = colors.accent_active;
            w.active.bg_stroke = Stroke::NONE;
        }
        let width = ui.available_width();
        ui.add_enabled(
            enabled,
            egui::Button::new(
                RichText::new(text)
                    .font(FontId::proportional(font::SUBTITLE))
                    .color(colors.text_on_accent),
            )
            .corner_radius(CornerRadius::same(radius::MD))
            .min_size(Vec2::new(width, 40.0)),
        )
    })
    .inner
}

/// A quieter action beside the primary one.
pub fn ghost_button(ui: &mut Ui, theme: Theme, text: &str) -> Response {
    let colors = palette(theme);
    ui.add(
        egui::Button::new(
            RichText::new(text)
                .font(FontId::proportional(font::BODY))
                .color(colors.text_muted),
        )
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, colors.border))
        .corner_radius(CornerRadius::same(radius::MD)),
    )
}

/// One row of the conversation list.
///
/// Drawn by hand rather than as a `SelectableLabel` because a row carries four things at once — an
/// avatar, a title, a preview and a right-aligned time plus badge — and egui's selectable label
/// draws one string. The manual version also lets the whole row be the click target, which is what a
/// pointer expects from a list.
pub struct RowContent<'a> {
    pub title: &'a str,
    pub preview: Option<&'a str>,
    pub time: Option<&'a str>,
    pub unread: u32,
    pub selected: bool,
    pub encrypted: bool,
}

/// Draws one conversation row and returns its response.
pub fn conversation_row(ui: &mut Ui, theme: Theme, content: RowContent<'_>) -> Response {
    let colors = palette(theme);
    let height = 62.0;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

    let background = if content.selected {
        colors.surface_selected
    } else if response.hovered() {
        colors.surface_hover
    } else {
        Color32::TRANSPARENT
    };
    if background != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::MD), background);
    }

    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(space::SM, space::SM)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    avatar(&mut inner, theme, content.title, 38.0);
    inner.add_space(space::SM);

    inner.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(elide(content.title, 26))
                    .font(FontId::proportional(font::BODY))
                    .color(colors.text)
                    .strong(),
            );
            if content.encrypted {
                // A lock on a conversation that is genuinely end-to-end encrypted, and nothing at all
                // on one that is not. A badge that appears either way teaches the user to ignore it.
                ui.label(RichText::new("\u{1F512}").font(FontId::proportional(font::TINY)));
            }
        });
        if let Some(preview) = content.preview {
            ui.label(
                RichText::new(elide(preview, 38))
                    .font(FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        }
    });

    inner.with_layout(Layout::right_to_left(Align::Center), |ui| {
        unread_badge(ui, theme, content.unread);
        if let Some(time) = content.time {
            ui.add_space(space::XS);
            ui.label(
                RichText::new(time)
                    .text_style(crate::theme::named(text_style::CAPTION))
                    .color(colors.text_muted),
            );
        }
    });

    response
}

/// A message bubble.
///
/// Outgoing bubbles are accent-filled and right-aligned, incoming ones surface-filled and left. The
/// asymmetry is the whole point: a reader must be able to tell who said what without reading a name,
/// and alignment does that at a glance in a way a label cannot.
pub fn bubble(ui: &mut Ui, theme: Theme, text: &str, meta: &str, outgoing: bool, tone: BubbleTone) {
    let colors = palette(theme);
    let (fill, foreground) = match tone {
        BubbleTone::Normal if outgoing => (colors.accent, colors.text_on_accent),
        BubbleTone::Normal => (colors.surface_raised, colors.text),
        // A failure is not styled like a message: it is a report about one, so it takes the muted
        // surface and the danger colour rather than pretending to be content.
        BubbleTone::Problem => (colors.surface_raised, colors.danger),
    };

    let layout = if outgoing {
        Layout::right_to_left(Align::Min)
    } else {
        Layout::left_to_right(Align::Min)
    };
    ui.with_layout(layout, |ui| {
        // Bubbles stop at 68% of the pane. Full-width bubbles on a maximised window produce lines too
        // long to track back to the start of, and they erase the alignment cue entirely.
        let max = (ui.available_width() * 0.68).max(140.0);
        egui::Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(radius::LG))
            .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
            .show(ui, |ui| {
                ui.set_max_width(max);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(text)
                            .font(FontId::proportional(font::BODY))
                            .color(foreground),
                    );
                    ui.add_space(space::XS * 0.5);
                    ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                        ui.label(
                            RichText::new(meta)
                                .font(FontId::proportional(font::TINY))
                                .color(muted_on(foreground)),
                        );
                    });
                });
            });
    });
}

/// Which of the two bubble treatments to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BubbleTone {
    Normal,
    Problem,
}

/// A dimmer version of a foreground colour, for the timestamp inside a bubble.
///
/// Derived from the text colour rather than taken from the palette because the bubble's fill differs
/// by direction: a fixed muted grey is legible on the surface fill and nearly invisible on accent.
fn muted_on(foreground: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(foreground.r(), foreground.g(), foreground.b(), 170)
}

/// Centred placeholder text for an empty pane.
///
/// An empty list with nothing in it looks broken. A sentence explaining why it is empty and what to
/// do next costs one line and removes the ambiguity.
pub fn empty_state(ui: &mut Ui, theme: Theme, title: &str, detail: &str) {
    let colors = palette(theme);
    ui.vertical_centered(|ui| {
        ui.add_space(space::XXL);
        ui.label(
            RichText::new(title)
                .font(FontId::proportional(font::SUBTITLE))
                .color(colors.text),
        );
        ui.add_space(space::SM);
        ui.label(
            RichText::new(detail)
                .font(FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
    });
}

/// The toast stack, bottom-centre.
///
/// Toasts are drawn as an overlay area rather than in the layout so an arriving one never reflows the
/// conversation under the reader's cursor.
pub fn toasts(ctx: &egui::Context, theme: Theme, items: &[crate::model::Toast]) {
    if items.is_empty() {
        return;
    }
    let colors = palette(theme);
    egui::Area::new(egui::Id::new("migo-toasts"))
        .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -space::XL))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                for toast in items {
                    let accent = match toast.kind {
                        crate::model::ToastKind::Info => colors.accent,
                        crate::model::ToastKind::Success => colors.positive,
                        crate::model::ToastKind::Error => colors.danger,
                    };
                    // Fade over the last second of the lifetime, so a toast leaves rather than
                    // vanishing between frames.
                    let alpha = (toast.remaining.min(1.0) * 255.0) as u8;
                    egui::Frame::new()
                        .fill(with_alpha(colors.surface_overlay, alpha))
                        .stroke(Stroke::new(1.0, with_alpha(accent, alpha)))
                        .corner_radius(CornerRadius::same(radius::MD))
                        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::SM as i8))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&toast.text)
                                    .font(FontId::proportional(font::SMALL))
                                    .color(with_alpha(colors.text, alpha)),
                            );
                        });
                    ui.add_space(space::SM);
                }
            });
        });
}

/// Replaces a colour's alpha, preserving its channels.
fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// Truncates on a character boundary, appending an ellipsis.
///
/// Character-wise rather than byte-wise: slicing a UTF-8 string at an arbitrary byte index panics,
/// and a conversation title is exactly the sort of user-supplied text that contains multi-byte
/// characters.
pub fn elide(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        if index >= max_chars {
            out.push('\u{2026}');
            break;
        }
        out.push(character);
    }
    out
}
