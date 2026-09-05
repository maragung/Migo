//! The signed-in shell: a desktop, floating windows, and a taskbar.
//!
//! # The desktop-OS metaphor
//!
//! The reference's web client is a window manager wearing a web page: a teal desktop, a Contacts
//! panel docked on the left, one floating window per conversation, and a taskbar along the
//! bottom that owns the window list, the balance, the clock and the way out. This module is the
//! honest egui translation of that model — [`egui::Window`] *is* a floating, draggable,
//! resizable, collapsible window, so the shell's work is not inventing windowing but staying out
//! of egui's way:
//!
//! * **Focus and z-order** are egui's own. A click brings a window to the top; [`focus`] and
//!   [`toggle`] only ask egui's memory to move or collapse a layer, and never track a stacking
//!   counter of their own — a second source of truth for "which window is on top" would drift
//!   from the first frame it existed.
//! * **Minimize** is egui's collapse: the window shrinks to its title bar, which is the honest
//!   desktop meaning of the word, and the taskbar reads the same collapsing state the title
//!   bar's own triangle writes ([`is_minimized`]).
//! * **Close** is egui's close button feeding a `&mut bool`, which the app reads after the
//!   frame to drop the window from its open list.
//!
//! What the shell *does* own is membership: which conversation windows are open, which side
//! windows are open, whether the Contacts window is on the desktop, and where a new window is
//! born. That is [`Desktop`], the one struct the app holds between frames. Everything else a
//! window manager remembers — position, size, collapsed-ness, stacking — lives in egui's
//! memory, keyed by the stable ids this module mints, and survives as long as the process does.
//!
//! Nothing here touches the network or a feature module. The windows are chrome; the app fills
//! them with the existing screens, the same `show` functions the old two-pane shell called.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::containers::collapsing_header::CollapsingState;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Galley, Id, LayerId, Layout, Order, Pos2, Sense,
    Stroke, TextStyle, Ui, Vec2,
};
use migo_core::Id as ConversationId;

use crate::theme::{self, font, palette, radius, space, Theme};
use crate::ui::widgets;
use crate::ui::Place;

/// Whether a window is minimized (collapsed to its title bar), reading the same state the
/// window's own collapse button writes.
#[must_use]
pub fn is_minimized(ctx: &egui::Context, window: Id) -> bool {
    !CollapsingState::load_with_default_open(ctx, window.with("collapsing"), true).is_open()
}

/// Whether a window is the active one — the top-most floating layer. egui's own click handling
/// maintains this; the shell only reads it, for the taskbar's active button and for deciding
/// which window Escape owes its attention to.
#[must_use]
pub fn is_active(ctx: &egui::Context, window: Id) -> bool {
    let layer = LayerId::new(Order::Middle, window);
    let top = ctx.memory_mut(|memory| memory.areas_mut().top_layer_id(Order::Middle));
    top == Some(layer)
}

/// Raises a window: un-minimizes it and puts it on top, without stealing anything else from
/// egui's own ordering.
pub fn focus(ctx: &egui::Context, window: Id) {
    let mut collapsing =
        CollapsingState::load_with_default_open(ctx, window.with("collapsing"), true);
    collapsing.set_open(true);
    collapsing.store(ctx);
    ctx.memory_mut(|memory| {
        memory
            .areas_mut()
            .move_to_top(LayerId::new(Order::Middle, window))
    });
}

/// A taskbar button's whole decision, the reference's `toggleWin`: a minimized window restores
/// and raises; the active one minimizes; any other one just raises.
pub fn toggle(ctx: &egui::Context, window: Id) {
    if is_minimized(ctx, window) {
        focus(ctx, window);
        return;
    }
    if is_active(ctx, window) {
        let mut collapsing =
            CollapsingState::load_with_default_open(ctx, window.with("collapsing"), true);
        collapsing.set_open(false);
        collapsing.store(ctx);
        return;
    }
    focus(ctx, window);
}

