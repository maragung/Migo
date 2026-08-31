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
//! # The v3 identity: teal and orange
//!
//! Both themes speak the reference's language (docs/design/new-client-ui.tsx): cyan-teal
//! surfaces and accents, an orange banner that owns the account, and the same login gradient in
//! either theme — the sign-in screen is the one place that is allowed to look identical in dark
//! and light, because it is the front door and the front door does not change with the lights.
//! Dark is a deep teal (`#0c1517` window, `#00BCD4` accent) rather than a neutral grey scale, so
//! the accent stays the brightest saturated thing on screen; light is the reference's cream
//! (`#fdfbf7`) with the teal `#00838F` doing the same job.

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
    /// The one brand colour. Primary actions that are not the banner's, the caret, the outgoing bubble.
    pub accent: Color32,
    /// A hovered accent surface.
    pub accent_hover: Color32,
    /// A pressed accent surface.
    pub accent_active: Color32,
    /// The brighter accent the active tab chip wears on the nav strip.
    pub accent_bright: Color32,
    /// The navigation strip's own teal — what the tab chips sit on.
    pub nav: Color32,
    /// The profile banner's gradient, left to right: deep orange, orange, amber.
    pub banner_a: Color32,
    pub banner_b: Color32,
    pub banner_c: Color32,
    /// Ink on the banner and on the orange primaries: white in both themes.
    pub banner_ink: Color32,
    /// The login screen's vertical gradient: cyan, bright cyan, teal. Theme-independent.
    pub login_a: Color32,
    pub login_b: Color32,
    pub login_c: Color32,
    /// Success, and "connected".
    pub positive: Color32,
    /// A warning, and "connecting".
    pub warning: Color32,
    /// An error, and "disconnected".
    pub danger: Color32,
    /// The verified-key indicator, deliberately distinct from `positive`.
    pub verified: Color32,
}

/// Dark — the canonical Migo dark palette, from `shared/design/tokens.json` v3.
///
/// Deep-teal surfaces with a cyan accent (`#00BCD4`), so the accent is the brightest saturated
/// thing on screen by a wide margin. Green is reserved for presence and success, red for failure;
/// neither ever marks a selection or a primary action, because a user must not have to learn
/// which "bright colour" means what.
const DARK: Palette = Palette {
    surface: Color32::from_rgb(0x0c, 0x15, 0x17),
    surface_raised: Color32::from_rgb(0x12, 0x20, 0x23),
    surface_overlay: Color32::from_rgb(0x1a, 0x2c, 0x30),
    surface_hover: Color32::from_rgb(0x15, 0x28, 0x2c),
    surface_selected: Color32::from_rgb(0x0e, 0x30, 0x38),
    border: Color32::from_rgb(0x24, 0x39, 0x3e),
    border_strong: Color32::from_rgb(0x35, 0x51, 0x58),
    text: Color32::from_rgb(0xe9, 0xf4, 0xf5),
    text_muted: Color32::from_rgb(0x9d, 0xb4, 0xb8),
    text_on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
    accent: Color32::from_rgb(0x00, 0xbc, 0xd4),
    accent_hover: Color32::from_rgb(0x26, 0xc6, 0xda),
    accent_active: Color32::from_rgb(0x00, 0x93, 0xaf),
    accent_bright: Color32::from_rgb(0x26, 0xc6, 0xda),
    nav: Color32::from_rgb(0x0f, 0x3a, 0x40),
    banner_a: Color32::from_rgb(0xea, 0x58, 0x0c),
    banner_b: Color32::from_rgb(0xf9, 0x73, 0x16),
    banner_c: Color32::from_rgb(0xf5, 0x9e, 0x0b),
    banner_ink: Color32::from_rgb(0xff, 0xff, 0xff),
    login_a: Color32::from_rgb(0x00, 0x93, 0xaf),
    login_b: Color32::from_rgb(0x00, 0xac, 0xc1),
    login_c: Color32::from_rgb(0x00, 0x83, 0x8f),
    positive: Color32::from_rgb(0x2f, 0xce, 0x7e),
    warning: Color32::from_rgb(0xf5, 0x9f, 0x00),
    danger: Color32::from_rgb(0xff, 0x5c, 0x7a),
    verified: Color32::from_rgb(0x74, 0xc0, 0xfc),
};

