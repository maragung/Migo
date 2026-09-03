//! The settings pane: server, theme, devices, and the way out.
//!
//! # What a settings pane is for
//!
//! The four things here are the four things a person can only discover by being shown them: which
//! server they are talking to (an address is easy to mistype and impossible to eyeball from a
//! chat screen), which colour the window is in, which devices hold a live session of their
//! account, and where the door is. Everything else the client decides for itself, because
//! presenting a knob for it would be promising a tuning that does not exist.
//!
//! # The session list is honest about not knowing
//!
//! `GET /v1/auth/sessions` is not offered by every deployment, and a panel that showed an empty
//! list when the request failed would be saying "no other devices" — the most reassuring answer
//! available — on the strength of no evidence at all. So the failure is held and drawn as a
//! sentence, and only a successful empty answer gets to say "this is the only session".

use egui::{Align, Layout, RichText, Ui};
use migo_core::Id;

use crate::model::{Connection, SessionRow};
use crate::net::Command;
use crate::theme::{font, palette, space, text_style};
use crate::ui::widgets;
use crate::ui::Context;

/// What the device list currently shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SessionsView {
    /// Never asked. The pane does not fetch on its own: a settings screen that quietly made a
    /// network call the moment it was drawn would be a surprise to anyone watching a firewall.
    #[default]
    NotAsked,
    /// Asked, not answered.
    Loading,
    /// Answered: these are the sessions.
    Ready(Vec<SessionRow>),
    /// Asked and refused — unreachable server, unknown route, anything. The string is safe to
    /// show: it is this client's own wording or the server's public message, never an internal
    /// error chain.
    Unavailable(String),
}

impl SessionsView {
    /// Files a REST outcome. Pure, so the mapping from "what happened" to "what shows" is pinned
    /// by a test rather than implied by two call sites drifting apart.
    pub fn from_result(result: Result<Vec<SessionRow>, String>) -> Self {
        match result {
            Ok(list) => Self::Ready(list),
            Err(reason) => Self::Unavailable(reason),
        }
    }
}

/// What a list-shaped REST answer currently shows, for the account-root surfaces.
///
/// The same four states as [`SessionsView`] — never asked, loading, answered, refused — over any
/// row type, because the honest-uncertainty rule is not about sessions in particular: a panel that
/// shows an empty list where a failure belongs is lying with the most reassuring answer available.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Fetch<T> {
    #[default]
    NotAsked,
    Loading,
    Ready(Vec<T>),
    Unavailable(String),
}

impl<T> Fetch<T> {
    /// Files a REST outcome, with the same shape [`SessionsView::from_result`] pins.
    pub fn from_result(result: Result<Vec<T>, String>) -> Self {
        match result {
            Ok(list) => Self::Ready(list),
            Err(reason) => Self::Unavailable(reason),
        }
    }
}

/// Everything the settings pane holds between frames.
#[derive(Default)]
pub struct SettingsState {
    /// The device list, as last asked or answered.
    pub sessions: SessionsView,
    /// The account's devices — the account-root view, not the session view.
    pub devices: Fetch<crate::model::DeviceRow>,
    /// The account's registered wallet addresses.
    pub wallets: Fetch<crate::model::EvmWalletRow>,
    /// The backup form: where to write the `.migo` container and the credential that will open it.
    ///
    /// The credential is a secret and is wiped the moment it leaves for the worker; the path is a
    /// path.
    pub backup_path: String,
    pub backup_credential: String,
    pub backup_confirm: String,
    /// The recovery-contact form: one string, an email or a phone. Not a secret — the server
    /// shows it back through recovery, and the field is a replace rather than an append.
    pub contact: String,
    /// The passphrase-change form. All three fields are secrets and are wiped the moment they
    /// leave for the worker; `passphrase_confirm` exists for the same reason the backup form's
    /// second credential field does — a passphrase mistyped on a one-shot form is an account
    /// nobody can sign in to.
    pub passphrase_current: String,
    pub passphrase_next: String,
    pub passphrase_confirm: String,
}