/// How far each new conversation window steps down-right from the last, and how many steps
/// before the cascade wraps back to the start — the reference's own rhythm, small enough that
/// eight windows still fit on a laptop screen.
const CASCADE_STEP: Vec2 = Vec2::new(26.0, 24.0);
/// How many cascade positions there are before the pattern repeats.
const CASCADE_SLOTS: usize = 8;
/// Where a conversation window is born: clear of the Contacts window's default home on the
/// left, near the top of the desktop — the reference's own first position.
const CHAT_CASCADE_ORIGIN: Pos2 = Pos2::new(296.0, 54.0);
/// The Contacts window's birthplace: the left edge, below the watermark the desktop carries.
pub const CONTACTS_POS: Pos2 = Pos2::new(12.0, 64.0);
/// The Contacts window's first size, the reference's own 360×560.
pub const CONTACTS_SIZE: Vec2 = Vec2::new(360.0, 560.0);
/// A conversation window's first size.
pub const CHAT_SIZE: Vec2 = Vec2::new(560.0, 480.0);
/// A side window's first size — the small floating panes the account menu opens.
pub const SIDE_SIZE: Vec2 = Vec2::new(420.0, 340.0);

/// The whole window-manager state: which windows exist, and nothing about where egui put them.
///
/// Position, size, collapsed-ness and stacking live in egui's memory, keyed by the stable window
/// ids this module mints, and survive as long as the process does. Membership is here because
/// *what is open* is a fact about the session, not about the frame: a sign-out closes every
/// window by dropping this struct's lists, not by asking egui to forget its layer cache.
pub struct Desktop {
    /// Whether the Contacts window is on the desktop. It starts open — it is the shell's home
    /// surface and the way into everything else — and its own close button is allowed to close
    /// it, because the taskbar's Migo button is the way back.
    pub contacts_open: bool,
    /// Which of the Contacts window's tabs is showing.
    pub contacts_tab: Place,
    /// The open conversation windows, in open order. The order is the cascade: a window's
    /// birthplace is derived from its index, so no separate position ledger is needed.
    pub chats: Vec<ConversationId>,
    /// The open side windows (profile, wallet, alerts, and the rest), in open order.
    pub sides: Vec<Place>,
    /// Whether the logout confirmation dialog is up. Logout is the one action the reference
    /// always confirms — the session is the whole state of the shell.
    pub logout_dialog: bool,
    /// When this session started, for the taskbar's timer. `None` until sign-in.
    pub session_start: Option<Instant>,
}

impl Default for Desktop {
    fn default() -> Self {
        Self {
            contacts_open: true,
            contacts_tab: Place::Friends,
            chats: Vec::new(),
            sides: Vec::new(),
            logout_dialog: false,
            session_start: None,
        }
    }
}

impl Desktop {
    /// The state a fresh session starts from: the Contacts window open on Friends, no
    /// conversations, no side windows, no dialog, and the clock running from now.
    pub fn new_session() -> Self {
        Self {
            session_start: Some(Instant::now()),
            ..Self::default()
        }
    }

    /// Registers a conversation window, if it is not open already. Open order is kept — it is
    /// the cascade — but no reordering on refocus: raising a window is egui's business.
    pub fn open_chat(&mut self, conversation_id: ConversationId) {
        if !self.chats.contains(&conversation_id) {
            self.chats.push(conversation_id);
        }
    }

    /// Drops a conversation window: the window's own close, the taskbar, and Escape all land
    /// here, and closing means closing — the conversation's history stays in the store, so
    /// reopening is one click away.
    pub fn close_chat(&mut self, conversation_id: ConversationId) {
        self.chats.retain(|id| *id != conversation_id);
    }

