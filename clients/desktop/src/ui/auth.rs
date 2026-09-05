//! The unlock, sign-in and register screens.
//!
//! # One passphrase, and what it protects
//!
//! Three secrets appear on these screens and they are not interchangeable, so the labels say which is
//! which rather than saying "passphrase" three times. The account passphrase authenticates to the server
//! and the server can verify it. The vault passphrase never leaves this machine: it derives the key
//! that seals the identity and prekey material on disk, and no server can help recover it. A user who
//! believes those are the same secret will pick the same string for both, which means a server
//! breach that leaks a passphrase hash also becomes a head start on their local key file.
//!
//! # What is not offered
//!
//! There is no "remember my passphrase" checkbox. Storing it would mean sealing the vault key with
//! something derived from nothing, which is a vault that opens itself. The refresh token is kept
//! inside the vault instead, so one passphrase at startup is all that is asked, and the access token
//! is deliberately not persisted at all.

use egui::{Align, Layout, RichText, Ui};

use crate::config::ServerEndpoint;
use crate::net::Command;
use crate::theme::{palette, space};
use crate::ui::captcha::{self, CaptchaState};
use crate::ui::server_form::{self, ServerFormState};
use crate::ui::{widgets, Context, Screen};

/// What the three forms are holding.
///
/// The secrets live here for exactly as long as the form is on screen and are cleared the moment they
/// are handed to the worker. A form struct is not a keyring.
pub struct AuthState {
    pub server: ServerEndpoint,
    pub identifier: String,
    pub account_passphrase: String,
    pub passphrase: String,
    pub confirm: String,
    /// True from submit until the worker reports success or failure, so a second click cannot fire a
    /// second registration.
    pub busy: bool,
    /// The image captcha and everything about answering it. Shared by the register and sign-in
    /// forms because a challenge answers either: the server binds it to nothing but itself, so
    /// the second form to appear reuses the first one's picture rather than paying for another.
    pub captcha: CaptchaState,
    /// The local form state for the server disclosure. The disclosure owns its own "open" flag
    /// (held in egui's temp data so it does not reset on every frame), but the typed-but-not-yet
    /// accepted values live here so they survive a screen switch.
    pub server_form: ServerFormState,
    /// The restore form: where the `.migo` container is, and the recovery credential that opens it.
    ///
    /// The credential is as secret as a passphrase and is wiped with the rest; the path is not secret
    /// but lives beside it so the form is one struct. `restore_username` is not a secret at all:
    /// it is the account's name, stored beside the session so the unlock screen greets the right
    /// person and a later passphraseless login can name the account to the server.
    pub restore_path: String,
    pub restore_credential: String,
    pub restore_username: String,
}

impl Default for AuthState {
    fn default() -> Self {
        let server = crate::config::default_loopback_server_endpoint("localhost", 18080);
        let server_form = ServerFormState::from_endpoint(&server);
        Self {
            server,
            identifier: String::new(),
            account_passphrase: String::new(),
            passphrase: String::new(),
            confirm: String::new(),
            busy: false,
            captcha: CaptchaState::default(),
            server_form,
            restore_path: String::new(),
            restore_credential: String::new(),
            restore_username: String::new(),
        }
    }
}

impl AuthState {
    /// Wipes every secret. Called on success, on failure, and on leaving the screen.
    pub fn clear_secrets(&mut self) {
        // Overwrite before dropping. `String::clear` keeps the allocation, so zeroing first means the
        // bytes are not left sitting in a buffer the allocator may hand out unchanged.
        for field in [
            &mut self.account_passphrase,
            &mut self.passphrase,
            &mut self.confirm,
            &mut self.restore_credential,
        ] {
            let filled = "\0".repeat(field.len());
            field.replace_range(.., &filled);
            field.clear();
        }
    }

    /// Updates the server endpoint from the disclosure widget, then re-seeds the form state so
    /// the next time the disclosure opens it shows the accepted value.
    pub fn apply_server(&mut self, endpoint: ServerEndpoint) {
        if endpoint != self.server {
            // A challenge was issued by the old server and cannot be answered on the new one.
            // Drop it; the captcha section fetches from the new endpoint the moment it next
            // draws with nothing held.
            self.captcha.reset();
        }
        self.server = endpoint.clone();
        self.server_form = ServerFormState::from_endpoint(&endpoint);
    }

    /// Whether the register form is complete enough to submit.
    fn register_ready(&self) -> bool {
        !self.server.host.trim().is_empty()
            && self.identifier.trim().len() >= 3
            && self.account_passphrase.len() >= 8
            && self.passphrase.len() >= crate::vault::MIN_PASSPHRASE_BYTES
            && self.passphrase == self.confirm
    }

