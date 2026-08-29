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

pub mod auth;
pub mod captcha;
pub mod chat;
pub mod server_form;
pub mod widgets;

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

/// Everything a screen may read, and the two things it may write.
pub struct Context<'a> {
    pub theme: Theme,
    pub connection: &'a Connection,
    pub account: Option<&'a Account>,
    /// Intent pushed here is forwarded to the worker after the frame.
    pub commands: &'a mut Vec<Command>,
    /// A screen change requested by a link on the screen, applied after the frame.
    ///
    /// Separate from [`Context::commands`] because navigation is not something the worker should hear
    /// about: moving from the sign-in form to the register form touches no socket and no key, and
    /// routing it through the network thread would make the window's responsiveness depend on it.
    pub navigate: &'a mut Option<Screen>,
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
}