    /// Where the nth open conversation window is born. Wrapping keeps the cascade from walking
    /// new windows off the desktop; egui takes over from the second frame, so this is only ever
    /// a first impression.
    #[must_use]
    pub fn chat_cascade(index: usize) -> Pos2 {
        let step = (index % CASCADE_SLOTS) as f32;
        CHAT_CASCADE_ORIGIN + CASCADE_STEP * step
    }

    /// Where the nth open side window is born — the same rhythm as the conversation cascade, so
    /// the whole desktop shares one idea of where a new window appears.
    #[must_use]
    pub fn side_cascade(index: usize) -> Pos2 {
        let step = (index % CASCADE_SLOTS) as f32;
        CHAT_CASCADE_ORIGIN + CASCADE_STEP * step
    }

    /// Registers a side window, reporting whether it was newly opened — the caller owes the
    /// place its first reads exactly once, and not again for a window that is already showing.
    pub fn open_side(&mut self, place: Place) -> bool {
        if self.sides.contains(&place) {
            false
        } else {
            self.sides.push(place);
            true
        }
    }

    /// Drops a side window.
    pub fn close_side(&mut self, place: Place) {
        self.sides.retain(|open| *open != place);
    }
}

/// The Contacts window's stable id.
#[must_use]
pub fn contacts_id() -> Id {
    Id::new("migo-window-contacts")
}

/// A conversation window's stable id.
#[must_use]
pub fn chat_id(conversation_id: ConversationId) -> Id {
    Id::new("migo-window-chat").with(conversation_id)
}

/// A side window's stable id.
#[must_use]
pub fn side_id(place: Place) -> Id {
    Id::new("migo-window-side").with(place.label())
}

/// One floating window, dressed in the reference's chrome: the teal gloss title bar with white
/// bold text (the active window wears the brighter accent via the theme's `widgets.open`
/// override), a hairline border, and the palette's raised surface behind the contents.
///
/// `default_pos` and `default_size` are birthplaces only — egui keeps whatever the user drags a
/// window to, keyed by `id`, and ignores both from the second frame on.
pub fn floating(
    theme: Theme,
    title: &str,
    id: Id,
    default_pos: Pos2,
    default_size: Vec2,
    min_size: Vec2,
) -> egui::Window<'static> {
    let colors = palette(theme);
    // The gloss title bar rounds its own top corners; the window frame rounds all four, and
    // egui squares the title bar's bottom pair when the window is expanded.
    let gloss = CornerRadius {
        nw: radius::LG,
        ne: radius::LG,
        sw: 0,
        se: 0,
    };
    egui::Window::new(
        egui::RichText::new(title.to_owned())
            .size(font::SMALL)
            .color(colors.text_on_accent)
            .strong(),
    )
    .id(id)
    .title_frame(
        egui::Frame::new()
            .fill(colors.accent_active)
            .corner_radius(gloss)
            .inner_margin(egui::Margin::symmetric(space::SM as i8, 2)),
    )
    .frame(
        egui::Frame::new()
            .fill(colors.surface_raised)
            .stroke(Stroke::new(1.0, colors.border))
            .corner_radius(egui::CornerRadius::same(radius::LG))
            .inner_margin(egui::Margin::same(space::SM as i8)),
    )
    .default_pos(default_pos)
    .default_size(default_size)
    .min_size(min_size)
    .collapsible(true)
}