    /// Whether the sign-in form is complete enough to submit.
    fn sign_in_ready(&self) -> bool {
        !self.server.host.trim().is_empty()
            && !self.identifier.trim().is_empty()
            && !self.account_passphrase.is_empty()
            && self.passphrase.len() >= crate::vault::MIN_PASSPHRASE_BYTES
    }

    /// Whether the restore form is complete enough to submit.
    ///
    /// The confirm field is required here unlike sign-in: a restore writes a brand-new vault, and a
    /// passphrase mistyped on a one-shot form would seal the only copy of the account's keys under
    /// something the user cannot reproduce.
    fn restore_ready(&self) -> bool {
        !self.server.host.trim().is_empty()
            && !self.restore_path.trim().is_empty()
            && !self.restore_credential.is_empty()
            && self.passphrase.len() >= crate::vault::MIN_PASSPHRASE_BYTES
            && self.passphrase == self.confirm
    }
}

/// Draws whichever auth screen is current.
///
/// The front door is the reference's: the flat turquoise ground fills the viewport in either
/// theme — the sign-in screen is the one surface that ignores the theme, because it is the front
/// door and the front door does not change with the lights — and the form sits on a flat deep-teal
/// card with a translucent white hairline, so every field, label and hint keeps the palette ink it
/// already had.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut AuthState, screen: Screen) {
    let colors = palette(context.theme);
    widgets::gradient_rect_vertical(
        ui,
        ui.max_rect(),
        colors.login_a,
        colors.login_b,
        colors.login_c,
    );

    // The theme control, top-right on the ground: the one setting available before a session
    // exists. Sun while dark, moon while light — the glyph names the theme one click would arrive
    // at, drawn as the ground's own ink.
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add_space(space::MD);
        let glyph = if context.theme.is_dark() {
            "\u{2600}"
        } else {
            "\u{1F319}"
        };
        if ui
            .add(
                egui::Button::new(
                    RichText::new(glyph)
                        .font(egui::FontId::proportional(crate::theme::font::TITLE))
                        .color(colors.banner_ink),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE),
            )
            .on_hover_text(format!("Switch to {}", context.theme.flipped().label()))
            .clicked()
        {
            context.want_theme(context.theme.flipped());
        }
    });

    // A fixed-width card centred in the window. A form stretched across a wide monitor puts the
    // label a hand's width from its field, and the eye loses the pairing. The card is the
    // reference's flat deep teal with a translucent white hairline — the one colour the palettes
    // do not name, because the front door is its own surface in either theme.
    let column = 380.0_f32.min(ui.available_width() - space::XL * 2.0);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.08).min(64.0));
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(0x0b, 0x6f, 0x82))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(71)))
            .corner_radius(egui::CornerRadius::same(crate::theme::radius::LG))
            .inner_margin(egui::Margin::same(space::XL as i8))
            .show(ui, |ui| {
                ui.set_width(column);
                brand(ui, context);
                ui.add_space(space::XL);
                match screen {
                    Screen::Unlock => unlock(ui, context, state),
                    Screen::SignIn => sign_in(ui, context, state),
                    Screen::Register => register(ui, context, state),
                    Screen::Restore => restore(ui, context, state),
                    // The opening screen has no form: the worker has not said yet whether a vault
                    // exists, and guessing would mean redrawing the whole screen a moment later.
                    Screen::Opening => opening(ui, context),
                    Screen::Chat => {}
                }
                ui.add_space(space::LG);
                if let crate::model::Connection::Failed(reason) = context.connection {
                    problem(ui, context, reason);
                }
            });
    });
}

/// The product name and one line about what it does.
fn brand(ui: &mut Ui, context: &Context<'_>) {
    let colors = palette(context.theme);
    ui.vertical_centered(|ui| {
        ui.add_space(space::XS);
        ui.horizontal_centered(|ui| {
            widgets::brand_mark(ui, context.theme);
            ui.add_space(space::SM);
            ui.label(
                RichText::new("Migo")
                    .font(egui::FontId::proportional(crate::theme::font::DISPLAY))
                    .color(colors.text)
                    .strong(),
            );
        });
        ui.add_space(space::XS);
        ui.label(
            RichText::new("End-to-end encrypted messaging")
                .font(egui::FontId::proportional(crate::theme::font::SMALL))
                .color(colors.text_muted),
        );
    });
}

/// Shown while the worker looks for a vault.
fn opening(ui: &mut Ui, context: &Context<'_>) {
    let colors = palette(context.theme);
    ui.vertical_centered(|ui| {
        ui.add_space(space::XL);
        ui.spinner();
        ui.add_space(space::MD);
        ui.label(
            RichText::new("Looking for your keys")
                .font(egui::FontId::proportional(crate::theme::font::SMALL))
                .color(colors.text_muted),
        );
    });
}

