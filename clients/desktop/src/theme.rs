//! The design system: one place that decides what the application looks like.
//!
//! Every colour, radius, gap and font size in this client comes from here. Nothing downstream
//! writes a literal `Color32::from_rgb(...)` or a bare `8.0` for padding, because the moment two
//! screens each carry their own idea of "the muted text colour" they drift, and a drifted interface
//! reads as unfinished no matter how correct the code behind it is.
//!
//! # Tokens, then a style
//!
//! [`Palette`] is the semantic layer: `surface`, `surface_raised`, `text_muted`, `accent`. Callers
//! ask for a *role*, never for a hue, which is what makes a second theme a data change rather than
//! a rewrite. [`install`] then pushes those tokens into egui's own [`egui::Style`] so that stock
//! widgets — a `TextEdit`, a `ScrollArea`, a tooltip — already look right without being wrapped.
//!
//! # Why two themes and not three
//!
//! Dark and light, following the desktop's own preference on first run. There is no per-widget
//! theme override and no accent picker: a messenger is read for hours at a time, and the useful
//! knob is the one the operating system already provides.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2};

/// Which of the two themes the interface is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    /// Follows the desktop's preference, falling back to dark when it does not express one.
    pub fn from_system(ctx: &egui::Context) -> Self {
        match ctx.system_theme() {
            Some(egui::Theme::Light) => Self::Light,
            _ => Self::Dark,
        }
    }

    /// The other theme, for the toggle in the title bar.
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    /// A one-word label for the toggle's tooltip.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    #[must_use]
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }
}

/// The semantic colour roles the interface is drawn from.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// The window's own background — the furthest-back layer.
    pub surface: Color32,
    /// A panel sitting on the surface: the conversation list, the composer bar.
    pub surface_raised: Color32,
    /// A card or bubble sitting on a panel.
    pub surface_overlay: Color32,
    /// A barely-there fill for hovered rows and zebra striping.
    pub surface_hover: Color32,
    /// The fill behind the currently selected conversation.
    pub surface_selected: Color32,
    /// Hairlines: panel separators, input outlines, bubble edges.
    pub border: Color32,
    /// A border that has the user's attention — a focused input.
    pub border_strong: Color32,
    /// Body text.
    pub text: Color32,
    /// Secondary text: timestamps, message previews, helper lines.
    pub text_muted: Color32,
    /// Text on top of [`Palette::accent`].
    pub text_on_accent: Color32,
    /// The one brand colour. Primary buttons, the caret, the outgoing bubble.
    pub accent: Color32,
    /// A hovered accent surface.
    pub accent_hover: Color32,
    /// A pressed accent surface.
    pub accent_active: Color32,
    /// Success, and "connected".
    pub positive: Color32,
    /// A warning, and "connecting".
    pub warning: Color32,
    /// An error, and "disconnected".
    pub danger: Color32,
    /// The verified-key indicator, deliberately distinct from `positive`.
    pub verified: Color32,
}

/// Dark, and the default. Neutral greys rather than blue-tinted ones so the accent stays the only
/// saturated thing on screen.
const DARK: Palette = Palette {
    surface: Color32::from_rgb(0x0e, 0x0f, 0x12),
    surface_raised: Color32::from_rgb(0x16, 0x18, 0x1d),
    surface_overlay: Color32::from_rgb(0x1e, 0x21, 0x27),
    surface_hover: Color32::from_rgb(0x25, 0x28, 0x30),
    surface_selected: Color32::from_rgb(0x2a, 0x2f, 0x3a),
    border: Color32::from_rgb(0x27, 0x2a, 0x32),
    border_strong: Color32::from_rgb(0x3c, 0x41, 0x4d),
    text: Color32::from_rgb(0xe8, 0xea, 0xed),
    text_muted: Color32::from_rgb(0x8d, 0x93, 0xa1),
    text_on_accent: Color32::from_rgb(0x08, 0x0a, 0x0c),
    accent: Color32::from_rgb(0x5c, 0xd0, 0xa8),
    accent_hover: Color32::from_rgb(0x72, 0xdd, 0xb8),
    accent_active: Color32::from_rgb(0x46, 0xb8, 0x91),
    positive: Color32::from_rgb(0x5c, 0xd0, 0xa8),
    warning: Color32::from_rgb(0xe3, 0xb3, 0x41),
    danger: Color32::from_rgb(0xe8, 0x6a, 0x6a),
    verified: Color32::from_rgb(0x77, 0xb6, 0xf0),
};

