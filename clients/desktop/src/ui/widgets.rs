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
    Align, Color32, CornerRadius, FontId, Layout, Response, RichText, Sense, Stroke, TextStyle, Ui,
    Vec2,
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

/// A navigation icon, painted with the painter rather than typed.
///
/// The design system draws its icons as strokes, and this client declares no icon font — so the
/// strip's glyphs are painted the way the brand mark is: geometric shapes sized to a 20px
/// box, one stroke weight, `currentColor` semantics via the palette. Each icon sits in a
/// 20×20 box centred on the allocation it is given.
pub fn place_icon(ui: &mut Ui, theme: Theme, place: crate::ui::Place, active: bool) {
    let colors = palette(theme);
    let stroke = egui::Stroke::new(
        1.75,
        if active {
            colors.accent
        } else {
            colors.text_muted
        },
    );
    let side = 20.0;
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), Sense::hover());
    let painter = ui.painter().clone();
    let min = rect.min;
    // A unit box: (0,0) top-left to (1,1) bottom-right, scaled to the allocation.
    let p = |x: f32, y: f32| egui::pos2(min.x + x * side, min.y + y * side);
    match place {
        crate::ui::Place::Rooms => {
            // A hash: the glyph the whole product marks rooms with.
            for x in [0.32, 0.68] {
                painter.line_segment([p(x, 0.08), p(x, 0.92)], stroke);
            }
            for y in [0.32, 0.68] {
                painter.line_segment([p(0.08, y), p(0.92, y)], stroke);
            }
        }
        crate::ui::Place::Feed => {
            // A pulse: activity as a heartbeat line.
            painter.line_segment([p(0.05, 0.55), p(0.3, 0.55)], stroke);
            painter.line_segment([p(0.3, 0.55), p(0.4, 0.2)], stroke);
            painter.line_segment([p(0.4, 0.2), p(0.55, 0.85)], stroke);
            painter.line_segment([p(0.55, 0.85), p(0.65, 0.55)], stroke);
            painter.line_segment([p(0.65, 0.55), p(0.95, 0.55)], stroke);
        }
        crate::ui::Place::Games => {
            // A game pad: a D-pad cross and the two action dots.
            painter.line_segment([p(0.4, 0.2), p(0.4, 0.62)], stroke);
            painter.line_segment([p(0.19, 0.41), p(0.61, 0.41)], stroke);
            painter.add(egui::Shape::circle_stroke(
                p(0.78, 0.34),
                side * 0.07,
                stroke,
            ));
            painter.add(egui::Shape::circle_stroke(
                p(0.88, 0.5),
                side * 0.07,
                stroke,
            ));
        }
        crate::ui::Place::Friends => {
            // Two people: a taller figure and a shorter one behind it.
            painter.add(egui::Shape::circle_stroke(
                p(0.35, 0.28),
                side * 0.13,
                stroke,
            ));
            painter.add(egui::Shape::circle_stroke(
                p(0.72, 0.33),
                side * 0.1,
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![
                    p(0.1, 0.9),
                    p(0.13, 0.62),
                    p(0.35, 0.5),
                    p(0.57, 0.62),
                    p(0.6, 0.9),
                ],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(0.6, 0.9), p(0.63, 0.68), p(0.86, 0.6), p(0.95, 0.9)],
                stroke,
            ));
        }
        crate::ui::Place::Alerts => {
            // A bell: a dome, a lip, and a clapper.
            painter.add(egui::Shape::line(
                vec![
                    p(0.2, 0.75),
                    p(0.2, 0.5),
                    p(0.3, 0.25),
                    p(0.5, 0.15),
                    p(0.7, 0.25),
                    p(0.8, 0.5),
                    p(0.8, 0.75),
                ],
                stroke,
            ));
            painter.line_segment([p(0.12, 0.75), p(0.88, 0.75)], stroke);
            painter.add(egui::Shape::circle_stroke(
                p(0.5, 0.88),
                side * 0.07,
                stroke,
            ));
        }
        crate::ui::Place::Search => {
            // A magnifier: a lens and a handle.
            painter.add(egui::Shape::circle_stroke(
                p(0.42, 0.42),
                side * 0.28,
                stroke,
            ));
            painter.line_segment([p(0.62, 0.62), p(0.9, 0.9)], stroke);
        }
        crate::ui::Place::Wallet => {
            // A wallet: a box with a clasp.
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.08, 0.22), p(0.92, 0.82)),
                4.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([p(0.62, 0.48), p(0.92, 0.48)], stroke);
            painter.line_segment([p(0.62, 0.38), p(0.62, 0.62)], stroke);
        }
        crate::ui::Place::Profile => {
            // A person card: the bust of a profile beside a card's lines.
            painter.add(egui::Shape::circle_stroke(
                p(0.32, 0.3),
                side * 0.14,
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![
                    p(0.12, 0.85),
                    p(0.15, 0.6),
                    p(0.32, 0.52),
                    p(0.5, 0.6),
                    p(0.52, 0.85),
                ],
                stroke,
            ));
            painter.line_segment([p(0.6, 0.3), p(0.9, 0.3)], stroke);
            painter.line_segment([p(0.6, 0.5), p(0.9, 0.5)], stroke);
            painter.line_segment([p(0.6, 0.7), p(0.82, 0.7)], stroke);
        }
        crate::ui::Place::Settings => {
            // A dial: a circle with spokes, the honest settings mark.
            painter.add(egui::Shape::circle_stroke(p(0.5, 0.5), side * 0.22, stroke));
            for (dx, dy) in [
                (0.0f32, -1.0f32),
                (0.0f32, 1.0f32),
                (-1.0f32, 0.0f32),
                (1.0f32, 0.0f32),
                (0.71f32, -0.71f32),
                (0.71f32, 0.71f32),
                (-0.71f32, -0.71f32),
                (-0.71f32, 0.71f32),
            ] {
                painter.line_segment(
                    [
                        egui::pos2(
                            p(0.5, 0.5).x + dx * side * 0.32,
                            p(0.5, 0.5).y + dy * side * 0.32,
                        ),
                        egui::pos2(
                            p(0.5, 0.5).x + dx * side * 0.46,
                            p(0.5, 0.5).y + dy * side * 0.46,
                        ),
                    ],
                    stroke,
                );
            }
        }
        crate::ui::Place::Admins => {
            // A shield: the mark the whole product stamps authority with. One outline for the
            // badge, a notch for the point, and a check for the mandate it carries.
            painter.add(egui::Shape::line(
                vec![
                    p(0.2, 0.12),
                    p(0.8, 0.12),
                    p(0.8, 0.5),
                    p(0.5, 0.9),
                    p(0.2, 0.5),
                    p(0.2, 0.12),
                ],
                stroke,
            ));
            painter.line_segment([p(0.36, 0.48), p(0.46, 0.6)], stroke);
            painter.line_segment([p(0.46, 0.6), p(0.68, 0.32)], stroke);
        }
    }
}

