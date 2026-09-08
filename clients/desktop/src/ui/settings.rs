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
//! # The checkup is a summary, not a second opinion
//!
//! The security checkup (§50) draws one line per fixed row — Identity, Devices, Wallets, Backup,
//! Recovery, E2EE — from facts the pane's own sections already own or can ask for, and every
//! verdict is one the data can back. A row that had its own idea of "secure" would be two
//! security stories in one window, disagreeing with each other the first time one of them got
//! behind.
//!
//! # The session list is honest about not knowing
//!
//! `GET /v1/auth/sessions` is not offered by every deployment, and a panel that showed an empty
//! list when the request failed would be saying "no other devices" — the most reassuring answer
//! available — on the strength of no evidence at all. So the failure is held and drawn as a
//! sentence, and only a successful empty answer gets to say "this is the only session".

use egui::{Align, Layout, RichText, Ui};
use migo_core::{Id, Timestamp};

use crate::model::{Connection, DeviceRow, EvmWalletRow, SessionRow};
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

/// What the recovery-contact answer currently shows — the same four states as [`SessionsView`],
/// over one bit instead of a list, because the honest-uncertainty rule is about answers, not
/// about their size: "could not check" and "not configured" are different sentences, and only
/// the second one is advice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RecoveryView {
    #[default]
    NotAsked,
    Loading,
    Ready(bool),
    Unavailable(String),
}

impl RecoveryView {
    /// Files a REST outcome. Pure, for the same reason [`SessionsView::from_result`] is.
    pub fn from_result(result: Result<bool, String>) -> Self {
        match result {
            Ok(configured) => Self::Ready(configured),
            Err(reason) => Self::Unavailable(reason),
        }
    }
}

/// The window after which an active device the account has not heard from becomes the checkup's
/// business: thirty days, in milliseconds. Long enough that a phone on a shelf for a fortnight
/// is nobody's warning, short enough that a credential nobody has used for a month is.
const OLD_DEVICE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Whether one device row is the "Old device active" warning: an active device whose last
/// sighting is more than [`OLD_DEVICE_MS`] behind `now`.
///
/// Revoked devices never qualify — a machine the account has already removed is not a live
/// credential however long ago it was seen. A row with no last-seen does not qualify either:
/// that is "not disclosed", not "not seen", and a warning built on a guess is the first warning
/// nobody heeds. The device asking gets no exemption: its last-seen is this session's, and if
/// the server's own row disagrees, that is a discrepancy worth surfacing, not one to define
/// away.
fn is_old_active_device(row: &DeviceRow, now: Timestamp) -> bool {
    row.status == "active" && row.last_seen.is_some_and(|seen| now - seen > OLD_DEVICE_MS)
}

/// The device the "Old device active" warning names, when there is one: the oldest qualifying
/// row, because with several candidates the one silent longest is the one the sentence is about.
fn old_active_device(rows: &[DeviceRow], now: Timestamp) -> Option<&DeviceRow> {
    rows.iter()
        .filter(|row| is_old_active_device(row, now))
        .min_by_key(|row| row.last_seen)
}

/// The checkup's vocabulary for the backup row — three answers, each one a fact rather than a
/// guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupCheck {
    /// This device holds no account root, so it has nothing to seal: the row says where backups
    /// live rather than pretending one is missing here.
    NoRoot,
    /// The device holds the root and no container it sealed still counts — it never sealed one,
    /// or a rotation retired the last.
    Never,
    /// A container was sealed from this device at this unix-seconds instant.
    BackedUp(u64),
}

/// Reduces the two facts the backup row has — whether this device holds the root, and when it
/// last sealed a container — to the answer it draws. Pure, so the transitions the row promises
/// (a seal dates it; a rotation undates it; a passenger device is not accused) are pinned by
/// tests rather than implied by the section that draws them.
fn backup_check(holds_root: bool, last_backup_at: Option<u64>) -> BackupCheck {
    match (holds_root, last_backup_at) {
        (false, _) => BackupCheck::NoRoot,
        (true, Some(at)) => BackupCheck::BackedUp(at),
        (true, None) => BackupCheck::Never,
    }
}

