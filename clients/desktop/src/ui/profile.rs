//! The profile pane: the account's own card, editable.
//!
//! # What a profile pane is for
//!
//! The public face of the account — display name, bio, the custom status line, and the privacy
//! of last-seen, messaging, and friend requests — as the web client's Profile panel and the
//! Android client's Profile screen draw it. The three clients are one product, so the fields
//! and their rules are shared, and the desktop's own contribution is the layout: a right-pane
//! panel beside the conversation, not a covering screen.
//!
//! # Absent means unchanged
//!
//! The server never sends the current privacy values back, even to their owner. So each
//! privacy control starts as "Leave as-is" and joins the save only once the user chooses;
//! a naive form pre-selected with a default would overwrite a deliberate choice with that
//! default. The searchable switch follows the same rule — its untouched state is "do not
//! touch" — while the birth year, which the wire does echo back, seeds from the card and
//! joins the patch exactly like the text fields: only when its draft differs from what the
//! server holds. A draft that names no plausible year is a typo, and the pane sends only
//! what it can stand behind.
//!
//! # The status rides a different wire
//!
//! The custom status is not a profile field: it publishes on the presence wire, beside the
//! account's last-known state, so saving a status never flips the account online or offline
//! as a side effect. The one honest wrinkle: the server this build talks to declines to
//! store a custom status (`presence_custom_status`), so the save is refused — a refusal that
//! arrives as the toast the presence wire already raises, which is the same treatment the
//! web and Android clients give it.

use egui::{Align, Layout, RichText, Ui};

use crate::model::OwnProfile;
use crate::net::Command;
use crate::theme::{font, palette, space, text_style};
use crate::ui::widgets;
use crate::ui::Context;

/// The birth-year bounds the pane accepts; anything else is left out of the patch, not sent.
const BIRTH_YEAR_MIN: u32 = 1900;
const BIRTH_YEAR_MAX: u32 = 2100;

/// What the profile pane holds between frames.
#[derive(Debug, Clone, Default)]
pub struct ProfileState {
    /// The account's own card, as last fetched or saved. `None` is "not loaded yet" — the
    /// pane asks on entry and a card that never arrives is the pane's own sentence, not a
    /// failure worth a toast.
    profile: Option<OwnProfile>,
    /// Why the last fetch or save was refused, in the server's own words. Filed rather than
    /// toasted because it belongs beside the form the user is looking at.
    failure: Option<String>,
    /// The save was accepted: shown for one line's worth of frames, cleared by the next edit.
    saved: bool,
    /// The form's draft display name.
    display_name: String,
    /// The form's draft bio.
    bio: String,
    /// The form's draft custom status.
    custom_status: String,
    /// The form's draft birth year — a string, because the field is a person typing digits.
    birth_year: String,
    /// The three privacy drafts, `-1` meaning "leave as-is" (absent from the save).
    show_last_seen: i8,
    who_can_message: i8,
    who_can_add: i8,
    /// The searchable draft: `-1` untouched, `0` off, `1` on.
    searchable: i8,
    /// The avatar picker's path draft. Not a save-section field: the avatar acts on its own
    /// button, and the path stays after an upload the way the backup path does, because a
    /// second change usually starts from a folder rather than a blank.
    avatar_path: String,
}

impl ProfileState {
    /// Files a freshly fetched or saved card, re-priming the form from it.
    ///
    /// The drafts are re-seeded from the card because the card is the truth — the save reply
    /// is the same shape a fetch returns — and the privacy drafts reset to untouched, since
    /// the choice the user just made is now the current setting the server holds.
    pub fn file(&mut self, profile: OwnProfile) {
        self.display_name = profile.display_name.clone();
        self.bio = profile.bio.clone().unwrap_or_default();
        self.custom_status = profile.custom_status.clone().unwrap_or_default();
        self.birth_year = profile
            .birth_year
            .map(|year| year.to_string())
            .unwrap_or_default();
        self.show_last_seen = -1;
        self.who_can_message = -1;
        self.who_can_add = -1;
        self.searchable = -1;
        self.saved = true;
        self.failure = None;
        self.profile = Some(profile);
    }