/// The desktop surface: the ground the windows float on, with the brand standing quietly in
/// the corner the eye lands on first.
///
/// Subtle on purpose — this is a desktop, not a splash screen. The mark is the same diamond the
/// taskbar and the auth screen carry, in translucent white so the surface says "Migo" without
/// shouting it over the windows.
pub fn surface(ui: &mut Ui) {
    let at = ui.max_rect().min + Vec2::new(20.0, 12.0);
    // The diamond, drawn faint.
    let side = 24.0;
    let half = side / 2.0;
    let center = at + Vec2::new(half, half);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x, center.y - half),
            egui::pos2(center.x + half, center.y),
            egui::pos2(center.x, center.y + half),
            egui::pos2(center.x - half, center.y),
        ],
        Color32::from_white_alpha(56),
        Stroke::NONE,
    ));
    // The word beside it, and one honest line under both.
    let word = ui.painter().layout_no_wrap(
        "Migo".to_owned(),
        FontId::proportional(font::SUBTITLE),
        Color32::from_white_alpha(72),
    );
    ui.painter().galley(
        at + Vec2::new(side + space::SM, center.y - word.size().y / 2.0),
        word,
        Color32::WHITE,
    );
    let line = ui.painter().layout_no_wrap(
        "end-to-end encrypted".to_owned(),
        FontId::proportional(font::TINY),
        Color32::from_white_alpha(90),
    );
    ui.painter().galley(
        at + Vec2::new(side + space::SM, center.y + font::SUBTITLE * 0.6),
        line,
        Color32::WHITE,
    );
}

/// One button's-worth of window for the taskbar to draw.
pub struct TaskEntry {
    /// The window's stable id — what a click toggles.
    pub id: Id,
    /// The window's title as the taskbar shows it.
    pub label: String,
    /// The kind word beside the label ("Room", "Chat", "Group"…), the reference's small
    /// uppercase hint about what the button opens. Empty draws nothing.
    pub kind: &'static str,
    /// Unread messages, for the badge a conversation window owes its button.
    pub unread: u32,
}

/// What the taskbar asked the shell for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAction {
    /// Raise, minimize or restore a window.
    Toggle(Id),
    /// Bring the Contacts window back — the Migo button with the window closed.
    ShowContacts,
    /// Open the logout confirmation.
    Logout,
}

/// The taskbar: the fixed dark bar along the bottom of the desktop.
///
/// Carries the reference's things in its order — the brand button (which is the Contacts
/// window's button, the way the reference's logo is), the open-window buttons with their state
/// dots, then the balance chip in gold, the session timer, the logout button and the clock at
/// the right edge. Drawn as a panel before the desktop surface so the surface and the windows
/// know where its edge is; each button's state is read from egui's memory at draw time, so a
/// window egui raised a frame ago is already the active button.
pub fn taskbar(
    ui: &mut Ui,
    theme: Theme,
    contacts_open: bool,
    entries: &[TaskEntry],
    coins: Option<u64>,
    session: Option<Duration>,
) -> Vec<TaskAction> {
    let colors = palette(theme);
    let mut actions = Vec::new();

    // The right cluster is measured before the row is laid out, because the window list scrolls
    // in whatever width is left once the balance, the timer, the logout and the clock have had
    // theirs — a bar whose right end moves with its buttons reads as broken.
    let mut cluster: Vec<(Arc<Galley>, Color32)> = Vec::new();
    if let Some(coins) = coins {
        cluster.push((
            chip_text(ui, &format!("{coins} $MIG"), colors.gold),
            colors.gold,
        ));
    }
    if let Some(elapsed) = session {
        cluster.push((
            chip_text(ui, &session_text(elapsed), colors.text_muted),
            colors.text_muted,
        ));
    }
    let logout = chip_text(ui, "Logout", colors.banner_ink);
    let clock = chip_text(
        ui,
        &crate::model::clock(migo_core::Timestamp::now()),
        colors.banner_ink,
    );

    // The bar's height follows its row of Small text, like every other chrome bar in the client.
    let row = ui.text_style_height(&TextStyle::Small);
    let bar = theme::bar_height(row, space::SM);
    egui::Panel::bottom("taskbar")
        .exact_size(bar)
        .frame(egui::Frame::new().fill(colors.nav))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                brand_button(ui, theme, contacts_open, &mut actions);
                ui.add_space(space::XS);

                // The window list: the leftover width between the brand and the right cluster,
                // scrolling sideways rather than hiding anything, because a window that is
                // off-screen is still a window.
                let cluster_width: f32 = cluster
                    .iter()
                    .map(|(galley, _)| chip_width(galley))
                    .sum::<f32>()
                    + chip_width(&logout)
                    + chip_width(&clock)
                    + ui.spacing().item_spacing.x;
                let list_width = (ui.available_width() - cluster_width).max(0.0);
                egui::ScrollArea::horizontal()
                    .id_salt("task-buttons")
                    .max_width(list_width)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            for entry in entries {
                                let minimized = is_minimized(ui.ctx(), entry.id);
                                let active = is_active(ui.ctx(), entry.id);
                                if task_button(ui, theme, entry, minimized, active) {
                                    actions.push(TaskAction::Toggle(entry.id));
                                }
                            }
                        });
                    });

                // The right cluster, right to left: the clock ends up outermost, then the way
                // out, then the facts about the session.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    draw_chip(ui, clock, colors.banner_ink);
                    if logout_button(ui, theme, logout) {
                        actions.push(TaskAction::Logout);
                    }
                    for (galley, ink) in cluster {
                        draw_chip(ui, galley, ink);
                    }
                });
            });
        });

    actions
}