/// The wallet row's two counts, by the statuses the wallet listing itself uses. A status this
/// build has no name for is counted nowhere rather than guessed at — the row's arithmetic must
/// not be the place a server's new status silently becomes "archived".
fn wallet_counts(rows: &[EvmWalletRow]) -> (usize, usize) {
    let active = rows.iter().filter(|row| row.status == "active").count();
    let archived = rows.iter().filter(|row| row.status == "archived").count();
    (active, archived)
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
    /// The identity-rotation confirmation. Whether the dialog is open, and the vault passphrase
    /// it collects — a secret, wiped the moment it leaves for the worker like every secret
    /// field here, because the worker must re-seal the vault with the successor key in the same
    /// breath as the ceremony and holds no passphrase of its own after unlock.
    pub rotate_open: bool,
    pub rotate_passphrase: String,
    /// When this device last sealed a `.migo` container, as the worker reported it: from the
    /// vault at sign-in, refreshed after every export, cleared by every rotation. Unix seconds,
    /// the same form the container's own timestamp and the vault's stamp field speak, so the
    /// checkup's date and the container's idea of its birthday can never disagree.
    pub last_backup_at: Option<u64>,
    /// The recovery-contact answer, for the checkup's Recovery row.
    pub recovery: RecoveryView,
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
                    security_checkup_section(ui, context, state);
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

    // The rotation confirmation floats over the pane, in the foreground, whatever the scroll is
    // doing — a question this consequential does not scroll away.
    rotate_dialog(ui.ctx(), context, state);
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

/// The security checkup (§50): one line per fixed row — Identity, Devices, Wallets, Backup,
/// Recovery, E2EE — the same six on every client, because a person who checks the account on
/// their phone in the morning and this window at night should meet the same questions twice.
///
/// # What a row may honestly say
///
/// Each row draws from a fact this pane actually holds or can ask for: the identity key and the
/// rotation door, the device and wallet listings, the vault's backup stamp, the server's
/// recovery-contact bit. The E2EE row reports no account-wide "needs verification" count, and
/// that absence is the honest part: a peer's current identity key is observed only when a
/// conversation fetches its key bundles, so what this session has seen is a sample, not a
/// census, and a count over it would be a number pretending otherwise. The per-conversation
/// warning in the chat — beside the safety numbers it is verified with — is the real surface.
///
/// The Check button asks the three network facts together because they are asked together, and
/// a person pressing one button expects one round trip's worth of answers, not three rows that
/// refresh at three different moments.
fn security_checkup_section(ui: &mut Ui, context: &mut Context<'_>, state: &mut SettingsState) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Security checkup");

    let Some(account) = context.account else {
        // Signed out, there is no account to check: a sentence, not six rows of dashes that
        // would each imply a verdict waiting on a sign-in.
        ui.label(
            RichText::new("Sign in to check this account's security.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        return;
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("The six things that keep the account safe, one line each.")
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let busy = matches!(state.devices, Fetch::Loading)
                || matches!(state.wallets, Fetch::Loading)
                || matches!(state.recovery, RecoveryView::Loading);
            if ui
                .add_enabled(!busy, egui::Button::new("Check"))
                .on_hover_text("Asks the server for the device, wallet and recovery facts.")
                .clicked()
            {
                state.devices = Fetch::Loading;
                state.wallets = Fetch::Loading;
                state.recovery = RecoveryView::Loading;
                context.issue(Command::Devices);
                context.issue(Command::Wallets);
                context.issue(Command::ContactStanding);
            }
        });
    });
    ui.add_space(space::SM);

    // --- identity ---------------------------------------------------------------
    // The key is active by the fact of being signed in: every ceremony since the session began
    // was signed with it, or none of this pane's data would have arrived. The only variable is
    // where rotation can happen from, which is exactly what the row's tail names.
    let rotation = if account.holds_root {
        "rotation available from this device"
    } else {
        "rotation needs a device holding the root"
    };
    checkup_row(
        ui,
        context,
        "Identity",
        &format!("Identity key active \u{00B7} {rotation}"),
        colors.positive,
        None,
    );

    // --- devices ----------------------------------------------------------------
    match &state.devices {
        Fetch::NotAsked => checkup_row(
            ui,
            context,
            "Devices",
            "Not checked yet",
            colors.text_muted,
            None,
        ),
        Fetch::Loading => checkup_row(
            ui,
            context,
            "Devices",
            "Checking\u{2026}",
            colors.text_muted,
            None,
        ),
        Fetch::Unavailable(reason) => checkup_row(
            ui,
            context,
            "Devices",
            &format!("Could not check: {reason}"),
            colors.warning,
            None,
        ),
        Fetch::Ready(rows) => {
            if rows.is_empty() {
                checkup_row(
                    ui,
                    context,
                    "Devices",
                    "The server listed no devices.",
                    colors.text_muted,
                    None,
                );
            } else if let Some(old) = old_active_device(rows, Timestamp::now()) {
                let verdict = format!(
                    "Old device active \u{2014} {}, last seen {}",
                    widgets::elide(&old.display_name, 24),
                    crate::model::date(
                        old.last_seen
                            .expect("the overdue rule only fires on a row with a last-seen"),
                    ),
                );
                checkup_row(ui, context, "Devices", &verdict, colors.warning, Some(
                    "An active credential nobody has used in thirty days is either a forgotten \
                     device or somebody else's. Remove it from the device list below.",
                ));
            } else {
                let active = rows.iter().filter(|row| row.status == "active").count();
                checkup_row(
                    ui,
                    context,
                    "Devices",
                    &format!("{active} active, none overdue"),
                    colors.positive,
                    None,
                );
            }
        }
    }

    // --- wallets ----------------------------------------------------------------
    match &state.wallets {
        Fetch::NotAsked => checkup_row(
            ui,
            context,
            "Wallets",
            "Not checked yet",
            colors.text_muted,
            None,
        ),
        Fetch::Loading => checkup_row(
            ui,
            context,
            "Wallets",
            "Checking\u{2026}",
            colors.text_muted,
            None,
        ),
        Fetch::Unavailable(reason) => checkup_row(
            ui,
            context,
            "Wallets",
            &format!("Could not check: {reason}"),
            colors.warning,
            None,
        ),
        Fetch::Ready(rows) => {
            if rows.is_empty() {
                checkup_row(
                    ui,
                    context,
                    "Wallets",
                    "No wallet addresses are registered.",
                    colors.text_muted,
                    None,
                );
            } else {
                let (active, archived) = wallet_counts(rows);
                checkup_row(
                    ui,
                    context,
                    "Wallets",
                    &format!("{active} active \u{00B7} {archived} archived"),
                    colors.positive,
                    None,
                );
            }
        }
    }

    // --- backup -----------------------------------------------------------------
    match backup_check(account.holds_root, state.last_backup_at) {
        BackupCheck::NoRoot => checkup_row(
            ui,
            context,
            "Backup",
            "Backups are sealed on a device that holds the account root",
            colors.text_muted,
            None,
        ),
        BackupCheck::Never => checkup_row(
            ui,
            context,
            "Backup",
            "Never backed up on this device",
            colors.warning,
            Some(
                "Seal one under Account backup below. The container is what carries the account \
                 onto a new device \u{2014} root, identity key and the wallet addresses it derives.",
            ),
        ),
        BackupCheck::BackedUp(at) => {
            // Saturating twice over: a stamp this client wrote is unix seconds and small, but the
            // field is read back from a file, and a corrupted future date should render, not
            // panic.
            let when = crate::model::date(Timestamp::from_unix_ms(
                i64::try_from(at).unwrap_or(i64::MAX).saturating_mul(1000),
            ));
            checkup_row(
                ui,
                context,
                "Backup",
                &format!("Backed up {when}"),
                colors.positive,
                None,
            );
        }
    }

    // --- recovery ---------------------------------------------------------------
    match &state.recovery {
        RecoveryView::NotAsked => checkup_row(
            ui,
            context,
            "Recovery",
            "Not checked yet",
            colors.text_muted,
            None,
        ),
        RecoveryView::Loading => checkup_row(
            ui,
            context,
            "Recovery",
            "Checking\u{2026}",
            colors.text_muted,
            None,
        ),
        RecoveryView::Unavailable(reason) => checkup_row(
            ui,
            context,
            "Recovery",
            &format!("Could not check: {reason}"),
            colors.warning,
            None,
        ),
        RecoveryView::Ready(true) => checkup_row(
            ui,
            context,
            "Recovery",
            "Recovery contact set",
            colors.positive,
            None,
        ),
        RecoveryView::Ready(false) => checkup_row(
            ui,
            context,
            "Recovery",
            "Recovery contact not set",
            colors.warning,
            Some(
                "Set one under Sign-in below \u{2014} an email or a phone, and it is where a \
                 recovery starts.",
            ),
        ),
    }

    // --- e2ee -------------------------------------------------------------------
    checkup_row(
        ui,
        context,
        "E2EE",
        "On for every conversation",
        colors.positive,
        Some(
            "A peer's identity key changing is flagged in the conversation itself, beside the \
             safety numbers it is verified with.",
        ),
    );
}