/// Draws the settings pane.
///
/// Scrolls as one document, so a device list long enough to push Sign out off the bottom of the
/// window pushes it into reach of the wheel instead of out of the interface.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut SettingsState) {
    let column = 460.0_f32.min(ui.available_width() - space::XL * 2.0);

    egui::ScrollArea::vertical()
        .id_salt("settings-pane")
        .max_height(ui.available_height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::XL);
                ui.allocate_ui(egui::vec2(column, 0.0), |ui| {
                    widgets::header(ui, context.theme, "Settings", None);
                    ui.add_space(space::LG);

                    server_section(ui, context);
                    ui.add_space(space::LG);
                    appearance_section(ui, context);
                    ui.add_space(space::LG);
                    sessions_section(ui, context, state);
                    ui.add_space(space::LG);
                    account_section(ui, context, state);
                    ui.add_space(space::LG);
                    sign_in_section(ui, context, state);
                    ui.add_space(space::LG);
                    backup_section(ui, context, state);
                    ui.add_space(space::XL);
                    sign_out_section(ui, context);
                });
            });
        });
}

/// The server this session lives on, and whether the socket to it is up.
fn server_section(ui: &mut Ui, context: &mut Context<'_>) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Server");

    let url = crate::config::rest_base_url(context.server);
    ui.label(
        RichText::new(&url)
            .font(egui::FontId::monospace(font::SMALL))
            .color(colors.text),
    );
    ui.add_space(space::XS);

    let (color, label) = match context.connection {
        Connection::Online => (colors.positive, "Connected"),
        Connection::Connecting => (colors.warning, "Connecting"),
        Connection::Offline => (colors.text_muted, "Offline"),
        Connection::Fallback(_) => (colors.accent, "Connected"),
        Connection::Failed(_) => (colors.danger, "Disconnected"),
    };
    ui.horizontal(|ui| widgets::status_dot(ui, context.theme, color, label));

    if let Some(account) = context.account {
        ui.add_space(space::XS);
        ui.label(
            RichText::new(format!(
                "Signed in as {} (device {})",
                account.username,
                crate::model::short_id(account.device_id)
            ))
            .font(egui::FontId::proportional(font::TINY))
            .color(colors.text_muted),
        );
    }
}

/// The interface's own knobs: how big it draws, and where its theme lives.
///
/// The scale control drives egui's zoom, which grows and shrinks the whole interface — type,
/// bars, spacing — as one piece, because that is what a person asking for a bigger interface
/// means. The theme itself is the banner's sun/moon control, exactly as the web client draws it:
/// one control per action, always visible, never a second copy hiding in a panel.
fn appearance_section(ui: &mut Ui, context: &mut Context<'_>) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Appearance");

    // The offered steps, as percentages. Discrete on purpose: four honest sizes beat a slider's
    // infinity of sizes nobody can tell apart, and a whole percent is what the settings file
    // stores — the choice has to survive a restart exactly as it was made.
    const STEPS: [(f32, &str); 4] = [
        (0.85, "Smaller"),
        (1.0, "Normal"),
        (1.15, "Larger"),
        (1.3, "Largest"),
    ];
    ui.label(
        RichText::new("Text size")
            .font(egui::FontId::proportional(font::BODY))
            .color(colors.text),
    );
    ui.horizontal(|ui| {
        for (zoom, label) in STEPS {
            if ui
                .add(egui::Button::new(label).selected(ui.ctx().zoom_factor() == zoom))
                .on_hover_text("Applies to this window only, immediately.")
                .clicked()
            {
                context.want_zoom(zoom);
            }
        }
    });
    ui.label(
        RichText::new(format!(
            "Theme: {} — switch it with the sun or moon on the banner.",
            context.theme.label()
        ))
        .font(egui::FontId::proportional(font::TINY))
        .color(colors.text_muted),
    );
}