/// The Migo brand as the taskbar's own button: the painted diamond and the word, in ink on the
/// bar. It doubles as the Contacts window's home button — one click brings the lists back,
/// whatever else is open, and the window's own task button is beside it when it already is.
fn brand_button(ui: &mut Ui, theme: Theme, contacts_open: bool, actions: &mut Vec<TaskAction>) {
    let colors = palette(theme);
    let word = ui.painter().layout_no_wrap(
        "Migo".to_owned(),
        FontId::proportional(font::SMALL),
        colors.banner_ink,
    );
    let size = Vec2::new(
        space::XS + 16.0 + space::XS + word.size().x + space::XS,
        24.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() {
        Color32::from_white_alpha(26)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    }
    // The diamond, painted the way the brand mark always is: U+25C6 is tofu in this font stack.
    let half = 8.0;
    let center = egui::pos2(rect.left() + space::XS + half, rect.center().y);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x, center.y - half),
            egui::pos2(center.x + half, center.y),
            egui::pos2(center.x, center.y + half),
            egui::pos2(center.x - half, center.y),
        ],
        colors.accent,
        Stroke::NONE,
    ));
    ui.painter().galley(
        egui::pos2(
            rect.left() + space::XS + 16.0 + space::XS,
            rect.center().y - word.size().y / 2.0,
        ),
        word,
        colors.banner_ink,
    );
    if response.clicked() {
        if contacts_open {
            actions.push(TaskAction::Toggle(contacts_id()));
        } else {
            actions.push(TaskAction::ShowContacts);
        }
    }
}