/// What happened to one chip on the tab strip.
#[derive(Debug, Default, Clone, Copy)]
pub struct ChipOutcome {
    /// The chip's body was clicked: select whatever it names.
    pub clicked: bool,
    /// The chip's close mark was clicked: close whatever it names.
    pub closed: bool,
}

/// One chip on the navigation strip.
///
/// The strip is the teal `nav` fill and a chip is a rounded label on it: the active chip takes
/// the brighter accent and the orange underline, exactly the pairing the reference draws. A
/// closable chip (an open conversation, an open panel) carries an × that must not also select —
/// the two responses are reported separately so a close never opens what it is closing.
pub fn tab_chip(
    ui: &mut Ui,
    theme: Theme,
    label: &str,
    icon: Option<crate::ui::Place>,
    active: bool,
    closable: bool,
) -> ChipOutcome {
    let colors = palette(theme);
    let font = FontId::proportional(font::SMALL);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), colors.banner_ink);
    let icon_room = if icon.is_some() { 26.0 } else { 0.0 };
    let close_room = if closable { 24.0 } else { 0.0 };
    let padding = Vec2::new(space::MD, 0.0);
    // The chip's height is the Small row plus a pad above and below, not a pixel count: the
    // strip sizes itself from this same derivation, so chip, text and bar grow together.
    let height = ui.text_style_height(&TextStyle::Small) + 2.0 * space::SM;
    let size = egui::vec2(
        galley.size().x + icon_room + close_room + padding.x * 2.0,
        height,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let mut outcome = ChipOutcome::default();
    if response.clicked() {
        outcome.clicked = true;
    }

    let fill = if active {
        colors.accent_bright
    } else {
        // A translucent dark over the teal strip reads as the reference's idle chip in either
        // theme: the strip is the same family of colour in both, so one overlay serves both.
        Color32::from_black_alpha(70)
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::TAB), fill);
    if active {
        // The orange underline: 3px of banner orange inset at the chip's foot.
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() + space::SM, rect.bottom() - 3.0),
            egui::pos2(rect.right() - space::SM, rect.bottom()),
        );
        ui.painter()
            .rect_filled(bar, CornerRadius::same(radius::SM), colors.banner_b);
    }

    let mut at = rect.left() + padding.x;
    if let Some(place) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(at, rect.center().y - 10.0),
            egui::vec2(20.0, 20.0),
        );
        let mut inner = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(icon_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        // White on the strip whatever the theme: the chip's ink is the banner's ink.
        inner.set_visuals(egui::Visuals {
            override_text_color: Some(colors.banner_ink),
            ..egui::Visuals::dark()
        });
        place_icon(&mut inner, theme, place, active);
        at += icon_room;
    }
    ui.painter().galley(
        egui::pos2(at, rect.center().y - galley.size().y / 2.0),
        galley,
        colors.banner_ink,
    );
    if active {
        // Stronger ink for the selected chip, so the strip's "you are here" is readable at a
        // glance even among many open chats.
    }

    if closable {
        let mark = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::vec2(16.0, 16.0),
        );
        let close = ui.interact(mark, ui.id().with(label).with("close"), Sense::click());
        if close.clicked() {
            outcome.closed = true;
        }
        let hover = if close.hovered() {
            Color32::from_white_alpha(70)
        } else {
            Color32::TRANSPARENT
        };
        if hover != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(mark, CornerRadius::same(radius::FULL), hover);
        }
        let stroke = egui::Stroke::new(1.5, colors.banner_ink);
        ui.painter().line_segment(
            [
                egui::pos2(mark.left() + 5.0, mark.top() + 5.0),
                egui::pos2(mark.right() - 5.0, mark.bottom() - 5.0),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                egui::pos2(mark.right() - 5.0, mark.top() + 5.0),
                egui::pos2(mark.left() + 5.0, mark.bottom() - 5.0),
            ],
            stroke,
        );
    }

    outcome
}