/// The session list, its refresh, and per-row revoke.
///
/// Titled "Sessions" rather than "Devices" because the account-root surface below lists devices:
/// a session is a login this week, a device is a machine with a login credential, and conflating
/// the two would make the security story harder to read rather than easier.
fn sessions_section(ui: &mut Ui, context: &mut Context<'_>, state: &mut SettingsState) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Devices");

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Every signed-in device for this account.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let busy = matches!(state.sessions, SessionsView::Loading);
            if ui
                .add_enabled(!busy, egui::Button::new("Refresh"))
                .clicked()
            {
                state.sessions = SessionsView::Loading;
                context.issue(Command::Sessions);
            }
        });
    });
    ui.add_space(space::SM);

    match &state.sessions {
        SessionsView::NotAsked => {
            ui.label(
                RichText::new("Not checked yet. Refresh to list this account's sessions.")
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        }
        SessionsView::Loading => {
            ui.spinner();
        }
        SessionsView::Unavailable(reason) => {
            ui.label(
                RichText::new(format!("Session list unavailable: {reason}"))
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.warning),
            );
        }
        SessionsView::Ready(rows) => {
            if rows.is_empty() {
                ui.label(
                    RichText::new("The server listed no sessions.")
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
            }
            let mut revoke: Option<Id> = None;
            for row in rows {
                session_row(ui, context, row, &mut revoke);
                ui.add_space(space::XS);
            }
            if let Some(session_id) = revoke {
                state.sessions = SessionsView::Loading;
                context.issue(Command::RevokeSession { session_id });
            }
        }
    }
}

/// One device row: name, when it was last seen, and the revoke button.
///
/// The current session's button is disabled with an explanation rather than hidden: a row that
/// silently has no button invites the user to wonder what else differs about it, and the honest
/// answer — "that one is this window" — is one hover away.
fn session_row(ui: &mut Ui, context: &Context<'_>, row: &SessionRow, revoke: &mut Option<Id>) {
    let colors = palette(context.theme);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(widgets::elide(&row.device, 40))
                            .font(egui::FontId::proportional(font::BODY))
                            .color(colors.text),
                    );
                    let mut detail = String::new();
                    if let Some(at) = row.created_at {
                        detail.push_str(&format!("since {}", crate::model::date(at)));
                    }
                    if let Some(at) = row.last_active_at {
                        if !detail.is_empty() {
                            detail.push_str(" \u{00B7} ");
                        }
                        detail.push_str(&format!("last seen {}", crate::model::date(at)));
                    }
                    if row.current {
                        if !detail.is_empty() {
                            detail.push_str(" \u{00B7} ");
                        }
                        detail.push_str("this device");
                    }
                    if !detail.is_empty() {
                        ui.label(
                            RichText::new(detail)
                                .text_style(crate::theme::named(text_style::CAPTION))
                                .color(colors.text_muted),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if row.current {
                        "This session"
                    } else {
                        "Revoke"
                    };
                    let button = widgets::ghost_button(ui, context.theme, label);
                    let response = if row.current {
                        button.on_hover_text(
                            "Use Sign out below to end the session this window is running on.",
                        )
                    } else {
                        button
                    };
                    if !row.current && response.clicked() {
                        *revoke = Some(row.session_id);
                    }
                });
            });
        });
}