/// One window's task button: the state dot, the label, the kind word, and the unread badge —
/// the active window in the accent teal, a minimized one pale, the rest quiet.
fn task_button(
    ui: &mut Ui,
    theme: Theme,
    entry: &TaskEntry,
    minimized: bool,
    active: bool,
) -> bool {
    let colors = palette(theme);
    let ink = if active {
        colors.text_on_accent
    } else {
        Color32::from_white_alpha(210)
    };
    // The label is elided before it is measured: a task button does not grow to fit a title, it
    // truncates one, the same way the reference's own 120px cap does.
    let label = ui.painter().layout_no_wrap(
        widgets::elide(&entry.label, 18),
        FontId::proportional(font::SMALL),
        ink,
    );
    let kind = (!entry.kind.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(entry.kind.to_owned(), FontId::proportional(font::TINY), ink)
    });
    let badge = (entry.unread > 0).then(|| {
        let text = if entry.unread > 99 {
            "99+".to_owned()
        } else {
            entry.unread.to_string()
        };
        ui.painter().layout_no_wrap(
            text,
            FontId::proportional(font::TINY),
            colors.text_on_accent,
        )
    });

    let dot_room = 7.0 + 6.0;
    // The galleys are moved into the painter below, so their sizes are read first — a button is
    // laid out from measurements, and the measurements must outlive the drawing.
    let label_size = label.size();
    let kind_size = kind.as_ref().map(|galley| galley.size());
    let badge_size = badge.as_ref().map(|galley| galley.size());
    let mut width = dot_room + label_size.x + 2.0 * space::SM;
    if let Some(size) = &kind_size {
        width += space::XS + size.x;
    }
    if let Some(size) = &badge_size {
        width += space::XS + size.x + 2.0 * space::XS;
    }
    let height = 24.0_f32.max(label_size.y + 2.0 * 4.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

    let fill = if active {
        colors.accent
    } else if response.hovered() {
        Color32::from_white_alpha(38)
    } else {
        Color32::from_white_alpha(18)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);

    // The state dot: green while the window stands, pale when it is minimized to its bar — the
    // reference's own two colours for the same two states.
    ui.painter().circle_filled(
        egui::pos2(rect.left() + space::SM + 3.5, rect.center().y),
        3.5,
        if minimized {
            Color32::from_white_alpha(130)
        } else {
            colors.positive
        },
    );

    let at = rect.left() + space::SM + dot_room;
    ui.painter().galley(
        egui::pos2(at, rect.center().y - label_size.y / 2.0),
        label,
        ink,
    );
    if let (Some(kind), Some(size)) = (kind, kind_size) {
        ui.painter().galley(
            egui::pos2(
                at + label_size.x + space::XS,
                rect.center().y - size.y / 2.0,
            ),
            kind,
            Color32::from_white_alpha(140),
        );
    }
    if let (Some(badge), Some(size)) = (badge, badge_size) {
        let corner = egui::Rect::from_min_max(
            egui::pos2(
                rect.right() - space::SM - size.x - 2.0 * space::XS,
                rect.center().y - size.y / 2.0 - space::XS * 0.75,
            ),
            egui::pos2(
                rect.right() - space::SM,
                rect.center().y + size.y / 2.0 + space::XS * 0.75,
            ),
        );
        ui.painter()
            .rect_filled(corner, CornerRadius::same(radius::FULL), colors.danger);
        ui.painter().galley(
            corner.min + Vec2::splat(space::XS),
            badge,
            colors.text_on_accent,
        );
    }

    response.clicked()
}

/// The logout button: the one destructive control on the bar, a raised tile on the nav teal
/// that brightens when it is aimed at.
fn logout_button(ui: &mut Ui, theme: Theme, label: Arc<Galley>) -> bool {
    let colors = palette(theme);
    let size = label.size() + Vec2::new(2.0 * space::SM, 2.0 * 3.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        colors.accent
    } else if response.hovered() {
        colors.accent_hover
    } else {
        colors.accent_active
    };
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    ui.painter().galley(
        egui::pos2(
            rect.left() + space::SM,
            rect.center().y - label.size().y / 2.0,
        ),
        label,
        colors.banner_ink,
    );
    response.clicked()
}

/// Lays out a chip's text in the bar's own type size.
fn chip_text(ui: &Ui, text: &str, ink: Color32) -> Arc<Galley> {
    ui.painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(font::SMALL), ink)
}

/// How much bar a chip's galley occupies, including its padding and the row's spacing.
fn chip_width(galley: &Galley) -> f32 {
    galley.size().x + 2.0 * space::SM + space::SM
}

/// A small fixed chip on the bar: the clock, the timer, the balance — a dark inset on the nav
/// teal, the reference's own reading of "a fact, stated".
fn draw_chip(ui: &mut Ui, galley: Arc<Galley>, ink: Color32) {
    let size = galley.size() + Vec2::new(2.0 * space::SM, 2.0 * 3.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), Color32::from_black_alpha(70));
    ui.painter().galley(
        egui::pos2(
            rect.left() + space::SM,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        ink,
    );
}

/// The session timer as the reference writes it: minutes until there is an hour to name.
fn session_text(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h{}m", minutes / 60, minutes % 60)
    }
}