    /// Files the reason a fetch or save was refused, keeping the form as it stands.
    ///
    /// Public because the app's event loop is the caller: the refusal arrives as an event,
    /// and the pane's state is the app's field, not the pane's own.
    pub fn fail(&mut self, reason: String) {
        self.saved = false;
        self.failure = Some(reason);
    }

    /// Whether anything in the form is worth a save.
    ///
    /// The same rule the save itself applies: a patch that changes nothing is not a save, and
    /// a button that always works is a button that lies about having done something.
    fn dirty(&self, profile: &OwnProfile) -> bool {
        self.display_name.trim() != profile.display_name
            || self.bio != profile.bio.clone().unwrap_or_default()
            || self.custom_status.trim() != profile.custom_status.clone().unwrap_or_default()
            || self.birth_year
                != profile
                    .birth_year
                    .map(|year| year.to_string())
                    .unwrap_or_default()
            || self.show_last_seen >= 0
            || self.who_can_message >= 0
            || self.who_can_add >= 0
            || self.searchable >= 0
    }
}

/// Draws the profile pane.
///
/// Scrolls as one document, the settings pane's own shape, so a form long enough to push the
/// save button off the bottom of the window pushes it into reach of the wheel instead of out
/// of the interface.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut ProfileState) {
    let column = 460.0_f32.min(ui.available_width() - space::XL * 2.0);

    egui::ScrollArea::vertical()
        .id_salt("profile-pane")
        .max_height(ui.available_height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::XL);
                ui.allocate_ui(egui::vec2(column, 0.0), |ui| {
                    widgets::header(ui, context.theme, "Profile", Some("Your public face"));
                    ui.add_space(space::LG);

                    if let Some(reason) = &state.failure {
                        ui.label(
                            RichText::new(reason.clone())
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(palette(context.theme).warning),
                        );
                        ui.add_space(space::SM);
                    }
                    if state.saved {
                        ui.label(
                            RichText::new("Profile saved.")
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(palette(context.theme).positive),
                        );
                        ui.add_space(space::SM);
                    }

                    match state.profile.clone() {
                        None => {
                            ui.label(
                                RichText::new(
                                    "Profile not loaded yet — the pane asks the server when \
                                     you arrive.",
                                )
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(palette(context.theme).text_muted),
                            );
                        }
                        Some(profile) => {
                            identity_section(ui, context, state, &profile);
                            ui.add_space(space::LG);
                            form_section(ui, context, state, &profile);
                            ui.add_space(space::LG);
                            save_section(ui, context, state, &profile);
                        }
                    }
                    ui.add_space(space::XL);
                });
            });
        });
}

/// The read-only head: who this card is, in the shape every other surface shows it.
///
/// The avatar button lives here rather than in the form because it is not a draft — it acts
/// the moment it is pressed, uploading the file and pointing the profile at it in one
/// action, the same "two steps, one action" contract the web panel's change-photo carries.
fn identity_section(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ProfileState,
    profile: &OwnProfile,
) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Account");

    ui.label(
        RichText::new(if profile.display_name.is_empty() {
            profile.username.clone()
        } else {
            profile.display_name.clone()
        })
        .font(egui::FontId::proportional(font::BODY))
        .color(colors.text),
    );
    ui.add_space(space::XS);
    ui.label(
        RichText::new(format!("@{}", profile.username))
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
    );
    ui.add_space(space::XS);
    ui.label(
        RichText::new(&profile.public_id)
            .font(egui::FontId::monospace(font::SMALL))
            .color(colors.text_muted),
    );

    avatar_section(ui, context, state);
}