/// The account-root surface: the machines that hold a login credential, and the wallet addresses
/// derived from the root. Reads both on the user's click, like the session list above.
fn account_section(ui: &mut Ui, context: &mut Context<'_>, state: &mut SettingsState) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Account");

    // --- safety number ---------------------------------------------------------
    // The fingerprint of this device's identity key, home here since the conversation list that
    // used to carry it left the shell. Shown as a monospace block so a pair of people can read it
    // to each other aloud and compare — that comparison is the only thing that detects a
    // substituted identity key, and it is worth the four lines it costs.
    if let Some(account) = context.account {
        ui.label(
            RichText::new("Safety number")
                .text_style(crate::theme::named(crate::theme::text_style::OVERLINE))
                .color(colors.text_muted),
        );
        ui.add_space(space::XS);
        ui.label(
            RichText::new(&account.safety_number)
                .font(egui::FontId::monospace(font::SMALL))
                .color(colors.text),
        );
        ui.add_space(space::XS);
        ui.label(
            RichText::new(
                "Compare this with the other person, in a call or in person. If it differs, \
                 stop and do not trust the conversation.",
            )
            .font(egui::FontId::proportional(font::TINY))
            .color(colors.text_muted),
        );
        ui.add_space(space::LG);
    }

    // --- devices ---------------------------------------------------------------
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Devices that can sign in to this account.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(
                    !matches!(state.devices, Fetch::Loading),
                    egui::Button::new("Refresh"),
                )
                .clicked()
            {
                state.devices = Fetch::Loading;
                context.issue(Command::Devices);
            }
        });
    });
    ui.add_space(space::SM);
    match &state.devices {
        Fetch::NotAsked => {
            ui.label(
                RichText::new("Not checked yet. Refresh to list this account's devices.")
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        }
        Fetch::Loading => {
            ui.spinner();
        }
        Fetch::Unavailable(reason) => {
            ui.label(
                RichText::new(format!("Device list unavailable: {reason}"))
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.warning),
            );
        }
        Fetch::Ready(rows) => {
            if rows.is_empty() {
                ui.label(
                    RichText::new("The server listed no devices.")
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
            }
            let mut remove: Option<Id> = None;
            for row in rows {
                device_row(ui, context, row, &mut remove);
                ui.add_space(space::XS);
            }
            if let Some(device_id) = remove {
                state.devices = Fetch::Loading;
                context.issue(Command::RevokeDevice { device_id });
            }
        }
    }

    ui.add_space(space::LG);

    // --- wallets ---------------------------------------------------------------
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Wallet addresses the account root derives.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(
                    !matches!(state.wallets, Fetch::Loading),
                    egui::Button::new("Refresh"),
                )
                .clicked()
            {
                state.wallets = Fetch::Loading;
                context.issue(Command::Wallets);
            }
        });
    });
    ui.add_space(space::SM);
    match &state.wallets {
        Fetch::NotAsked => {
            ui.label(
                RichText::new("Not checked yet. Refresh to list this account's wallets.")
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        }
        Fetch::Loading => {
            ui.spinner();
        }
        Fetch::Unavailable(reason) => {
            ui.label(
                RichText::new(format!("Wallet list unavailable: {reason}"))
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.warning),
            );
        }
        Fetch::Ready(rows) => {
            if rows.is_empty() {
                ui.label(
                    RichText::new("No wallet addresses are registered.")
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
            }
            let mut archive: Option<Id> = None;
            for row in rows {
                wallet_row(ui, context, row, &mut archive);
                ui.add_space(space::XS);
            }
            if let Some(wallet_id) = archive {
                state.wallets = Fetch::Loading;
                context.issue(Command::ArchiveWallet { wallet_id });
            }
        }
    }
}

/// One account device row.
///
/// The credential mark is the security question a device list exists to answer: a device *with* a
/// credential can take part in the passphraseless ceremony, and one appearing that the user does not
/// recognise is the moment to remove it — or rotate. The current device and already-revoked ones
/// carry no button: removing the device the button is pressed on would sign its own user out
/// mid-click, and a revoked device is gone.
fn device_row(
    ui: &mut Ui,
    context: &Context<'_>,
    row: &crate::model::DeviceRow,
    remove: &mut Option<Id>,
) {
    let colors = palette(context.theme);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(widgets::elide(&row.display_name, 40))
                            .font(egui::FontId::proportional(font::BODY))
                            .color(colors.text),
                    );
                    let mut detail = row.platform.clone();
                    if let Some(at) = row.created_at {
                        detail.push_str(&format!(" \u{00B7} added {}", crate::model::date(at)));
                    }
                    if let Some(at) = row.last_seen {
                        detail.push_str(&format!(" \u{00B7} last seen {}", crate::model::date(at)));
                    }
                    if row.has_credential {
                        detail.push_str(" \u{00B7} holds a login credential");
                    }
                    if row.is_current {
                        detail.push_str(" \u{00B7} this device");
                    }
                    ui.label(
                        RichText::new(detail)
                            .text_style(crate::theme::named(text_style::CAPTION))
                            .color(colors.text_muted),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if row.is_current || row.status == "revoked" {
                        ui.label(
                            RichText::new(if row.is_current {
                                "current"
                            } else {
                                &row.status
                            })
                            .text_style(crate::theme::named(text_style::CAPTION))
                            .color(colors.text_muted),
                        );
                    } else {
                        if widgets::ghost_button(ui, context.theme, "Remove")
                            .on_hover_text(
                                "Signs this device out of the account everywhere and ends its \
                                 ability to sign in again. Its messages and keys stay on the \
                                 device it was.",
                            )
                            .clicked()
                        {
                            *remove = Some(row.device_id);
                        }
                        ui.label(
                            RichText::new(&row.status)
                                .text_style(crate::theme::named(text_style::CAPTION))
                                .color(colors.text_muted),
                        );
                    }
                });
            });
        });
}