/// The unlock form: one passphrase, no server round trip until it opens.
fn unlock(ui: &mut Ui, context: &mut Context<'_>, state: &mut AuthState) {
    widgets::header(
        ui,
        context.theme,
        "Unlock",
        Some("Your keys are on this device. The passphrase never leaves it."),
    );
    ui.add_space(space::LG);

    let response = widgets::field(
        ui,
        context.theme,
        "Vault passphrase",
        &mut state.passphrase,
        true,
        "",
    );
    let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

    let ready = state.passphrase.len() >= crate::vault::MIN_PASSPHRASE_BYTES && !state.busy;
    let clicked = widgets::primary_button(ui, context.theme, "Unlock", ready).clicked();
    if ready && (clicked || submitted) {
        context.issue(Command::Unlock {
            passphrase: state.passphrase.clone(),
        });
        state.busy = true;
        state.clear_secrets();
    }

    ui.add_space(space::MD);
    ui.vertical_centered(|ui| {
        // Deliberately offers to sign in again, not to delete the vault. Discarding key material is
        // irreversible and every peer's safety number changes with it, so it is not something a stray
        // click should be able to do; signing in with the same passphrase reuses the existing keys.
        if widgets::ghost_button(ui, context.theme, "Sign in to a different account").clicked() {
            state.clear_secrets();
            context.go(Screen::SignIn);
        }
    });
}

/// Renders the server disclosure into `ui`, applying the user's accepted endpoint back into
/// [`AuthState`]. Split out so the sign-in and register screens share the same disclosure
/// rendering rather than carrying the wiring twice. The widget returns a value on two paths —
/// "Use this server" and the one-tap transport selector under the toggle — and both land here,
/// where [`AuthState::apply_server`] persists the endpoint and re-seeds the form.
fn draw_server_disclosure(ui: &mut Ui, context: &Context<'_>, state: &mut AuthState) {
    let theme = context.theme;
    if let Some(endpoint) = server_form::show(ui, theme, &state.server, &mut state.server_form) {
        state.apply_server(endpoint);
    }
}

/// The sign-in form.
fn sign_in(ui: &mut Ui, context: &mut Context<'_>, state: &mut AuthState) {
    widgets::header(
        ui,
        context.theme,
        "Sign in",
        Some("To an account you already have."),
    );
    ui.add_space(space::LG);

    draw_server_disclosure(ui, context, state);
    widgets::field(
        ui,
        context.theme,
        "Username or email",
        &mut state.identifier,
        false,
        "",
    );
    widgets::field(
        ui,
        context.theme,
        "Account passphrase",
        &mut state.account_passphrase,
        true,
        "",
    );
    widgets::field(
        ui,
        context.theme,
        "New vault passphrase",
        &mut state.passphrase,
        true,
        "at least 8 characters",
    );
    hint(
        ui,
        context,
        "The vault passphrase encrypts your keys on this computer. It is not sent anywhere and cannot be reset.",
    );
    ui.add_space(space::LG);

    let ready = state.sign_in_ready() && !state.busy;
    if widgets::primary_button(ui, context.theme, "Sign in", ready).clicked() {
        context.issue(Command::SignIn {
            server: state.server.clone(),
            identifier: state.identifier.trim().to_owned(),
            account_passphrase: state.account_passphrase.clone(),
            passphrase: state.passphrase.clone(),
            captcha: state.captcha.take_proof(),
        });
        state.busy = true;
        state.clear_secrets();
    }

    ui.add_space(space::MD);
    ui.vertical_centered(|ui| {
        if widgets::ghost_button(ui, context.theme, "Create an account instead").clicked() {
            state.clear_secrets();
            state.busy = false;
            context.go(Screen::Register);
        }
        ui.add_space(space::XS);
        // The third door: not a passphrase, but the container a founding device sealed. It is offered
        // from sign-in because it is the same situation — an existing account, a new machine.
        if widgets::ghost_button(ui, context.theme, "Restore from a backup").clicked() {
            state.clear_secrets();
            state.busy = false;
            context.go(Screen::Restore);
        }
    });
}