/// The avatar picker: a path, a button, and the honest limits.
///
/// egui has no file dialog in this build, so the path is typed — the same contract the
/// backup seal's "Save as" field already holds, and the only one this pane can stand behind.
/// The server's own policy caps the file at two mebibytes and re-judges the bytes at commit,
/// so the pane's job is only to say where the file is.
fn avatar_section(ui: &mut Ui, context: &mut Context<'_>, state: &mut ProfileState) {
    widgets::subheader(ui, context.theme, "Avatar");

    ui.label(
        RichText::new(
            "A local image, uploaded as your avatar: PNG, JPEG, WebP, GIF or AVIF, up to 2 MiB. \
             The server judges the bytes, not the name.",
        )
        .font(egui::FontId::proportional(font::SMALL))
        .color(palette(context.theme).text_muted),
    );
    ui.add_space(space::SM);
    widgets::field(
        ui,
        context.theme,
        "Image file",
        &mut state.avatar_path,
        false,
        "e.g. ~/Pictures/me.png",
    );
    let ready = !state.avatar_path.trim().is_empty();
    if widgets::primary_button(ui, context.theme, "Change photo", ready)
        .on_hover_text("Uploads the image and points your profile at it — one action.")
        .clicked()
    {
        let path = std::path::PathBuf::from(state.avatar_path.trim());
        context.issue(Command::ChangeAvatar { path });
    }
}

/// The editable fields: name, bio, status, birth year, privacy.
///
/// The card rides in for the field hints the drafts cannot know — the status line's current
/// value, shown through the draft until it is edited — and for the field-level comparison a
/// placeholder cannot stand behind.
fn form_section(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ProfileState,
    _profile: &OwnProfile,
) {
    let colors = palette(context.theme);
    widgets::subheader(ui, context.theme, "Edit profile");

    widgets::field(
        ui,
        context.theme,
        "Display name",
        &mut state.display_name,
        false,
        "Your name",
    );
    widgets::field(
        ui,
        context.theme,
        "Bio",
        &mut state.bio,
        false,
        "A line about you",
    );
    widgets::field(
        ui,
        context.theme,
        "Custom status",
        &mut state.custom_status,
        false,
        "What are you up to?",
    );
    ui.label(
        RichText::new("Shown beside your presence, everywhere your name appears.")
            .font(egui::FontId::proportional(font::TINY))
            .color(colors.text_muted),
    );
    ui.add_space(space::SM);
    widgets::field(
        ui,
        context.theme,
        "Birth year (optional)",
        &mut state.birth_year,
        false,
        "Not disclosed",
    );
    ui.label(
        RichText::new("Not public; visible only on your own profile pane.")
            .font(egui::FontId::proportional(font::TINY))
            .color(colors.text_muted),
    );
    ui.add_space(space::LG);

    widgets::subheader(ui, context.theme, "Privacy");
    ui.label(
        RichText::new(
            "Current settings are private, so each control starts as \u{201c}Leave as-is\u{201d}; \
             only a choice you make is saved.",
        )
        .font(egui::FontId::proportional(font::TINY))
        .color(colors.text_muted),
    );
    ui.add_space(space::SM);

    privacy_choice(
        ui,
        context,
        "Who sees your last seen",
        &mut state.show_last_seen,
    );
    privacy_choice(
        ui,
        context,
        "Who can message you",
        &mut state.who_can_message,
    );
    privacy_choice(
        ui,
        context,
        "Who can add you as a friend",
        &mut state.who_can_add,
    );
    ui.add_space(space::SM);

    // The searchable switch: three states in two positions, because "leave as-is" is the
    // only honest default for a value the server will not show back.
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Appear in username search")
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text),
        );
        let label = match state.searchable {
            -1 => "leave as-is",
            0 => "off",
            _ => "on",
        };
        if ui.button(label).clicked() {
            state.searchable = if state.searchable <= 0 { 1 } else { 0 };
        }
    });
    ui.label(
        RichText::new(
            "Your current setting is private; the switch joins the save only once you flip it.",
        )
        .font(egui::FontId::proportional(font::TINY))
        .color(colors.text_muted),
    );
    ui.add_space(space::LG);
}