/// Light. Not an inversion of [`DARK`] — the borders carry more of the structure here, because a
/// light interface cannot lean on fill contrast the way a dark one can.
const LIGHT: Palette = Palette {
    surface: Color32::from_rgb(0xf7, 0xf8, 0xfa),
    surface_raised: Color32::from_rgb(0xff, 0xff, 0xff),
    surface_overlay: Color32::from_rgb(0xff, 0xff, 0xff),
    surface_hover: Color32::from_rgb(0xed, 0xef, 0xf3),
    surface_selected: Color32::from_rgb(0xe2, 0xe7, 0xef),
    border: Color32::from_rgb(0xdd, 0xe1, 0xe8),
    border_strong: Color32::from_rgb(0xb4, 0xbb, 0xc7),
    text: Color32::from_rgb(0x14, 0x17, 0x1c),
    text_muted: Color32::from_rgb(0x5f, 0x67, 0x74),
    text_on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
    accent: Color32::from_rgb(0x0f, 0x9d, 0x74),
    accent_hover: Color32::from_rgb(0x14, 0xae, 0x81),
    accent_active: Color32::from_rgb(0x0b, 0x82, 0x60),
    positive: Color32::from_rgb(0x0f, 0x9d, 0x74),
    warning: Color32::from_rgb(0xa8, 0x71, 0x00),
    danger: Color32::from_rgb(0xc9, 0x33, 0x3f),
    verified: Color32::from_rgb(0x1f, 0x6f, 0xd0),
};

/// The palette for a theme.
#[must_use]
pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark => DARK,
        Theme::Light => LIGHT,
    }
}

/// The spacing scale. Everything is a multiple of 4, so vertical rhythm survives being composed
/// out of independent widgets.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
}

/// The corner-radius scale. `u8` because that is what egui's [`CornerRadius`] stores.
pub mod radius {
    pub const SM: u8 = 6;
    pub const MD: u8 = 10;
    pub const LG: u8 = 14;
    /// A circle, for avatars and icon buttons.
    pub const FULL: u8 = 255;
}

/// The type scale, in points.
pub mod font {
    pub const DISPLAY: f32 = 26.0;
    pub const TITLE: f32 = 18.0;
    pub const SUBTITLE: f32 = 15.0;
    pub const BODY: f32 = 14.5;
    pub const SMALL: f32 = 12.5;
    pub const TINY: f32 = 11.0;
}

/// Named text styles beyond egui's built-in five.
pub mod text_style {
    /// The screen-level heading on the auth screens.
    pub const DISPLAY: &str = "display";
    /// A panel or conversation title.
    pub const TITLE: &str = "title";
    /// Timestamps, previews, helper lines.
    pub const CAPTION: &str = "caption";
    /// Badges and overline labels.
    pub const OVERLINE: &str = "overline";
}

/// Looks up a named text style, falling back to body text if it was never registered.
#[must_use]
pub fn named(name: &'static str) -> TextStyle {
    TextStyle::Name(name.into())
}