/// The logout confirmation: a small centered window with the gloss title bar, the reference's
/// `ConfirmDialog` shape. Returns true on the frame the user confirmed.
///
/// Centered and anchored rather than draggable — a question that can be dragged away is a
/// question that can be ignored, and this one gates the whole session. Escape cancels, which is
/// the same answer the reference's dialog gives the key.
pub fn logout_dialog(ctx: &egui::Context, theme: Theme, open: &mut bool) -> bool {
    if !*open {
        return false;
    }
    let colors = palette(theme);
    let mut confirmed = false;
    // The dialog is a question, so it carries no close (X) of its own: Cancel, Escape and the
    // confirm are the whole answer set, and a question that can be X-ed away without being
    // answered is a question that can be ignored.
    let mut window_open = true;

    floating(
        theme,
        "Logout",
        Id::new("migo-logout-dialog"),
        Pos2::ZERO,
        Vec2::new(340.0, 0.0),
        Vec2::new(340.0, 0.0),
    )
    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
    .order(Order::Foreground)
    .resizable(false)
    .collapsible(false)
    .show(ctx, |ui| {
        ui.add_space(space::SM);
        ui.label(
            egui::RichText::new("Sign out of Migo on this device?")
                .font(FontId::proportional(font::BODY))
                .color(colors.text)
                .strong(),
        );
        ui.add_space(space::XS);
        ui.label(
            egui::RichText::new(
                "Every open window closes with the session. The vault stays locked on disk until \
                 you sign in again.",
            )
            .font(FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
        ui.add_space(space::MD);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::ghost_button(ui, theme, "Cancel").clicked() {
                window_open = false;
            }
            ui.add_space(space::SM);
            if confirm_button(ui, theme, "Log out").clicked() {
                confirmed = true;
                window_open = false;
            }
        });
    });

    // Escape cancels, the same answer the reference's dialog gives the key.
    let escaped = ctx.input(|input| input.key_pressed(egui::Key::Escape));
    if !window_open || escaped {
        *open = false;
    }
    confirmed
}

/// The confirm half of the dialog: the banner orange, because "yes" to leaving is still a
/// primary action, and the palette already names that colour.
fn confirm_button(ui: &mut Ui, theme: Theme, text: &str) -> egui::Response {
    let colors = palette(theme);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::proportional(font::BODY),
        colors.banner_ink,
    );
    let size = galley.size() + Vec2::new(2.0 * space::LG, 2.0 * space::SM);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        colors.banner_a
    } else if response.hovered() {
        colors.banner_c
    } else {
        colors.banner_b
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::MD), fill);
    ui.painter().galley(
        egui::pos2(
            rect.left() + space::LG,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        colors.banner_ink,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cascade wraps: the ninth conversation window shares a birthplace with the first, and
    /// no two neighbouring positions coincide. A window manager that stepped forever would walk
    /// new windows off the desktop.
    #[test]
    fn cascade_positions_wrap() {
        let first = Desktop::chat_cascade(0);
        assert_eq!(first, Desktop::chat_cascade(CASCADE_SLOTS));
        assert_ne!(first, Desktop::chat_cascade(1));
        assert_eq!(
            Desktop::side_cascade(0),
            Desktop::side_cascade(CASCADE_SLOTS)
        );
        // The first position is the reference's own opening position.
        assert_eq!(first, CHAT_CASCADE_ORIGIN);
    }

    /// The session timer names minutes until an hour, then hours-and-minutes — the reference's
    /// own two shapes, boundary included.
    #[test]
    fn session_text_names_its_units() {
        assert_eq!(session_text(Duration::from_secs(0)), "0m");
        assert_eq!(session_text(Duration::from_secs(59 * 60)), "59m");
        assert_eq!(session_text(Duration::from_secs(60 * 60)), "1h0m");
        assert_eq!(session_text(Duration::from_secs(61 * 60)), "1h1m");
    }
}
