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
//!
//! # The dark theme is the neon theme
//!
//! Dark is not a neutral grey scale but the neon palette: near-black surfaces with a faint
//! violet cast, one neon cyan accent, neon green for "online" and success, and a red kept well
//! away from the accent so a failure can never be mistaken for a highlight. The light theme
//! stays the quiet one it always was, because a neon palette on white is unreadable, not
//! striking.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2};
use serde::{Deserialize, Serialize};

/// Which of the two themes the interface is drawn in.
///
/// `Serialize`/`Deserialize` so the user's choice can live in the settings file; the wire form
/// is `"dark"`/`"light"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

    /// The other theme, for the toggle in the top bar.
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

/// The neon dark theme, and the default — the canonical Migo dark palette, from
/// `shared/design/tokens.json`.
///
/// The surfaces sit almost at black with a violet cast (`#0a0a12` window, `#111118` panel) so the
/// cyan accent is the brightest saturated thing on screen by a wide margin — the neon effect the
/// palette is named for comes from that contrast, not from any glow. Green is reserved for
/// presence and success, red for failure; neither ever marks a selection or a primary action,
/// because a user must not have to learn which "bright colour" means what.
const DARK: Palette = Palette {
    surface: Color32::from_rgb(0x0a, 0x0a, 0x12),
    surface_raised: Color32::from_rgb(0x11, 0x11, 0x18),
    surface_overlay: Color32::from_rgb(0x17, 0x17, 0x21),
    surface_hover: Color32::from_rgb(0x1c, 0x1c, 0x29),
    surface_selected: Color32::from_rgb(0x0f, 0x2b, 0x36),
    border: Color32::from_rgb(0x22, 0x22, 0x30),
    border_strong: Color32::from_rgb(0x38, 0x38, 0x50),
    text: Color32::from_rgb(0xe8, 0xe8, 0xf0),
    text_muted: Color32::from_rgb(0x88, 0x88, 0xa0),
    // Near-black with a cyan cast rather than pure black: on `#00d4ff` the difference between
    // `#000000` and this is what keeps the label from looking like a hole in the button.
    text_on_accent: Color32::from_rgb(0x05, 0x14, 0x1c),
    accent: Color32::from_rgb(0x00, 0xd4, 0xff),
    accent_hover: Color32::from_rgb(0x33, 0xdd, 0xff),
    accent_active: Color32::from_rgb(0x00, 0xaa, 0xcc),
    positive: Color32::from_rgb(0x00, 0xff, 0x88),
    warning: Color32::from_rgb(0xe3, 0xb3, 0x41),
    danger: Color32::from_rgb(0xff, 0x44, 0x66),
    verified: Color32::from_rgb(0x77, 0xb6, 0xf0),
};

/// Light — the canonical Migo light palette, from `shared/design/tokens.json`.
///
/// Not an inversion of [`DARK`]: the borders carry more of the structure here, because a light
/// interface cannot lean on fill contrast the way a dark one can. The accent is the blue the web
/// client's light theme carries, strong enough to hold white text above the 4.5:1 contrast bar.
const LIGHT: Palette = Palette {
    surface: Color32::from_rgb(0xf0, 0xf2, 0xf5),
    surface_raised: Color32::from_rgb(0xff, 0xff, 0xff),
    surface_overlay: Color32::from_rgb(0xff, 0xff, 0xff),
    surface_hover: Color32::from_rgb(0xe9, 0xed, 0xf3),
    surface_selected: Color32::from_rgb(0xdc, 0xeb, 0xfa),
    border: Color32::from_rgb(0xe0, 0xe3, 0xe8),
    border_strong: Color32::from_rgb(0xc5, 0xca, 0xd0),
    text: Color32::from_rgb(0x1a, 0x1d, 0x24),
    text_muted: Color32::from_rgb(0x5c, 0x63, 0x70),
    text_on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
    accent: Color32::from_rgb(0x00, 0x77, 0xe6),
    accent_hover: Color32::from_rgb(0x1a, 0x86, 0xee),
    accent_active: Color32::from_rgb(0x00, 0x5c, 0xb8),
    positive: Color32::from_rgb(0x00, 0xa8, 0x5a),
    warning: Color32::from_rgb(0xe6, 0xa1, 0x00),
    danger: Color32::from_rgb(0xe0, 0x40, 0x50),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The neon palette as specified, spelled out once so a regression is a diff against the
    /// spec rather than against itself. Only the roles the spec names are asserted; the derived
    /// surfaces (overlay, hover, selected, borders) are free to move as the theme is tuned, and
    /// pinning them here would turn every polish into a test failure.
    #[test]
    fn dark_is_the_neon_palette() {
        let p = palette(Theme::Dark);
        assert_eq!(p.surface, Color32::from_rgb(0x0a, 0x0a, 0x12));
        assert_eq!(p.surface_raised, Color32::from_rgb(0x11, 0x11, 0x18));
        assert_eq!(p.accent, Color32::from_rgb(0x00, 0xd4, 0xff));
        assert_eq!(p.positive, Color32::from_rgb(0x00, 0xff, 0x88));
        assert_eq!(p.danger, Color32::from_rgb(0xff, 0x44, 0x66));
        assert_eq!(p.text, Color32::from_rgb(0xe8, 0xe8, 0xf0));
        assert_eq!(p.text_muted, Color32::from_rgb(0x88, 0x88, 0xa0));
    }

    /// The light theme is the canonical Migo light palette: the same token table the web client
    /// and the Android client read, so one identity follows the account across every device.
    #[test]
    fn light_is_the_canonical_migo_palette() {
        let p = palette(Theme::Light);
        assert_eq!(p.surface, Color32::from_rgb(0xf0, 0xf2, 0xf5));
        assert_eq!(p.surface_raised, Color32::from_rgb(0xff, 0xff, 0xff));
        assert_eq!(p.accent, Color32::from_rgb(0x00, 0x77, 0xe6));
        assert_eq!(p.text, Color32::from_rgb(0x1a, 0x1d, 0x24));
        assert_eq!(p.danger, Color32::from_rgb(0xe0, 0x40, 0x50));
    }

    /// The toggle must remain an involution: flipping twice lands where flipping once did, and
    /// the label always names the theme the button would switch *to*.
    #[test]
    fn flipping_is_an_involution() {
        assert_eq!(Theme::Dark.flipped(), Theme::Light);
        assert_eq!(Theme::Light.flipped(), Theme::Dark);
        assert_eq!(Theme::Dark.flipped().flipped(), Theme::Dark);
        assert_eq!(Theme::Light.label(), "Light");
        assert_eq!(Theme::Dark.label(), "Dark");
    }
}