/// The register form.
fn register(ui: &mut Ui, context: &mut Context<'_>, state: &mut AuthState) {
    widgets::header(ui, context.theme, "Create an account", None);
    ui.add_space(space::LG);

    draw_server_disclosure(ui, context, state);
    widgets::field(
        ui,
        context.theme,
        "Username",
        &mut state.identifier,
        false,
        "",
    );
    widgets::field(
        ui,
        context.theme,
        "Account passphrase",
        &mut state.account_passphrase,
        true,
        "",
    );
    widgets::field(
        ui,
        context.theme,
        "Vault passphrase",
        &mut state.passphrase,
        true,
        "",
    );
    widgets::field(
        ui,
        context.theme,
        "Confirm passphrase",
        &mut state.confirm,
        true,
        "",
    );

    if !state.confirm.is_empty() && state.passphrase != state.confirm {
        problem(ui, context, "The two passphrases do not match.");
        ui.add_space(space::SM);
    }
    hint(
        ui,
        context,
        "Two different secrets: the account passphrase signs you in to the server, the vault passphrase encrypts your keys here. Do not reuse one for the other.",
    );
    captcha::show(ui, context, &mut state.captcha, &state.server);
    ui.add_space(space::LG);

    let ready = state.register_ready() && !state.busy;
    if widgets::primary_button(ui, context.theme, "Create account", ready).clicked() {
        context.issue(Command::Register {
            server: state.server.clone(),
            username: state.identifier.trim().to_owned(),
            account_passphrase: state.account_passphrase.clone(),
            passphrase: state.passphrase.clone(),
            captcha: state.captcha.take_proof(),
        });
        state.busy = true;
        state.clear_secrets();
    }

    ui.add_space(space::MD);
    ui.vertical_centered(|ui| {
        if widgets::ghost_button(ui, context.theme, "I already have an account").clicked() {
            state.clear_secrets();
            state.busy = false;
            context.go(Screen::SignIn);
        }
    });
}

/// The restore form: a `.migo` container plus its recovery credential, onto this device.
///
/// The container holds the account root, so opening it *is* the sign-in — the ML-DSA add-device
/// ceremony runs before a vault is written, and the passphrase below seals the new vault rather
/// than opening anything. A restore is a new device: fresh E2EE keys, its own login credential,
/// the account root the container carried.
fn restore(ui: &mut Ui, context: &mut Context<'_>, state: &mut AuthState) {
    widgets::header(
        ui,
        context.theme,
        "Restore your account",
        Some("From a .migo backup container."),
    );
    ui.add_space(space::LG);

    draw_server_disclosure(ui, context, state);
    widgets::field(
        ui,
        context.theme,
        "Username",
        &mut state.restore_username,
        false,
        "your account's name, for next time",
    );
    widgets::field(
        ui,
        context.theme,
        "Container file",
        &mut state.restore_path,
        false,
        "e.g. migo-backup.migo",
    );
    widgets::field(
        ui,
        context.theme,
        "Recovery credential",
        &mut state.restore_credential,
        true,
        "the words you chose when the backup was made",
    );
    widgets::field(
        ui,
        context.theme,
        "New vault passphrase",
        &mut state.passphrase,
        true,
        "at least 8 characters",
    );
    widgets::field(
        ui,
        context.theme,
        "Confirm passphrase",
        &mut state.confirm,
        true,
        "",
    );
    if !state.confirm.is_empty() && state.passphrase != state.confirm {
        problem(ui, context, "The two passphrases do not match.");
        ui.add_space(space::SM);
    }
    hint(
        ui,
        context,
        "The recovery credential opens the container and is never sent anywhere. The vault passphrase encrypts your keys on this computer and cannot be reset.",
    );
    ui.add_space(space::LG);

    let ready = state.restore_ready() && !state.busy;
    if widgets::primary_button(ui, context.theme, "Restore", ready).clicked() {
        let path = std::path::PathBuf::from(state.restore_path.trim());
        context.issue(Command::ImportContainer {
            path,
            credential: state.restore_credential.clone(),
            passphrase: state.passphrase.clone(),
            username: state.restore_username.clone(),
            server: state.server.clone(),
        });
        state.busy = true;
        state.clear_secrets();
    }

    ui.add_space(space::MD);
    ui.vertical_centered(|ui| {
        if widgets::ghost_button(ui, context.theme, "Back to sign in").clicked() {
            state.clear_secrets();
            state.busy = false;
            context.go(Screen::SignIn);
        }
    });
}

/// Small explanatory text under a group of fields.
fn hint(ui: &mut Ui, context: &Context<'_>, text: &str) {
    let colors = palette(context.theme);
    ui.label(
        RichText::new(text)
            .font(egui::FontId::proportional(crate::theme::font::TINY))
            .color(colors.text_muted),
    );
}

/// A failure, in the danger colour, wrapped rather than truncated.
///
/// The text comes from the worker, which only ever forwards a public error message. Nothing here can
/// render an internal message, because nothing internal reaches this thread.
fn problem(ui: &mut Ui, context: &Context<'_>, text: &str) {
    let colors = palette(context.theme);
    ui.label(
        RichText::new(text)
            .font(egui::FontId::proportional(crate::theme::font::SMALL))
            .color(colors.danger),
    );
}