/// One wallet row: the address in monospace, the derivation index beside it, and the archive
/// button an address no longer in use wants.
///
/// Archiving is honest about what it is: the address leaves the account's active list, but the
/// root still derives it and registering it again brings it back. A hover says exactly that, so
/// the button is not mistaken for deleting a key.
fn wallet_row(
    ui: &mut Ui,
    context: &Context<'_>,
    row: &crate::model::EvmWalletRow,
    archive: &mut Option<Id>,
) {
    let colors = palette(context.theme);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&row.address)
                        .font(egui::FontId::monospace(font::SMALL))
                        .color(colors.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if row.status == "archived" {
                        ui.label(
                            RichText::new(format!(
                                "archived \u{00B7} \u{23}{}",
                                row.derivation_index
                            ))
                            .text_style(crate::theme::named(text_style::CAPTION))
                            .color(colors.text_muted),
                        );
                    } else {
                        let label = match &row.label {
                            Some(text) => {
                                format!("\u{23}{} \u{00B7} {}", row.derivation_index, text)
                            }
                            None => format!("\u{23}{}", row.derivation_index),
                        };
                        ui.label(
                            RichText::new(label)
                                .text_style(crate::theme::named(text_style::CAPTION))
                                .color(colors.text_muted),
                        );
                        if widgets::ghost_button(ui, context.theme, "Archive")
                            .on_hover_text(
                                "Takes the address off the account's active list. The root still \
                                 derives it; registering it again brings it back.",
                            )
                            .clicked()
                        {
                            *archive = Some(row.wallet_id);
                        }
                    }
                });
            });
        });
}

