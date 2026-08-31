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
    /// Signed in.
    Chat,
}

/// Which pane a signed-in user is looking at, chosen from the top navigation bar.
///
/// Deliberately separate from [`Screen`]: the screens are the auth *pipeline* (which form, which
/// gate), while a place is where a signed-in person already is. Folding friends into `Screen`
/// would let the auth flow "navigate" to it, and a sign-out would have to remember to reset it
/// rather than it simply being unreachable without an account.
///
/// The order is the information architecture — the same list the web client's rail and the
/// Android client's bottom bar carry, in the same order, because it is one product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Conversations and threads — where a session starts, per the messenger-first spec.
    Chat,
    /// The public room directory and the way in.
    Rooms,
    /// The activity stream.
    Space,
    /// The social graph: friends, requests, adding by id.
    Friends,
    /// The durable notification inbox.
    Alerts,
    /// One box, everything it can honestly find.
    Search,
    /// The MIG balance, the gift shop, the statement, progression, badges, leaderboard.
    Wallet,
    /// Server, theme, devices, sign-out.
    Settings,
}

impl Place {
    /// The tab's own word, in the bar and nowhere else.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chats",
            Self::Rooms => "Rooms",
            Self::Space => "Space",
            Self::Friends => "Friends",
            Self::Alerts => "Alerts",
            Self::Search => "Search",
            Self::Wallet => "Wallet",
            Self::Settings => "Settings",
        }
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
    /// A place change requested by a link on the screen, applied after the frame.
    ///
    /// The same reasoning as [`Context::navigate`] again: a search hit that opens a chat moves the
    /// window, not the socket, and routing that through the worker would make a click's
    /// responsiveness depend on the network thread.
    pub open_place: &'a mut Option<Place>,
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

    /// Asks to show a different place once this frame is finished.
    pub fn go_place(&mut self, place: Place) {
        *self.open_place = Some(place);
    }
}
