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
//! # The flat modern identity: teal and orange
//!
//! Both themes speak the reference's language: teal surfaces and accents, an orange banner that
//! owns the account, and the same flat turquoise ground behind the sign-in card in either theme
//! — the sign-in screen is the one place that is allowed to look identical in dark and light,
//! because it is the front door and the front door does not change with the lights. Dark is the
//! same family taken deep (`#072A33` window, `#1FA5C0` accent); light is the reference's soft
//! teal ground (`#EEF7FA`) with `#1287A0` doing the same job. The restyle is flat by decree:
//! solid colours only, separation from 1px borders and one soft elevation shadow — never a
//! gradient, a bevel or a text shadow. The banner and login triples are three equal stops, so
//! the gradient call sites paint flat bands without knowing it.
//!
//! The signed-in shell extends the same idea to the desktop-OS metaphor: the desktop surface is
//! the reference's turquoise (`#0F96AD`) in light and a deep teal of the same hue in dark, the
//! taskbar is the nav teal (`#0D4353` in light), and every floating window's title bar is the
//! accent teal with white bold text — the reference's `gloss-title`.

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
    /// A brighter accent, one step up from [`Palette::accent`]. The flat restyle's active tab chip
    /// is white rather than accent-filled, so no widget reads this any more — it stays (the spec
    /// names it) and the palette tests pin its value, hence the allow.
    #[allow(dead_code)] // read only by the cfg(test) palette tests
    pub accent_bright: Color32,
    /// The navigation strip's own teal — what the tab chips sit on.
    pub nav: Color32,
    /// The profile banner's band, left to right: three equal stops of the flat orange.
    pub banner_a: Color32,
    pub banner_b: Color32,
    pub banner_c: Color32,
    /// Ink on the banner and on the orange primaries: white in both themes.
    pub banner_ink: Color32,
    /// The login screen's flat turquoise ground: three equal stops, theme-independent.
    pub login_a: Color32,
    pub login_b: Color32,
    pub login_c: Color32,
    /// The signed-in desktop surface — the ground the floating windows sit on. The reference's
    /// flat turquoise in light; the same hue taken deep in dark, so the desktop reads as one
    /// family with the taskbar in either theme.
    pub desktop: Color32,
    /// The wallet's coin colour, on the taskbar's balance chip and wherever a balance is stated.
    /// Gold rather than the accent teal, because the balance is a number about money and the
    /// teal is a colour about the interface.
    pub gold: Color32,
    /// Success, and "connected".
    pub positive: Color32,
    /// A warning, and "connecting".
    pub warning: Color32,
    /// An error, and "disconnected".
    pub danger: Color32,
    /// The verified-key indicator, deliberately distinct from `positive`.
    pub verified: Color32,
}

/// Dark — the flat modern Migo dark palette, deep teal in the reference's family.
///
/// Deep-teal surfaces with a `#1FA5C0` accent, so the accent is the brightest saturated thing on
/// screen by a wide margin. Green is reserved for presence and success, red for failure; neither
/// ever marks a selection or a primary action, because a user must not have to learn which
/// "bright colour" means what.
const DARK: Palette = Palette {
    surface: Color32::from_rgb(0x07, 0x2a, 0x33),
    surface_raised: Color32::from_rgb(0x0c, 0x3a, 0x46),
    surface_overlay: Color32::from_rgb(0x11, 0x4b, 0x5a),
    surface_hover: Color32::from_rgb(0x10, 0x40, 0x4e),
    surface_selected: Color32::from_rgb(0x14, 0x56, 0x6a),
    border: Color32::from_rgb(0x1a, 0x58, 0x66),
    border_strong: Color32::from_rgb(0x2a, 0x72, 0x85),
    text: Color32::from_rgb(0xe6, 0xf4, 0xf8),
    text_muted: Color32::from_rgb(0xa3, 0xc4, 0xcd),
    text_on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
    accent: Color32::from_rgb(0x1f, 0xa5, 0xc0),
    accent_hover: Color32::from_rgb(0x34, 0xbf, 0xd8),
    accent_active: Color32::from_rgb(0x15, 0x7e, 0x94),
    accent_bright: Color32::from_rgb(0x34, 0xbf, 0xd8),
    nav: Color32::from_rgb(0x06, 0x22, 0x2a),
    banner_a: Color32::from_rgb(0xf5, 0x82, 0x0c),
    banner_b: Color32::from_rgb(0xf5, 0x82, 0x0c),
    banner_c: Color32::from_rgb(0xf5, 0x82, 0x0c),
    banner_ink: Color32::from_rgb(0xff, 0xff, 0xff),
    login_a: Color32::from_rgb(0x0f, 0x96, 0xad),
    login_b: Color32::from_rgb(0x0f, 0x96, 0xad),
    login_c: Color32::from_rgb(0x0f, 0x96, 0xad),
    desktop: Color32::from_rgb(0x0a, 0x3f, 0x4e),
    gold: Color32::from_rgb(0xf0, 0xa9, 0x12),
    positive: Color32::from_rgb(0x3f, 0xce, 0x6b),
    warning: Color32::from_rgb(0xf5, 0xb8, 0x3d),
    danger: Color32::from_rgb(0xe5, 0x50, 0x3c),
    verified: Color32::from_rgb(0x34, 0xbf, 0xd8),
};

