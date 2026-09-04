//! Screens, and the narrow context each one is handed.
//!
//! # Why a screen cannot reach the network
//!
//! No screen in this module holds a [`crate::net::Net`]. Each is given a [`Context`] carrying the few
//! read-only facts it needs to draw plus a command buffer to push intent into, and [`crate::app`]
//! drains that buffer after the frame and forwards it.
//!
//! That indirection buys two concrete things. A screen becomes a pure function of state, so its
//! behaviour is decided by what it was handed rather than by what it can reach — and there is nowhere
//! for an `if let Some(gateway)` to appear halfway down a layout function, which is how a paint loop
//! starts making network decisions. And commands are collected and applied *after* the frame, so a
//! click that changes the conversation list cannot mutate the list a later widget in the same frame
//! is still iterating.

pub mod alerts;
pub mod auth;
pub mod captcha;
pub mod chat;
pub mod friends;
pub mod games;
pub mod profile;
pub mod rooms;
pub mod search;
pub mod server_form;
pub mod settings;
pub mod space;
pub mod wallet;
pub mod widgets;

use crate::config::ServerEndpoint;
use crate::model::{Account, Connection};
use crate::net::Command;
use crate::theme::Theme;

/// Which screen is showing.
///
/// The variants are ordered as a user meets them. [`Screen::Opening`] exists because the worker has to
/// look for a vault on disk before anything can be offered: showing a sign-in form and then replacing
/// it with an unlock form a moment later reads as a glitch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Waiting to hear whether a vault exists.
    Opening,
    /// A vault exists; ask for the passphrase.
    Unlock,
    /// Sign in to an existing account on this device.
    SignIn,
    /// Create a new account.
    Register,
    /// Restore an account from a `.migo` container onto this device.
    Restore,
    /// Signed in.
    Chat,
}

/// Which pane a signed-in user is looking at, chosen from the tab strip.
///
/// Deliberately separate from [`Screen`]: the screens are the auth *pipeline* (which form, which
/// gate), while a place is where a signed-in person already is. Folding friends into `Screen`
/// would let the auth flow "navigate" to it, and a sign-out would have to remember to reset it
/// rather than it simply being unreachable without an account.
///
/// The order is the information architecture — the strip's tabs first (Friends, Rooms, Games,
/// Feed), then the panels the account menu opens in the right pane (Alerts, Search, Wallet,
/// Settings). It is the same split the web client's two panes and the Android client's
/// covering screens draw, because it is one product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The social graph: friends, requests, adding by id.
    Friends,
    /// The public room directory and the way in.
    Rooms,
    /// The games the server referees, and where they are played.
    Games,
    /// The activity stream.
    Feed,
    /// The durable notification inbox.
    Alerts,
    /// One box, everything it can honestly find.
    Search,
    /// The MIG balance, the gift shop, the statement, progression, badges, leaderboard.
    Wallet,
    /// The account's own card: display name, bio, custom status, and the privacy of last-seen,
    /// messaging, and friend requests. The account menu's "My Profile", on the right pane.
    Profile,
    /// Server, theme, devices, sign-out.
    Settings,
}

impl Place {
    /// The tab's own word, on the strip and nowhere else.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Friends => "Friends",
            Self::Rooms => "Rooms",
            Self::Games => "Games",
            Self::Feed => "Feed",
            Self::Alerts => "Alerts",
            Self::Search => "Search",
            Self::Wallet => "Wallet",
            Self::Profile => "Profile",
            Self::Settings => "Settings",
        }
    }

    /// The right pane's own word, on its menu bar — the reference calls the credits pane "TopUp".
    #[must_use]
    pub fn right_label(self) -> &'static str {
        match self {
            Self::Wallet => "TopUp",
            other => other.label(),
        }
    }

    /// The four places that are always on the strip, in the reference's order. A conversation
    /// is not one of them: it opens as its own closable tab on the right pane's bar (see the
    /// shell's chat bar), which is the reference's whole model. Feed and Games live here and
    /// nowhere else — the left panel owns them, so the right pane can never draw the same
    /// activity stream a second time. The right pane's panels (Alerts, Search, Wallet,
    /// Settings) are not a strip at all: the banner's account menu opens each on its own.
    pub const SYSTEM_TABS: [Self; 4] = [Self::Friends, Self::Rooms, Self::Games, Self::Feed];

    /// Whether the place is one of the strip's permanent four.
    #[must_use]
    pub fn is_system_tab(self) -> bool {
        Self::SYSTEM_TABS.contains(&self)
    }
}

/// Everything a screen may read, and the things it may write.
pub struct Context<'a> {
    pub theme: Theme,
    pub connection: &'a Connection,
    pub account: Option<&'a Account>,
    /// The server the session lives on, for the settings panel's server section.
    ///
    /// Read-only: changing servers is an auth-form concern, because it means no session exists.
    pub server: &'a ServerEndpoint,
    /// Intent pushed here is forwarded to the worker after the frame.
    pub commands: &'a mut Vec<Command>,
    /// A screen change requested by a link on the screen, applied after the frame.
    ///
    /// Separate from [`Context::commands`] because navigation is not something the worker should hear
    /// about: moving from the sign-in form to the register form touches no socket and no key, and
    /// routing it through the network thread would make the window's responsiveness depend on it.
    pub navigate: &'a mut Option<Screen>,
    /// A theme change requested from a screen, applied after the frame.
    ///
    /// Same reasoning as [`Context::navigate`]: the worker has no business restyling a window, and a
    /// settings panel that had to round-trip the network thread to flip a palette would feel broken
    /// on a bad link.
    pub theme_choice: &'a mut Option<Theme>,
    /// An interface-scale change requested from a screen, applied after the frame.
    ///
    /// Same reasoning as [`Context::theme_choice`]: a zoom is a window's own property, so the
    /// settings panel hands the factor back and the shell applies it where the style lives —
    /// once, between frames, rather than from inside a layout closure mid-draw.
    pub zoom_choice: &'a mut Option<f32>,
}

impl Context<'_> {
    /// Queues one command for the worker.
    pub fn issue(&mut self, command: Command) {
        self.commands.push(command);
    }

    /// Asks to show a different screen once this frame is finished.
    pub fn go(&mut self, screen: Screen) {
        *self.navigate = Some(screen);
    }

    /// Asks to redraw the whole window in the other theme once this frame is finished.
    pub fn want_theme(&mut self, theme: Theme) {
        *self.theme_choice = Some(theme);
    }

    /// Asks to redraw the whole window at another interface scale once this frame is finished.
    pub fn want_zoom(&mut self, zoom: f32) {
        *self.zoom_choice = Some(zoom);
    }
}