/// Pushes a theme's tokens into egui's style.
///
/// Called once at startup and again whenever the theme changes. It is deliberately total: every
/// field it touches is set unconditionally from the palette rather than nudged from whatever the
/// previous theme left behind, so switching themes twice lands exactly where switching once did.
pub fn install(ctx: &egui::Context, theme: Theme) {
    let p = palette(theme);
    let mut style = (*ctx.global_style()).clone();

    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(font::TITLE, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(font::BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(font::BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(font::SMALL, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(font::SMALL, FontFamily::Monospace),
        ),
        (
            named(text_style::DISPLAY),
            FontId::new(font::DISPLAY, FontFamily::Proportional),
        ),
        (
            named(text_style::TITLE),
            FontId::new(font::SUBTITLE, FontFamily::Proportional),
        ),
        (
            named(text_style::CAPTION),
            FontId::new(font::SMALL, FontFamily::Proportional),
        ),
        (
            named(text_style::OVERLINE),
            FontId::new(font::TINY, FontFamily::Proportional),
        ),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(space::SM, space::SM);
    s.button_padding = Vec2::new(space::MD, space::SM);
    s.window_margin = Margin::same(space::MD as i8);
    s.menu_margin = Margin::same(space::SM as i8);
    s.indent = space::LG;
    s.interact_size = Vec2::new(40.0, 32.0);
    s.slider_width = 180.0;
    s.combo_width = 180.0;
    s.text_edit_width = 320.0;
    s.icon_width = 18.0;
    s.icon_width_inner = 10.0;
    s.icon_spacing = space::SM;
    s.tooltip_width = 420.0;
    s.menu_spacing = space::XS;
    s.scroll.bar_width = 8.0;
    s.scroll.bar_inner_margin = 2.0;
    s.scroll.bar_outer_margin = 0.0;
    s.scroll.floating = true;

    let mut v = if theme.is_dark() {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.dark_mode = theme.is_dark();
    v.override_text_color = Some(p.text);
    v.panel_fill = p.surface;
    v.window_fill = p.surface_raised;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(radius::LG);
    v.menu_corner_radius = CornerRadius::same(radius::MD);
    v.faint_bg_color = p.surface_hover;
    v.extreme_bg_color = p.surface;
    v.code_bg_color = p.surface_overlay;
    v.hyperlink_color = p.verified;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection = egui::style::Selection {
        bg_fill: p.accent.gamma_multiply(0.35),
        stroke: Stroke::new(1.0, p.accent),
    };
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.button_frame = true;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.resize_corner_size = 12.0;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    // Flat by choice. Depth here comes from three surface levels and a hairline border, which
    // stays legible at any scale factor; a blurred drop shadow does not, and it is the first thing
    // that looks wrong on a fractional-scaling display.
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;

    let w = &mut v.widgets;
    // Non-interactive: labels, separators, panel frames.
    w.noninteractive.bg_fill = p.surface_raised;
    w.noninteractive.weak_bg_fill = p.surface_raised;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text_muted);
    w.noninteractive.corner_radius = CornerRadius::same(radius::MD);
    w.noninteractive.expansion = 0.0;
    // Inactive: a button or field at rest.
    w.inactive.bg_fill = p.surface_overlay;
    w.inactive.weak_bg_fill = p.surface_overlay;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = CornerRadius::same(radius::SM);
    w.inactive.expansion = 0.0;
    // Hovered.
    w.hovered.bg_fill = p.surface_hover;
    w.hovered.weak_bg_fill = p.surface_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = CornerRadius::same(radius::SM);
    w.hovered.expansion = 0.0;
    // Active: held down.
    w.active.bg_fill = p.surface_selected;
    w.active.weak_bg_fill = p.surface_selected;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = CornerRadius::same(radius::SM);
    w.active.expansion = 0.0;
    // Open: a combo box or menu that is showing its contents.
    w.open.bg_fill = p.surface_overlay;
    w.open.weak_bg_fill = p.surface_overlay;
    w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = CornerRadius::same(radius::SM);
    w.open.expansion = 0.0;

    style.visuals = v;
    // Fast enough that the interface feels responsive, slow enough that a hover is a fade rather
    // than a flicker.
    style.animation_time = 0.10;
    ctx.set_global_style(style);
}