/// Light — the flat modern Migo light palette, the reference's canonical one.
///
/// The soft teal ground (`#EEF7FA`) with white raised surfaces and the teal `#1287A0` as the
/// accent, strong enough to hold white text above the 4.5:1 contrast bar. The 1px borders carry
/// more of the structure here, because a light interface cannot lean on fill contrast the way a
/// dark one can.
const LIGHT: Palette = Palette {
    surface: Color32::from_rgb(0xee, 0xf7, 0xfa),
    surface_raised: Color32::from_rgb(0xff, 0xff, 0xff),
    surface_overlay: Color32::from_rgb(0xe5, 0xf4, 0xf7),
    surface_hover: Color32::from_rgb(0xe9, 0xf7, 0xfa),
    surface_selected: Color32::from_rgb(0xd9, 0xee, 0xf3),
    border: Color32::from_rgb(0xcf, 0xe3, 0xea),
    border_strong: Color32::from_rgb(0xbf, 0xdf, 0xe6),
    text: Color32::from_rgb(0x13, 0x4e, 0x5e),
    text_muted: Color32::from_rgb(0x5f, 0x8a, 0x99),
    text_on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
    accent: Color32::from_rgb(0x12, 0x87, 0xa0),
    accent_hover: Color32::from_rgb(0x1b, 0x99, 0xb3),
    accent_active: Color32::from_rgb(0x0e, 0x71, 0x89),
    accent_bright: Color32::from_rgb(0x1b, 0x99, 0xb3),
    nav: Color32::from_rgb(0x0d, 0x43, 0x53),
    banner_a: Color32::from_rgb(0xf5, 0x82, 0x0c),
    banner_b: Color32::from_rgb(0xf5, 0x82, 0x0c),
    banner_c: Color32::from_rgb(0xf5, 0x82, 0x0c),
    banner_ink: Color32::from_rgb(0xff, 0xff, 0xff),
    login_a: Color32::from_rgb(0x0f, 0x96, 0xad),
    login_b: Color32::from_rgb(0x0f, 0x96, 0xad),
    login_c: Color32::from_rgb(0x0f, 0x96, 0xad),
    desktop: Color32::from_rgb(0x0f, 0x96, 0xad),
    gold: Color32::from_rgb(0xf0, 0xa9, 0x12),
    positive: Color32::from_rgb(0x3f, 0xce, 0x6b),
    warning: Color32::from_rgb(0xf5, 0xb8, 0x3d),
    danger: Color32::from_rgb(0xe5, 0x50, 0x3c),
    verified: Color32::from_rgb(0x1d, 0x9c, 0xb5),
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
    /// Windows read at 12px in the reference, so the large radius matches it.
    pub const LG: u8 = 12;
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

/// A chrome bar's height: the full row height of the text it carries, plus breathing room.
///
/// The bars used to hardcode pixel heights tuned to one type scale — 38 for the right pane's
/// bars, 46 for the strip, 58 for the banner — which clipped the moment the type grew and gave
/// nothing back when it shrank. Asking the font how tall its row is keeps a bar honest about
/// what it has to fit. The result is in points; the interface zoom (the settings panel's scale
/// control) scales it with everything else, so a bar's *proportion* lives here and a device's
/// *absolute* size lives in the zoom.
///
/// `pad` is the breathing room above and below the row, so a two-row bar passes the sum of its
/// rows and one pad that already counts twice.
#[must_use]
pub fn bar_height(rows: f32, pad: f32) -> f32 {
    rows + 2.0 * pad
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
    // Open: a combo box or menu that is showing its contents — and, by a coincidence egui's
    // window machinery bakes in, the title bar of the top-most floating window: the active
    // window's title bar is repainted in `widgets.open.weak_bg_fill`, over whatever the window's
    // own title frame said. The accent is what that override wants to say — the active window
    // wears the brighter teal, the inactive ones the frame's darker one — and an open combo box
    // taking the same teal is the same family, not a clash.
    w.open.bg_fill = p.surface_overlay;
    w.open.weak_bg_fill = p.accent;
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

    /// The flat modern dark palette as specified, spelled out once so a regression is a diff
    /// against the spec rather than against itself. Only the roles the spec names are asserted;
    /// the derived surfaces (hover, selected, borders) are free to move as the theme is tuned,
    /// and pinning them here would turn every polish into a test failure.
    #[test]
    fn dark_is_the_reference_palette() {
        let p = palette(Theme::Dark);
        assert_eq!(p.surface, Color32::from_rgb(0x07, 0x2a, 0x33));
        assert_eq!(p.surface_raised, Color32::from_rgb(0x0c, 0x3a, 0x46));
        assert_eq!(p.accent, Color32::from_rgb(0x1f, 0xa5, 0xc0));
        assert_eq!(p.accent_bright, Color32::from_rgb(0x34, 0xbf, 0xd8));
        assert_eq!(p.nav, Color32::from_rgb(0x06, 0x22, 0x2a));
        assert_eq!(p.desktop, Color32::from_rgb(0x0a, 0x3f, 0x4e));
        assert_eq!(p.gold, Color32::from_rgb(0xf0, 0xa9, 0x12));
        assert_eq!(p.positive, Color32::from_rgb(0x3f, 0xce, 0x6b));
        assert_eq!(p.danger, Color32::from_rgb(0xe5, 0x50, 0x3c));
        assert_eq!(p.text, Color32::from_rgb(0xe6, 0xf4, 0xf8));
        assert_eq!(p.text_muted, Color32::from_rgb(0xa3, 0xc4, 0xcd));
    }

    /// The light theme is the canonical flat modern Migo light palette: the same token table the
    /// web client and the Android client read, so one identity follows the account across every
    /// device — soft teal ground, white raised surfaces, `#1287A0` accent, `#0D4353` nav.
    #[test]
    fn light_is_the_canonical_migo_palette() {
        let p = palette(Theme::Light);
        assert_eq!(p.surface, Color32::from_rgb(0xee, 0xf7, 0xfa));
        assert_eq!(p.surface_raised, Color32::from_rgb(0xff, 0xff, 0xff));
        assert_eq!(p.accent, Color32::from_rgb(0x12, 0x87, 0xa0));
        assert_eq!(p.accent_bright, Color32::from_rgb(0x1b, 0x99, 0xb3));
        assert_eq!(p.nav, Color32::from_rgb(0x0d, 0x43, 0x53));
        assert_eq!(p.desktop, Color32::from_rgb(0x0f, 0x96, 0xad));
        assert_eq!(p.gold, Color32::from_rgb(0xf0, 0xa9, 0x12));
        assert_eq!(p.text, Color32::from_rgb(0x13, 0x4e, 0x5e));
        assert_eq!(p.danger, Color32::from_rgb(0xe5, 0x50, 0x3c));
    }

    /// The banner and the login ground are the reference's own flat colours — every stop in each
    /// triple is equal, so no gradient can appear — and they are deliberately theme-independent:
    /// the front door does not change with the lights.
    #[test]
    fn the_banner_and_login_gradients_ignore_the_theme() {
        for theme in [Theme::Dark, Theme::Light] {
            let p = palette(theme);
            assert_eq!(p.banner_a, Color32::from_rgb(0xf5, 0x82, 0x0c));
            assert_eq!(p.banner_b, Color32::from_rgb(0xf5, 0x82, 0x0c));
            assert_eq!(p.banner_c, Color32::from_rgb(0xf5, 0x82, 0x0c));
            assert_eq!(p.banner_ink, Color32::from_rgb(0xff, 0xff, 0xff));
            assert_eq!(p.login_a, Color32::from_rgb(0x0f, 0x96, 0xad));
            assert_eq!(p.login_b, Color32::from_rgb(0x0f, 0x96, 0xad));
            assert_eq!(p.login_c, Color32::from_rgb(0x0f, 0x96, 0xad));
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