/// One checkup line: the fixed row name on the left, the verdict on the right, with the verdict's
/// colour carrying the row's whole range — positive for a fact worth knowing, warning for one
/// worth acting on, muted for "not checked" or "not this device's to do".
///
/// The detail line, when there is one, points at where the action is: a warning that does not
/// name its own remedy is anxiety with a rounded corner.
fn checkup_row(
    ui: &mut Ui,
    context: &Context<'_>,
    name: &str,
    verdict: &str,
    color: egui::Color32,
    detail: Option<&str>,
) {
    let colors = palette(context.theme);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(name)
                .font(egui::FontId::proportional(font::BODY))
                .color(colors.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(verdict)
                    .text_style(crate::theme::named(text_style::CAPTION))
                    .color(color),
            );
        });
    });
    if let Some(text) = detail {
        ui.label(
            RichText::new(text)
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
    }
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
    // used to carry it left the shell. It is this device's *own* number — a name for its key, not
    // the number a conversation is verified with: that is the pair number in the chat itself, one
    // per peer device, the same string on both screens. Shown as a monospace block because a
    // person comparing a device's key with what its owner expects still deserves something they
    // can read digit by digit.
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
                "This device's own identity number. A conversation is verified with the pair \
                 number shown in the chat itself — the same number on both screens.",
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

    ui.add_space(space::LG);

    // --- identity key -----------------------------------------------------------
    // Rotation's door, at the account section's end because it is the account's own key and the
    // rarest thing here. A ghost button with the explanation one hover away, and the real
    // consequences in the dialog rather than the pane: the pane says what the button is, the
    // dialog says what it does, and nobody reaches the passphrase field without both.
    let holds_root = context.account.is_some_and(|account| account.holds_root);
    if holds_root {
        if widgets::ghost_button(ui, context.theme, "Rotate identity key")
            .on_hover_text(
                "Replaces the account's ML-DSA identity key with a fresh one held only by this \
                 device. The confirmation explains what that means before anything happens.",
            )
            .clicked()
        {
            state.rotate_open = true;
        }
    } else {
        ui.label(
            RichText::new(
                "Only a device that holds the account root can rotate the account's identity \
                 key. Seal or restore a backup on this device first.",
            )
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
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

/// The rotation confirmation: what changes, what does not, and the vault passphrase.
///
/// A floating window anchored over the pane rather than an inline form, the same shape the
/// shell's logout question takes, because rotation is a one-way account-wide action and should
/// be answered deliberately or not at all. The two paragraphs are the honest consequences — the
/// quiet half first (nothing anyone verified has to be verified again), the costly half second
/// (the new key exists only here, and everything else that holds the root still holds the old
/// one) — and the passphrase is required because the successor key is sealed into the vault in
/// the same breath as the ceremony: the worker holds no passphrase after unlock, by design.
fn rotate_dialog(ctx: &egui::Context, context: &mut Context<'_>, state: &mut SettingsState) {
    if !state.rotate_open {
        return;
    }
    let colors = palette(context.theme);
    let mut confirmed = false;
    // A question, not a form: no close of its own — Cancel, Escape and the confirm are the whole
    // answer set, and a question that can be X-ed away without being answered is a question that
    // can be ignored.
    let mut window_open = true;

    crate::ui::desktop::floating(
        context.theme,
        "Rotate identity key",
        egui::Id::new("migo-rotate-dialog"),
        egui::Pos2::ZERO,
        egui::vec2(420.0, 0.0),
        egui::vec2(420.0, 0.0),
    )
    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
    .order(egui::Order::Foreground)
    .resizable(false)
    .collapsible(false)
    .show(ctx, |ui| {
        ui.add_space(space::SM);
        ui.label(
            RichText::new("Replace the account's identity key?")
                .font(egui::FontId::proportional(font::BODY))
                .color(colors.text)
                .strong(),
        );
        ui.add_space(space::XS);
        ui.label(
            RichText::new(
                "A new ML-DSA identity key is generated on this device and becomes the \
                 account's; the old one is retired on the server. Your sessions, conversations, \
                 encryption keys and safety numbers continue unchanged — nothing anyone has \
                 verified needs verifying again.",
            )
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
        ui.add_space(space::XS);
        ui.label(
            RichText::new(
                "The new key lives only in this device's vault. Every other device that holds \
                 the account root — and any backup made before now — still carries the old key: \
                 a restore from an old backup will be refused until a fresh one is sealed here.",
            )
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        );
        ui.add_space(space::SM);
        widgets::field(
            ui,
            context.theme,
            "Vault passphrase",
            &mut state.rotate_passphrase,
            true,
            "the passphrase that unlocks this device",
        );
        ui.add_space(space::MD);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::ghost_button(ui, context.theme, "Cancel").clicked() {
                window_open = false;
            }
            ui.add_space(space::SM);
            let ready = !state.rotate_passphrase.is_empty();
            if widgets::primary_button(ui, context.theme, "Rotate identity key", ready).clicked() {
                confirmed = true;
                window_open = false;
            }
        });
    });

    // Escape cancels, the same answer the logout question gives the key.
    let escaped = ctx.input(|input| input.key_pressed(egui::Key::Escape));
    if !window_open || escaped {
        state.rotate_open = false;
        if confirmed {
            context.issue(Command::RotateIdentity {
                passphrase: state.rotate_passphrase.clone(),
            });
        }
        // Wiped whether it left or not: a dialog that kept a passphrase after closing would be a
        // second copy of the vault's secret, on a pane that also shows the device list.
        state.rotate_passphrase.clear();
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
        assert_eq!(SettingsState::default().recovery, RecoveryView::NotAsked);
        // And it must not claim a backup it has not been told about.
        assert_eq!(SettingsState::default().last_backup_at, None);
    }

    /// A device row for the age rule, parameterised by exactly the fields the rule reads.
    fn device(n: u8, status: &str, days_since_seen: Option<i64>) -> DeviceRow {
        let last_seen = days_since_seen.map(|days| {
            // The 2026-09-01 epoch of these tests, offset in whole days.
            let now = Timestamp::from_unix_ms(1_800_000_000_000);
            Timestamp::from_unix_ms(now.as_unix_ms() - days * 86_400_000)
        });
        DeviceRow {
            device_id: Id::from_bytes([n; 16]),
            display_name: format!("Device {n}"),
            platform: "desktop".to_owned(),
            status: status.to_owned(),
            created_at: None,
            last_seen,
            has_credential: true,
            is_current: n == 0,
        }
    }

    /// The age rule's whole judgement in one test: only an *active* device gone quiet past the
    /// window is the warning — a revoked one is history, an undisclosed last-seen is not a
    /// silence, and exactly thirty days is inside the window, because the rule says "older
    /// than", not "as old as".
    #[test]
    fn the_device_age_rule_flags_only_longsilent_active_devices() {
        let now = Timestamp::from_unix_ms(1_800_000_000_000);
        let silent = |days: i64| device(1, "active", Some(days));

        assert!(is_old_active_device(&silent(31), now));
        assert!(!is_old_active_device(&silent(30), now));
        assert!(!is_old_active_device(&silent(29), now));
        // Revoked: the account already dealt with it, whenever it was last seen.
        assert!(!is_old_active_device(&device(2, "revoked", Some(400)), now));
        // Pending: never held a live credential to warn about.
        assert!(!is_old_active_device(&device(3, "pending", Some(400)), now));
        // No last-seen at all: "not disclosed", not "not seen".
        assert!(!is_old_active_device(&device(4, "active", None), now));

        // The device asking is subject to the same rule as any other — its last-seen is this
        // session's, and a row that says otherwise is a discrepancy, not an exemption.
        assert!(is_old_active_device(&device(0, "active", Some(31)), now));
    }

    /// With several overdue devices the warning names the one silent longest, because that is
    /// the machine the sentence is about — not merely the first one the listing happened to
    /// order.
    #[test]
    fn the_old_device_warning_names_the_silent_longest() {
        let now = Timestamp::from_unix_ms(1_800_000_000_000);
        let rows = vec![
            device(1, "active", Some(31)),
            device(2, "active", Some(200)),
            device(3, "active", Some(45)),
        ];
        let named = old_active_device(&rows, now).expect("three overdue rows");
        assert_eq!(named.display_name, "Device 2");

        // And nothing to name when nothing qualifies.
        let quiet = vec![
            device(1, "active", Some(5)),
            device(2, "revoked", Some(200)),
        ];
        assert!(old_active_device(&quiet, now).is_none());
    }

    /// The backup row's three answers and the transitions between them: a seal dates it, a
    /// rotation undates it — a container sealed before the rotation cannot vouch the account,
    /// so the row must forget the date the dead container was made — and a device without the
    /// root is told where backups live rather than accused of missing one.
    #[test]
    fn the_backup_row_transitions_through_seal_rotation_and_rootlessness() {
        // A root-holding device that never sealed: the warning.
        assert_eq!(backup_check(true, None), BackupCheck::Never);

        // The export lands: dated, with the instant the container was sealed.
        assert_eq!(
            backup_check(true, Some(1_800_000_000)),
            BackupCheck::BackedUp(1_800_000_000)
        );

        // The rotation retires that container's right to vouch: back to never, not to a date.
        assert_eq!(backup_check(true, None), BackupCheck::Never);

        // A passenger device has nothing to seal, however the fields line up.
        assert_eq!(backup_check(false, None), BackupCheck::NoRoot);
        assert_eq!(
            backup_check(false, Some(1_800_000_000)),
            BackupCheck::NoRoot
        );
    }

    /// The recovery row keeps "could not check" and "not configured" apart, because only the
    /// second one is advice.
    #[test]
    fn the_recovery_view_files_outcomes_honestly() {
        assert_eq!(
            RecoveryView::from_result(Ok(true)),
            RecoveryView::Ready(true)
        );
        assert_eq!(
            RecoveryView::from_result(Ok(false)),
            RecoveryView::Ready(false)
        );
        assert_eq!(
            RecoveryView::from_result(Err("cannot reach the server".to_owned())),
            RecoveryView::Unavailable("cannot reach the server".to_owned())
        );
    }

    /// The wallet row counts by the listing's own statuses, and a status this build has no name
    /// for counts nowhere — the row's arithmetic is not where a new server status quietly
    /// becomes "archived".
    #[test]
    fn the_wallet_row_counts_the_two_statuses_it_names() {
        fn wallet(n: u8, status: &str) -> EvmWalletRow {
            EvmWalletRow {
                wallet_id: Id::from_bytes([n; 16]),
                address: format!("{n:040x}"),
                derivation_index: i32::from(n),
                status: status.to_owned(),
                label: None,
            }
        }

        assert_eq!(
            wallet_counts(&[
                wallet(1, "active"),
                wallet(2, "active"),
                wallet(3, "archived")
            ]),
            (2, 1)
        );
        assert_eq!(wallet_counts(&[]), (0, 0));
        assert_eq!(wallet_counts(&[wallet(9, "frozen")]), (0, 0));
    }
}