/// The sign-in material: the recoverable contact, and the passphrase itself.
///
/// The contact form is a replace, not an append — the server keeps exactly one value, normalised
/// on arrival — and the form says so, because a person who expected a list would expect a second
/// save to keep both. The passphrase form states its cost before the button unlocks: the server
/// ends every session of the account and answers with a replacement grant for this window, so
/// every other device signs in again with the new passphrase.
fn sign_in_section(ui: &mut Ui, context: &mut Context<'_>, state: &mut SettingsState) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Sign-in");

    // --- recovery contact -------------------------------------------------------
    ui.label(
        RichText::new(
            "A recovery contact — an email or a phone — is where a recovery starts. The account \
             keeps one; saving replaces it.",
        )
        .font(egui::FontId::proportional(font::SMALL))
        .color(colors.text_muted),
    );
    widgets::field(
        ui,
        context.theme,
        "Email or phone",
        &mut state.contact,
        false,
        "name@example.com or +62…",
    );
    ui.add_space(space::SM);
    let contact_ready = !state.contact.trim().is_empty();
    if widgets::primary_button(ui, context.theme, "Save contact", contact_ready)
        .on_hover_text(
            "The server judges the shape: an email containing @, or a phone starting with +.",
        )
        .clicked()
    {
        context.issue(Command::SetContact {
            email_or_phone: state.contact.trim().to_owned(),
        });
    }

    ui.add_space(space::LG);

    // --- passphrase -------------------------------------------------------------
    ui.label(
        RichText::new(
            "Changing the passphrase signs out every other session, on every device; this window \
             stays signed in. The next time the app starts it may ask you to sign in again.",
        )
        .font(egui::FontId::proportional(font::SMALL))
        .color(colors.text_muted),
    );
    ui.add_space(space::SM);
    widgets::field(
        ui,
        context.theme,
        "Current passphrase",
        &mut state.passphrase_current,
        true,
        "",
    );
    widgets::field(
        ui,
        context.theme,
        "New passphrase",
        &mut state.passphrase_next,
        true,
        "long enough to stay unguessable",
    );
    widgets::field(
        ui,
        context.theme,
        "Confirm new passphrase",
        &mut state.passphrase_confirm,
        true,
        "",
    );

    let mismatch =
        !state.passphrase_confirm.is_empty() && state.passphrase_next != state.passphrase_confirm;
    if mismatch {
        ui.label(
            RichText::new("The two new passphrases do not match.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.danger),
        );
    }
    let passphrase_ready = !state.passphrase_current.is_empty()
        && !state.passphrase_next.is_empty()
        && state.passphrase_next == state.passphrase_confirm;
    ui.add_space(space::SM);
    if widgets::primary_button(ui, context.theme, "Change passphrase", passphrase_ready).clicked() {
        context.issue(Command::ChangePassphrase {
            current: state.passphrase_current.clone(),
            next: state.passphrase_next.clone(),
        });
        // Wipe all three the moment they leave for the worker; a form that kept them would be a
        // third copy of the account's secret, on a pane that also shows the device list.
        state.passphrase_current.clear();
        state.passphrase_next.clear();
        state.passphrase_confirm.clear();
    }
}

/// The `.migo` backup form.
///
/// Offered only on a device that holds the root, because a backup is a copy of the root and a
/// device without one has nothing to copy. The credential is confirmed twice for the same reason
/// the restore form's passphrase is: the container is the account, and a credential mistyped on a
/// one-shot form would seal it under something nobody can reproduce.
fn backup_section(ui: &mut Ui, context: &mut Context<'_>, state: &mut SettingsState) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Account backup");

    let holds_root = context.account.is_some_and(|account| account.holds_root);
    if !holds_root {
        ui.label(
            RichText::new(
                "This device signs in with your passphrase and does not hold the account root, so \
                 it cannot make a backup. Seal one from a device that restored or created the \
                 account.",
            )
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
        return;
    }

    ui.label(
        RichText::new(
            "Seals the account root into a portable container: enough to restore the account onto \
             another device, and nothing without the credential you choose here.",
        )
        .font(egui::FontId::proportional(font::SMALL))
        .color(colors.text_muted),
    );
    ui.add_space(space::SM);
    widgets::field(
        ui,
        context.theme,
        "Save as",
        &mut state.backup_path,
        false,
        "e.g. migo-backup.migo",
    );
    widgets::field(
        ui,
        context.theme,
        "Recovery credential",
        &mut state.backup_credential,
        true,
        "long enough to stay unguessable",
    );
    widgets::field(
        ui,
        context.theme,
        "Confirm credential",
        &mut state.backup_confirm,
        true,
        "",
    );

    let credential_ready = !state.backup_credential.is_empty()
        && state.backup_credential == state.backup_confirm
        && !state.backup_path.trim().is_empty();
    let mismatch =
        !state.backup_confirm.is_empty() && state.backup_credential != state.backup_confirm;
    if mismatch {
        ui.label(
            RichText::new("The two credentials do not match.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.danger),
        );
    }
    ui.add_space(space::SM);
    if widgets::primary_button(ui, context.theme, "Seal backup", credential_ready).clicked() {
        let path = std::path::PathBuf::from(state.backup_path.trim());
        context.issue(Command::ExportContainer {
            path,
            credential: state.backup_credential.clone(),
        });
        // Wipe the credential the moment it leaves for the worker; the path stays, because a
        // second backup usually goes to the same kind of place.
        state.backup_credential.clear();
        state.backup_confirm.clear();
    }
}

/// Sign out.
///
/// The one destructive action on the pane, so it is last, alone, and styled as a ghost rather
/// than a primary button: the primary colour is for the thing the pane wants the user to find,
/// and nobody should be helped into destroying their local keys by accident.
fn sign_out_section(ui: &mut Ui, context: &mut Context<'_>) {
    if widgets::ghost_button(ui, context.theme, "Sign out").clicked() {
        context.issue(Command::SignOut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: u8, current: bool) -> SessionRow {
        SessionRow {
            session_id: Id::from_bytes([n; 16]),
            device: format!("Device {n}"),
            created_at: None,
            last_active_at: None,
            current,
        }
    }

    #[test]
    fn sessions_view_files_outcomes_honestly() {
        // Success carries the rows, whatever their length.
        let view = SessionsView::from_result(Ok(vec![row(1, false), row(2, true)]));
        assert_eq!(view, SessionsView::Ready(vec![row(1, false), row(2, true)]));

        // An empty success is "Ready and empty" — never conflated with a failure.
        assert_eq!(
            SessionsView::from_result(Ok(Vec::new())),
            SessionsView::Ready(Vec::new())
        );

        // A failure keeps its reason, so the pane can say why it does not know.
        assert_eq!(
            SessionsView::from_result(Err("cannot reach the server".to_owned())),
            SessionsView::Unavailable("cannot reach the server".to_owned())
        );
    }

    #[test]
    fn the_pane_starts_not_asking() {
        // A fresh pane must not claim to be loading anything: the fetch is the user's click.
        assert_eq!(SettingsState::default().sessions, SessionsView::NotAsked);
    }
}