/// Paints a horizontal three-stop gradient across a rect.
///
/// egui has no gradient primitive, so this is the two-triangle mesh it decomposes to: `a` at the
/// left edge, `b` at the middle, `c` at the right. Used by the profile banner, which is the one
/// surface in the client that is a gradient rather than a fill.
pub fn gradient_rect(ui: &mut Ui, rect: egui::Rect, a: Color32, b: Color32, c: Color32) {
    let mut mesh = egui::Mesh::default();
    let (l, m, r) = (rect.left_top(), rect.center_top(), rect.right_top());
    let (lb, mb, rb) = (
        rect.left_bottom(),
        rect.center_bottom(),
        rect.right_bottom(),
    );
    let base = mesh.vertices.len() as u32;
    for (pos, color) in [(l, a), (lb, a), (m, b), (mb, b)] {
        mesh.colored_vertex(pos, color);
    }
    mesh.indices
        .extend([base, base + 1, base + 2, base + 2, base + 1, base + 3]);
    let base = mesh.vertices.len() as u32;
    for (pos, color) in [(m, b), (mb, b), (r, c), (rb, c)] {
        mesh.colored_vertex(pos, color);
    }
    mesh.indices
        .extend([base, base + 1, base + 2, base + 2, base + 1, base + 3]);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Paints a vertical three-stop gradient across a rect, for the login screen.
pub fn gradient_rect_vertical(ui: &mut Ui, rect: egui::Rect, a: Color32, b: Color32, c: Color32) {
    let mut mesh = egui::Mesh::default();
    let (t, m, bo) = (rect.left_top(), rect.left_center(), rect.left_bottom());
    let (tr, mr, br) = (rect.right_top(), rect.right_center(), rect.right_bottom());
    let base = mesh.vertices.len() as u32;
    for (pos, color) in [(t, a), (tr, a), (m, b), (mr, b)] {
        mesh.colored_vertex(pos, color);
    }
    mesh.indices
        .extend([base, base + 1, base + 2, base + 2, base + 1, base + 3]);
    let base = mesh.vertices.len() as u32;
    for (pos, color) in [(m, b), (mr, b), (bo, c), (br, c)] {
        mesh.colored_vertex(pos, color);
    }
    mesh.indices
        .extend([base, base + 1, base + 2, base + 2, base + 1, base + 3]);
    ui.painter().add(egui::Shape::mesh(mesh));
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

/// The avatar as it sits on the orange banner: the same monogram logic, drawn as a translucent
/// white disc ringed in white, because the tinted avatar's hue would argue with the gradient
/// behind it.
pub fn banner_avatar(ui: &mut Ui, theme: Theme, seed: &str, diameter: f32) -> Response {
    let colors = palette(theme);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    let radius = diameter / 2.0;
    ui.painter()
        .circle_filled(rect.center(), radius, Color32::from_white_alpha(60));
    ui.painter().circle_stroke(
        rect.center(),
        radius - 1.0,
        egui::Stroke::new(1.5, colors.banner_ink),
    );

    let initial = seed
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned());
    let font = FontId::proportional(diameter * 0.42);
    let galley = ui
        .painter()
        .layout_no_wrap(initial, font, colors.banner_ink);
    let at = rect.center() - galley.size() / 2.0;
    ui.painter().galley(at, galley, colors.banner_ink);
    response
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

/// The one prominent action on a screen: the reference's orange — the banner's own gradient
/// family, so every "Go" in the product is the same orange.
pub fn primary_button(ui: &mut Ui, theme: Theme, text: &str, enabled: bool) -> Response {
    let colors = palette(theme);
    // Scoped so the three banner shades apply to this button and nothing after it.
    //
    // `Button::fill` is one colour for every state, so a filled button gets no hover and no press
    // feedback — which reads as disabled, on the one control the user is most likely to aim at.
    // Writing the shades into the widget visuals instead lets egui pick the right one itself.
    ui.scope(|ui| {
        {
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = if enabled {
                colors.banner_b
            } else {
                colors.surface_hover
            };
            w.inactive.bg_stroke = Stroke::NONE;
            w.hovered.weak_bg_fill = colors.banner_c;
            w.hovered.bg_stroke = Stroke::NONE;
            w.active.weak_bg_fill = colors.banner_a;
            w.active.bg_stroke = Stroke::NONE;
        }
        let width = ui.available_width();
        ui.add_enabled(
            enabled,
            egui::Button::new(
                RichText::new(text)
                    .font(FontId::proportional(font::SUBTITLE))
                    .color(colors.banner_ink),
            )
            .corner_radius(CornerRadius::same(radius::MD))
            .min_size(Vec2::new(width, 40.0)),
        )
    })
    .inner
}

/// The composer's send control: a filled accent circle with a paper plane drawn on it — the
/// reference composer's send mark. Disabled reads as the muted surface, never as a grey text
/// button.
pub fn send_button(ui: &mut Ui, theme: Theme, enabled: bool) -> Response {
    let colors = palette(theme);
    let side = 40.0;
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(side), Sense::click());
    // The three accent states the palette names, chosen by the interaction egui already tracked
    // for this response: idle, hovered, held.
    let fill = if !enabled {
        colors.surface_hover
    } else if response.is_pointer_button_down_on() {
        colors.accent_active
    } else if response.hovered() {
        colors.accent_hover
    } else {
        colors.accent
    };
    ui.painter().circle_filled(rect.center(), side / 2.0, fill);

    // A paper plane: two strokes forming the classic shape, in the fill's contrast ink.
    let ink = if enabled {
        colors.text_on_accent
    } else {
        colors.text_muted
    };
    let stroke = egui::Stroke::new(1.75, ink);
    let min = rect.min;
    let p = |x: f32, y: f32| egui::pos2(min.x + x * side, min.y + y * side);
    ui.painter().add(egui::Shape::line(
        vec![
            p(0.26, 0.52),
            p(0.76, 0.28),
            p(0.52, 0.76),
            p(0.44, 0.56),
            p(0.26, 0.52),
        ],
        stroke,
    ));
    ui.painter()
        .line_segment([p(0.76, 0.28), p(0.44, 0.56)], stroke);
    response
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
        // Truncation follows the row's real width rather than a char count: a fixed count is
        // wrong in both directions — cutting a title short on a wide window, and overflowing on
        // a narrow one — and the row already knows how much room it has.
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(content.title)
                        .font(FontId::proportional(font::BODY))
                        .color(colors.text)
                        .strong(),
                )
                .truncate(),
            );
            if content.encrypted {
                // A lock on a conversation that is genuinely end-to-end encrypted, and nothing at all
                // on one that is not. A badge that appears either way teaches the user to ignore it.
                ui.label(RichText::new("\u{1F512}").font(FontId::proportional(font::TINY)));
            }
        });
        if let Some(preview) = content.preview {
            ui.add(
                egui::Label::new(
                    RichText::new(preview)
                        .font(FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                )
                .truncate(),
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