/// The save button, and the one rule it carries: only a form with something to save works.
fn save_section(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ProfileState,
    profile: &OwnProfile,
) {
    let dirty = state.dirty(profile);
    if widgets::primary_button(ui, context.theme, "Save changes", dirty)
        .on_hover_text(
            "Only the fields you changed are sent; everything else keeps its server-side value.",
        )
        .clicked()
    {
        let patch = build_patch(state, profile);
        if let Some(patch) = patch {
            context.issue(patch);
        }
        // The status saves on its own wire, so it does not wait for the profile patch.
        let status = state.custom_status.trim().to_owned();
        if status != profile.custom_status.clone().unwrap_or_default() {
            context.issue(Command::SaveStatus { status });
        }
        state.saved = false;
        state.failure = None;
    }
}

/// One privacy control: a label plus a dropdown whose first entry is "leave as-is".
///
/// The choice is an index, `-1` for untouched, `0`-`2` for nobody / friends / everyone — the
/// wire's own values, so the save sends the choice without translation.
fn privacy_choice(ui: &mut Ui, context: &mut Context<'_>, label: &str, choice: &mut i8) {
    let colors = palette(context.theme);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let options = ["Leave as-is", "Nobody", "Friends", "Everyone"];
            let selected = options[(*choice).clamp(0, 3) as usize];
            egui::ComboBox::from_id_salt(label)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for (index, option) in options.iter().enumerate() {
                        let value = index as i8 - 1;
                        if ui.selectable_label(*choice == value, *option).clicked() {
                            *choice = value;
                        }
                    }
                });
        });
    });
    ui.add_space(space::SM);
}

/// The numeric birth year the draft names, or `None` when it names nothing sendable.
fn valid_birth_year(raw: &str) -> Option<u32> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let year = text.parse::<u32>().ok()?;
    (BIRTH_YEAR_MIN..=BIRTH_YEAR_MAX)
        .contains(&year)
        .then_some(year)
}

/// The patch a dirty form sends, or `None` when the drafts changed nothing on this wire.
///
/// Every field applies the same rule: it joins the patch only when it differs from the card,
/// and the privacy choices only when a choice was made at all. The birth year follows the
/// text fields' rule now that the wire echoes it back: the draft joins only when it differs
/// from the year the card carries, and a differing draft must still name a plausible year —
/// "carbuncle" is a typo, and the pane sends only what it can stand behind. The result is a
/// save that touches exactly what the user touched — the wire's absent-means-unchanged
/// contract.
fn build_patch(state: &ProfileState, profile: &OwnProfile) -> Option<Command> {
    let current_year = profile
        .birth_year
        .map(|year| year.to_string())
        .unwrap_or_default();
    let birth_year = if state.birth_year != current_year {
        valid_birth_year(&state.birth_year)
    } else {
        None
    };
    let patch = crate::net::ProfilePatch {
        display_name: (state.display_name.trim() != profile.display_name)
            .then(|| state.display_name.trim().to_owned()),
        bio: (state.bio != profile.bio.clone().unwrap_or_default()).then(|| state.bio.clone()),
        birth_year,
        show_last_seen: choice_value(state.show_last_seen),
        who_can_message: choice_value(state.who_can_message),
        who_can_add: choice_value(state.who_can_add),
        searchable: (state.searchable >= 0).then_some(state.searchable == 1),
    };
    let changed = patch.display_name.is_some()
        || patch.bio.is_some()
        || patch.birth_year.is_some()
        || patch.show_last_seen.is_some()
        || patch.who_can_message.is_some()
        || patch.who_can_add.is_some()
        || patch.searchable.is_some();
    changed.then_some(Command::SaveProfile(patch))
}

/// The wire value a privacy choice names, or `None` for untouched.
fn choice_value(choice: i8) -> Option<u32> {
    match choice {
        0..=2 => Some(choice as u32),
        _ => None,
    }
}