/// Light — the canonical Migo light palette, from `shared/design/tokens.json` v3.
///
/// The reference's cream (`#fdfbf7`) with the teal `#00838F` as the accent, strong enough to
/// hold white text above the 4.5:1 contrast bar. The borders carry more of the structure here,
/// because a light interface cannot lean on fill contrast the way a dark one can.
const LIGHT: Palette = Palette {
    surface: Color32::from_rgb(0xfd, 0xfb, 0xf7),
    surface_raised: Color32::from_rgb(0xff, 0xff, 0xff),
    surface_overlay: Color32::from_rgb(0xf5, 0xf1, 0xe8),
    surface_hover: Color32::from_rgb(0xef, 0xe9, 0xdb),
    surface_selected: Color32::from_rgb(0xdc, 0xee, 0xf1),
    border: Color32::from_rgb(0xe8, 0xe2, 0xd4),
    border_strong: Color32::from_rgb(0xd3, 0xca, 0xb4),
    text: Color32::from_rgb(0x1e, 0x2b, 0x2e),
    text_muted: Color32::from_rgb(0x5c, 0x6a, 0x6d),
    text_on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
    accent: Color32::from_rgb(0x00, 0x83, 0x8f),
    accent_hover: Color32::from_rgb(0x00, 0xac, 0xc1),
    accent_active: Color32::from_rgb(0x00, 0x60, 0x6b),
    accent_bright: Color32::from_rgb(0x00, 0xac, 0xc1),
    nav: Color32::from_rgb(0x00, 0x83, 0x8f),
    banner_a: Color32::from_rgb(0xea, 0x58, 0x0c),
    banner_b: Color32::from_rgb(0xf9, 0x73, 0x16),
    banner_c: Color32::from_rgb(0xf5, 0x9e, 0x0b),
    banner_ink: Color32::from_rgb(0xff, 0xff, 0xff),
    login_a: Color32::from_rgb(0x00, 0x93, 0xaf),
    login_b: Color32::from_rgb(0x00, 0xac, 0xc1),
    login_c: Color32::from_rgb(0x00, 0x83, 0x8f),
    positive: Color32::from_rgb(0x05, 0x96, 0x69),
    warning: Color32::from_rgb(0xe6, 0x77, 0x00),
    danger: Color32::from_rgb(0xe0, 0x31, 0x31),
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
    /// A tab chip on the navigation strip — the reference's `tabChip` token.
    pub const TAB: u8 = 12;
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

    /// The v3 palette as specified (shared/design/tokens.json), spelled out once so a regression
    /// is a diff against the spec rather than against itself. Only the roles the spec names are
    /// asserted; the derived surfaces (hover, selected, borders) are free to move as the theme is
    /// tuned, and pinning them here would turn every polish into a test failure.
    #[test]
    fn dark_is_the_reference_palette() {
        let p = palette(Theme::Dark);
        assert_eq!(p.surface, Color32::from_rgb(0x0c, 0x15, 0x17));
        assert_eq!(p.surface_raised, Color32::from_rgb(0x12, 0x20, 0x23));
        assert_eq!(p.accent, Color32::from_rgb(0x00, 0xbc, 0xd4));
        assert_eq!(p.nav, Color32::from_rgb(0x0f, 0x3a, 0x40));
        assert_eq!(p.positive, Color32::from_rgb(0x2f, 0xce, 0x7e));
        assert_eq!(p.danger, Color32::from_rgb(0xff, 0x5c, 0x7a));
        assert_eq!(p.text, Color32::from_rgb(0xe9, 0xf4, 0xf5));
        assert_eq!(p.text_muted, Color32::from_rgb(0x9d, 0xb4, 0xb8));
    }

    /// The light theme is the canonical Migo light palette: the same token table the web client
    /// and the Android client read, so one identity follows the account across every device.
    #[test]
    fn light_is_the_canonical_migo_palette() {
        let p = palette(Theme::Light);
        assert_eq!(p.surface, Color32::from_rgb(0xfd, 0xfb, 0xf7));
        assert_eq!(p.surface_raised, Color32::from_rgb(0xff, 0xff, 0xff));
        assert_eq!(p.accent, Color32::from_rgb(0x00, 0x83, 0x8f));
        assert_eq!(p.text, Color32::from_rgb(0x1e, 0x2b, 0x2e));
        assert_eq!(p.danger, Color32::from_rgb(0xe0, 0x31, 0x31));
    }

    /// The banner and the login gradient are the reference's own colours, and they are
    /// deliberately theme-independent: the front door does not change with the lights.
    #[test]
    fn the_banner_and_login_gradients_ignore_the_theme() {
        for theme in [Theme::Dark, Theme::Light] {
            let p = palette(theme);
            assert_eq!(p.banner_a, Color32::from_rgb(0xea, 0x58, 0x0c));
            assert_eq!(p.banner_b, Color32::from_rgb(0xf9, 0x73, 0x16));
            assert_eq!(p.banner_c, Color32::from_rgb(0xf5, 0x9e, 0x0b));
            assert_eq!(p.banner_ink, Color32::from_rgb(0xff, 0xff, 0xff));
            assert_eq!(p.login_a, Color32::from_rgb(0x00, 0x93, 0xaf));
            assert_eq!(p.login_b, Color32::from_rgb(0x00, 0xac, 0xc1));
            assert_eq!(p.login_c, Color32::from_rgb(0x00, 0x83, 0x8f));
        }
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
