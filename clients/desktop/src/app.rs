//! The application: one state struct, one frame function, one event drain.
//!
//! # Where state changes happen
//!
//! Exactly two places, and nowhere else. [`App::drain`] applies events from the worker, and the
//! command buffer returned by a screen is forwarded after the frame. A widget deep in a layout closure
//! never reaches back into application state, which is what keeps the borrow checker quiet without
//! interior mutability and, more usefully, what makes the sequence of changes in a frame something you
//! can read off two functions rather than reconstruct from a call tree.
//!
//! # Repaints
//!
//! egui is a reactive UI: it repaints on input and otherwise sleeps. That is the right default for
//! battery, but this application has two things that change without input. Arriving events are handled
//! by the worker calling `request_repaint` on the context it was given. Toast fade-out is handled here,
//! by requesting a repaint only while a toast is on screen.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::ServerEndpoint;
use crate::model::{Account, Connection, Toast, ToastKind};
use crate::net::{Command, Event, Net};
use crate::settings::{self, Settings};
use crate::theme::{self, palette, space, Theme};
use crate::ui::alerts::AlertsState;
use crate::ui::auth::AuthState;
use crate::ui::chat::ChatState;
use crate::ui::friends::FriendsState;
use crate::ui::rooms::RoomsState;
use crate::ui::search::SearchState;
use crate::ui::settings::SettingsState;
use crate::ui::space::SpaceState;
use crate::ui::wallet::WalletState;
use crate::ui::{widgets, Context, Place, Screen};

/// The whole application state.
pub struct App {
    theme: Theme,
    net: Net,
    screen: Screen,
    /// Which pane the tab strip has selected, when no conversation tab is active.
    place: Place,
    /// The open conversation tabs, in open order — the strip's closable chat chips.
    open_chats: Vec<migo_core::Id>,
    /// The conversation whose thread is showing, when a chat tab is the active tab.
    active_chat: Option<migo_core::Id>,
    /// The open secondary panels, in open order — the strip's closable panel chips.
    open_panels: Vec<Place>,
    connection: Connection,
    account: Option<Account>,
    auth: AuthState,
    chat: ChatState,
    friends: FriendsState,
    settings_panel: SettingsState,
    rooms: RoomsState,
    space: SpaceState,
    alerts: AlertsState,
    search: SearchState,
    wallet: WalletState,
    /// The merged activity stream, rebuilt whenever either durable half moves.
    activity: Vec<crate::model::ActivityRow>,
    toasts: Vec<Toast>,
    /// Reused each frame so a screen's command buffer costs no allocation per frame.
    commands: Vec<Command>,
    /// The persisted settings record: the source of truth for what gets written back to disk,
    /// updated field by field as the user changes things. Kept apart from the live values so
    /// that an environment override of the server (see `main`) never leaks into the file through
    /// an unrelated save.
    settings: Settings,
    /// The path the settings file lives at, when the platform data directory is reachable.
    /// None disables persistence; the form still works, the choice just does not survive a reload.
    settings_path: Option<PathBuf>,
}

impl App {
    /// Builds the application and starts the network worker.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        vault_path: PathBuf,
        server: ServerEndpoint,
    ) -> Self {
        // The settings file is read here as well as in `main` (which wants only the server):
        // the theme lives in the same small file, and one more read at startup is cheaper than
        // a fifth parameter on `new`.
        let settings_path = crate::settings::Settings::default_path();
        let settings = settings_path
            .as_deref()
            .map(settings::load_or_default)
            .unwrap_or_else(Settings::default_for_dev);
        // Follow the desktop's own light or dark setting until the user has toggled a
        // preference of their own. Overriding it before that would mean a window that does not
        // match every other window on the machine, and the user did not ask for that.
        let theme = settings
            .theme
            .unwrap_or_else(|| Theme::from_system(&cc.egui_ctx));
        theme::install(&cc.egui_ctx, theme);

        let net = Net::spawn(cc.egui_ctx.clone(), vault_path);
        // The server address is the one field the caller decides; everything else on the auth form
        // starts empty, and a passphrase field pre-filled from anywhere would be a bug.
        // The server is the same endpoint the user just set; the auth form's own
        // `server` field starts from the loopback default and the user can
        // overtype it from the form's server disclosure.
        let auth = AuthState {
            server: server.clone(),
            ..AuthState::default()
        };

        Self {
            theme,
            net,
            screen: Screen::Opening,
            place: Place::Chat,
            open_chats: Vec::new(),
            active_chat: None,
            open_panels: Vec::new(),
            connection: Connection::Offline,
            account: None,
            auth,
            chat: ChatState::default(),
            friends: FriendsState::default(),
            settings_panel: SettingsState::default(),
            rooms: RoomsState::default(),
            space: SpaceState::default(),
            alerts: AlertsState::default(),
            search: SearchState::default(),
            wallet: WalletState::default(),
            activity: Vec::new(),
            toasts: Vec::new(),
            commands: Vec::new(),
            settings,
            settings_path,
        }
    }

    /// Persists the settings record, when the platform data directory is reachable.
    /// Best-effort: a failed write is logged at warn level, never surfaced as a toast, because
    /// the in-memory state is the source of truth for the current session.
    fn persist_settings(&self) {
        let Some(path) = self.settings_path.as_ref() else {
            return;
        };
        if let Err(error) = settings::save(path, &self.settings) {
            tracing::warn!("migo-desktop: could not persist settings: {error}");
        }
    }

    /// Applies every event the worker has produced since the last frame.
    fn drain(&mut self) {
        while let Some(event) = self.net.try_recv() {
            match event {
                Event::Connection(state) => {
                    // A failure while a form is submitting has to release the form, or the button stays
                    // disabled and the user is stuck looking at an error they cannot act on.
                    if matches!(state, Connection::Failed(_)) {
                        self.auth.busy = false;
                    }
                    self.connection = state;
                }
                Event::VaultFound => {
                    if self.account.is_none() {
                        self.screen = Screen::Unlock;
                    }
                }
                Event::VaultMissing => {
                    if self.account.is_none() {
                        self.screen = Screen::Register;
                    }
                }
                Event::SignedIn(account) => {
                    self.auth.busy = false;
                    self.auth.clear_secrets();
                    // Whatever challenge the form was holding died with the submit that
                    // succeeded; the next sign-in starts from a fresh fetch, not a stale image.
                    self.auth.captcha.reset();
                    self.account = Some(account);
                    // A session starts at its dashboard, and with none of the previous session's
                    // graph or device list: those describe an account, and this may be a
                    // different one signing in over the same window.
                    self.place = Place::Chat;
                    self.open_chats.clear();
                    self.active_chat = None;
                    self.open_panels.clear();
                    self.friends = FriendsState::default();
                    self.settings_panel = SettingsState::default();
                    self.rooms = RoomsState::default();
                    self.space = SpaceState::default();
                    self.alerts = AlertsState::default();
                    self.search = SearchState::default();
                    self.wallet = WalletState::default();
                    self.activity.clear();
                    self.screen = Screen::Chat;
                    // The banner carries the balance, so the session's first reads include the
                    // wallet the same way they include the conversation list.
                    self.commands.push(Command::Wallet);
                }
                Event::SignedOut => {
                    self.account = None;
                    self.auth.busy = false;
                    self.auth.clear_secrets();
                    self.auth.captcha.reset();
                    // Drop every decrypted message with the session. Leaving a thread on screen after
                    // sign-out would mean plaintext outliving the keys that produced it, which is the
                    // one thing a signed-out client must not do.
                    self.chat = ChatState::default();
                    // The social graph is not secret in the way messages are, but it is an account's
                    // business: names, requests, who is online. It goes with the session for the same
                    // reason the threads do, and the pane starts its next session NotAsked.
                    self.friends = FriendsState::default();
                    self.settings_panel = SettingsState::default();
                    self.rooms = RoomsState::default();
                    self.space = SpaceState::default();
                    self.alerts = AlertsState::default();
                    self.search = SearchState::default();
                    self.wallet = WalletState::default();
                    self.activity.clear();
                    self.place = Place::Chat;
                    self.open_chats.clear();
                    self.active_chat = None;
                    self.open_panels.clear();
                    self.screen = Screen::Unlock;
                }
                Event::CaptchaChallenge(challenge) => self.auth.captcha.hold(challenge),
                Event::CaptchaUnavailable { reason } => self.auth.captcha.unavailable(reason),
                Event::CaptchaRefused => self.auth.captcha.refused(),
                Event::Conversations(list) => self.chat.set_conversations(list),
                Event::History {
                    conversation_id,
                    messages,
                } => {
                    self.chat.absorb_history(conversation_id, messages);
                }
                Event::Message(message) => {
                    let conversation_id = message.conversation_id;
                    let preview = message.body.preview();
                    let at = message.sent_at;
                    let incoming = !message.outgoing;
                    self.chat.absorb(message);

                    if let Some(conversation) = self
                        .chat
                        .conversations
                        .iter_mut()
                        .find(|c| c.conversation_id == conversation_id)
                    {
                        conversation.preview = Some(preview);
                        conversation.updated_at = Some(at);
                        // Only count it unread when the conversation is not the one being read. A badge
                        // on the conversation someone is looking at is noise.
                        if incoming && self.chat.selected != Some(conversation_id) {
                            conversation.unread = conversation.unread.saturating_add(1);
                        }
                    } else {
                        // A message for a conversation not in the list yet: ask for the list rather
                        // than inventing an entry from one message, which would get the member set and
                        // the encryption mode wrong.
                        self.commands.push(Command::Conversations);
                    }
                }
                Event::Accepted {
                    message_id,
                    conversation_id,
                    seq,
                } => {
                    self.chat.accept(conversation_id, message_id, seq);
                }
                Event::SendFailed { message_id } => self.chat.reject(message_id),
                Event::Typing {
                    conversation_id,
                    user_id,
                    typing,
                } => {
                    let who = self.chat.typing.entry(conversation_id).or_default();
                    who.retain(|id| *id != user_id);
                    if typing {
                        who.push(user_id);
                    }
                }
                Event::Names(names) => {
                    merge_names(&mut self.chat.names, names.clone());
                    self.friends.merge_names(names);
                }
                Event::Relationships(entries) => {
                    self.friends.set_relationships(entries);
                }
                Event::FriendChanged { user_id, accepted } => {
                    // The event says the graph moved, not how, so the response is a re-read rather
                    // than a patch: the refreshed list is the truth and the toast is the telling.
                    // The name may not be resolved yet — the profile fetch that names a new
                    // requester rides the same refresh this queues — so the id's tail stands in
                    // for one toast's worth of time. A state this build cannot name is toasted
                    // not at all: "something happened" is already what the refresh says.
                    if let Some(accepted) = accepted {
                        let who = self
                            .friends
                            .names
                            .get(&user_id)
                            .cloned()
                            .unwrap_or_else(|| crate::model::short_id(user_id));
                        let text = if accepted {
                            format!("{who} is now a friend")
                        } else {
                            format!("New friend request from {who}")
                        };
                        self.toasts.push(Toast::info(text));
                    }
                    self.commands.push(Command::Friends);
                }
                Event::PresenceChanged { user_id, state } => {
                    self.friends.set_presence(user_id, state);
                }
                Event::Sessions(result) => {
                    self.settings_panel.sessions =
                        crate::ui::settings::SessionsView::from_result(result);
                }
                Event::Rooms(rows) => {
                    self.rooms.rooms = rows;
                    self.rooms.loaded = true;
                }
                Event::RoomJoined {
                    conversation_id,
                    room_id,
                    title,
                } => {
                    self.rooms.joined.insert(room_id, conversation_id);
                    self.toasts.push(Toast::success(format!("Joined {title}")));
                    self.open_conversation(conversation_id);
                }
                Event::RoomLeft { room_id } => {
                    self.rooms.joined.remove(&room_id);
                    self.toasts.push(Toast::info("Left the room"));
                }
                Event::Alerts(rows) => {
                    self.alerts.items = rows;
                    self.alerts.loaded = true;
                    self.rebuild_activity();
                }
                Event::AlertPushed => {
                    // The push is the cue to re-read whatever inbox-shaped surface is showing.
                    self.commands.push(Command::Notifications);
                }
                Event::Balance { coins, points } => {
                    self.wallet.coins = Some(coins);
                    self.wallet.points = Some(points);
                }
                Event::Ledger(rows) => {
                    self.wallet.ledger = rows;
                    self.rebuild_activity();
                }
                Event::ProgressionArrived(progression) => {
                    self.wallet.progression = Some(progression);
                }
                Event::Badges(codes) => {
                    self.wallet.badges = codes;
                }
                Event::Leaderboard(rows) => {
                    self.wallet.leaders = rows;
                }
                Event::Gifts(rows) => {
                    self.wallet.gifts = rows;
                }
                Event::People(rows) => {
                    if self.search.query.trim().is_empty() {
                        // The graph's own suggestions, kept for the pre-query state.
                        self.search.suggestions = rows;
                    } else {
                        self.search.people = Some(rows);
                        self.search.busy = false;
                    }
                }
                Event::Toast { text, kind } => self.toasts.push(match kind {
                    ToastKind::Info => Toast::info(text),
                    ToastKind::Success => Toast::success(text),
                    ToastKind::Error => Toast::error(text),
                }),
            }
        }
    }

    /// Ages the toast stack and reports whether one is still on screen.
    fn age_toasts(&mut self, delta: f32) -> bool {
        for toast in &mut self.toasts {
            toast.remaining -= delta;
        }
        self.toasts.retain(|toast| toast.remaining > 0.0);
        // Only the three most recent are kept. A stack that grows without bound covers the composer,
        // and older toasts describe a situation the newer ones have already superseded.
        while self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        !self.toasts.is_empty()
    }

    /// The navigation strip: five system tabs, then a closable chip per open conversation and per
    /// open panel — the reference's MDI tab model, worn the same at every width.
    ///
    /// The strip scrolls horizontally rather than hiding anything, because a tab that is
    /// off-screen is still a tab: hiding it would strand the surface it names behind no control
    /// at all.
    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let colors = palette(self.theme);
        // Resolved before the panel closure so the chip loop never borrows the chat state it
        // mutates through the click handlers.
        let unread: u32 = self.chat.conversations.iter().map(|c| c.unread).sum();
        let account_id = self.account.as_ref().map(|a| a.account_id);
        let chat_tabs: Vec<(migo_core::Id, String)> = self
            .open_chats
            .iter()
            .map(|id| {
                let title = self
                    .chat
                    .conversations
                    .iter()
                    .find(|c| c.conversation_id == *id)
                    .map(|c| {
                        account_id
                            .map(|me| c.display_title(me, &self.chat.names))
                            .unwrap_or_else(|| crate::model::short_id(*id))
                    })
                    .unwrap_or_else(|| crate::model::short_id(*id));
                (*id, title)
            })
            .collect();
        let panels: Vec<Place> = self.open_panels.clone();
        let active_chat = self.active_chat;
        let place = self.place;

        let mut selected: Option<Place> = None;
        let mut chat_pick: Option<migo_core::Id> = None;
        let mut chat_close: Option<migo_core::Id> = None;
        let mut panel_pick: Option<Place> = None;
        let mut panel_close: Option<Place> = None;

        egui::Panel::top("tab-strip")
            .exact_size(46.0)
            .frame(egui::Frame::new().fill(colors.nav))
            .show(ui, |ui| {
                ui.add_space(space::XS);
                egui::ScrollArea::horizontal()
                    .id_salt("tabs")
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, _viewport| {
                        ui.horizontal_centered(|ui| {
                            for candidate in Place::SYSTEM_TABS {
                                let label = if candidate == Place::Chat && unread > 0 {
                                    format!("Chats ({unread})")
                                } else {
                                    candidate.label().to_owned()
                                };
                                let outcome = widgets::tab_chip(
                                    ui,
                                    self.theme,
                                    &label,
                                    Some(candidate),
                                    active_chat.is_none() && place == candidate,
                                    false,
                                );
                                if outcome.clicked {
                                    selected = Some(candidate);
                                }
                            }
                            for (conversation_id, title) in &chat_tabs {
                                let outcome = widgets::tab_chip(
                                    ui,
                                    self.theme,
                                    title,
                                    None,
                                    active_chat == Some(*conversation_id),
                                    true,
                                );
                                if outcome.clicked {
                                    chat_pick = Some(*conversation_id);
                                }
                                if outcome.closed {
                                    chat_close = Some(*conversation_id);
                                }
                            }
                            for panel in &panels {
                                let outcome = widgets::tab_chip(
                                    ui,
                                    self.theme,
                                    panel.label(),
                                    Some(*panel),
                                    active_chat.is_none() && place == *panel,
                                    true,
                                );
                                if outcome.clicked {
                                    panel_pick = Some(*panel);
                                }
                                if outcome.closed {
                                    panel_close = Some(*panel);
                                }
                            }
                        });
                    });
            });

        if let Some(target) = selected {
            self.select_place(target);
        }
        if let Some(conversation_id) = chat_pick {
            self.select_chat(conversation_id);
        }
        if let Some(conversation_id) = chat_close {
            self.close_chat(conversation_id);
        }
        if let Some(panel) = panel_pick {
            self.select_place(panel);
        }
        if let Some(panel) = panel_close {
            self.close_panel(panel);
        }
    }

    /// The profile banner: the orange gradient that owns the account.
    ///
    /// The reference puts the avatar, the name, the balance and the way out here, and the account
    /// menu is the honest mapping of what this client carries: profile facts live in settings,
    /// credits live in the wallet, and the exit is the sign-out.
    fn banner(&mut self, ui: &mut egui::Ui, theme_choice: &mut Option<Theme>) {
        let colors = palette(self.theme);
        let username = self
            .account
            .as_ref()
            .map(|account| account.username.clone())
            .unwrap_or_default();
        let coins = self.wallet.coins;
        // The state word for the banner's dot, resolved here so the panel closure never borrows
        // the connection it is drawn beside.
        let (dot_color, dot_label) = {
            let colors_local = colors;
            match &self.connection {
                Connection::Online => (colors_local.positive, "Connected"),
                Connection::Connecting => (colors_local.warning, "Connecting"),
                Connection::Offline => (colors_local.banner_ink, "Offline"),
                Connection::Fallback(_) => (colors_local.accent, "Connected"),
                Connection::Failed(_) => (colors_local.danger, "Disconnected"),
            }
        };
        let connection_detail = self.connection.label().to_owned();

        let mut menu: Option<Place> = None;
        let mut sign_out = false;
        let mut flip_theme = false;

        egui::Panel::top("banner")
            .exact_size(58.0)
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let rect = ui.max_rect();
                widgets::gradient_rect(ui, rect, colors.banner_a, colors.banner_b, colors.banner_c);
                ui.horizontal_centered(|ui| {
                    ui.add_space(space::MD);
                    widgets::banner_avatar(ui, self.theme, &username, 32.0);
                    ui.add_space(space::SM);
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(widgets::elide(&username, 20))
                                .font(egui::FontId::proportional(crate::theme::font::SUBTITLE))
                                .color(colors.banner_ink)
                                .strong(),
                        );
                        // The connection dot travels with the name: the one live fact about the
                        // session, stated where the account is.
                        ui.horizontal(|ui| {
                            let (dot, _) =
                                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot.center(), 3.5, dot_color);
                            ui.label(
                                egui::RichText::new(dot_label)
                                    .font(egui::FontId::proportional(crate::theme::font::TINY))
                                    .color(colors.banner_ink),
                            )
                            .on_hover_text(&connection_detail);
                        });
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(space::MD);
                        // Sun while dark, moon while light: the glyph names the theme one click
                        // would arrive at, drawn as ink on the banner.
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(if self.theme.is_dark() {
                                        "\u{2600}"
                                    } else {
                                        "\u{1F319}"
                                    })
                                    .color(colors.banner_ink),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .on_hover_text(format!("Switch to {}", self.theme.flipped().label()))
                            .clicked()
                        {
                            flip_theme = true;
                        }
                        // The account menu, opened from the chevron beside the avatar — the
                        // reference's dropdown, carrying the three things it offers.
                        let mut opened = None;
                        let mut out = false;
                        ui.scope(|ui| {
                            let w = &mut ui.style_mut().visuals.widgets;
                            w.inactive.bg_fill = egui::Color32::TRANSPARENT;
                            w.inactive.bg_stroke = egui::Stroke::NONE;
                            w.hovered.bg_fill = egui::Color32::from_black_alpha(60);
                            w.hovered.bg_stroke = egui::Stroke::NONE;
                            w.active.bg_fill = egui::Color32::from_black_alpha(90);
                            w.active.bg_stroke = egui::Stroke::NONE;
                            ui.menu_button(
                                egui::RichText::new("\u{25BE}")
                                    .color(colors.banner_ink)
                                    .strong(),
                                |ui| {
                                    if ui.button("My Profile").clicked() {
                                        opened = Some(Place::Settings);
                                        ui.close();
                                    }
                                    if ui.button("My Credits & TopUp").clicked() {
                                        opened = Some(Place::Wallet);
                                        ui.close();
                                    }
                                    // The two panes the reference keeps off the strip: they
                                    // arrive here as tabs of their own, the same chips a
                                    // conversation opens.
                                    if ui.button("Alerts").clicked() {
                                        opened = Some(Place::Alerts);
                                        ui.close();
                                    }
                                    if ui.button("Search").clicked() {
                                        opened = Some(Place::Search);
                                        ui.close();
                                    }
                                    if ui.button("Exit / Logout").clicked() {
                                        out = true;
                                        ui.close();
                                    }
                                },
                            );
                        });
                        menu = opened;
                        sign_out = out;
                        // The balance chip: the session's real $MIG, dark on the gradient.
                        if let Some(coins) = coins {
                            widgets::pill(
                                ui,
                                &format!("{coins} $MIG"),
                                colors.banner_ink,
                                egui::Color32::from_black_alpha(90),
                            );
                        }
                    });
                });
            });

        if let Some(target) = menu {
            self.select_place(target);
        }
        if sign_out {
            self.commands.push(Command::SignOut);
        }
        if flip_theme {
            *theme_choice = Some(self.theme.flipped());
        }
    }

    /// Selects a place: the secondary panels arrive as tabs of their own.
    fn select_place(&mut self, target: Place) {
        self.active_chat = None;
        if target.is_panel() && !self.open_panels.contains(&target) {
            self.open_panels.push(target);
        }
        self.place = target;
        self.entered_place(target);
    }

    /// Activates a conversation's tab, opening it if it is not open yet.
    fn select_chat(&mut self, conversation_id: migo_core::Id) {
        if self.chat.selected != Some(conversation_id) {
            self.open_conversation(conversation_id);
            return;
        }
        if !self.open_chats.contains(&conversation_id) {
            self.open_chats.push(conversation_id);
        }
        self.active_chat = Some(conversation_id);
    }

    /// Registers an open conversation as a tab and makes it the active one.
    fn activate_chat(&mut self, conversation_id: migo_core::Id) {
        if !self.open_chats.contains(&conversation_id) {
            self.open_chats.push(conversation_id);
        }
        self.active_chat = Some(conversation_id);
    }

    /// Closes a conversation's tab: the thread falls through to the most recently opened one, or
    /// back to whatever place was showing beneath it.
    fn close_chat(&mut self, conversation_id: migo_core::Id) {
        self.open_chats.retain(|id| *id != conversation_id);
        if self.active_chat == Some(conversation_id) {
            match self.open_chats.last().copied() {
                Some(next) => {
                    self.active_chat = Some(next);
                    self.open_conversation(next);
                }
                None => {
                    self.active_chat = None;
                    if self.chat.selected == Some(conversation_id) {
                        self.chat.selected = None;
                    }
                }
            }
        }
    }

    /// Closes a panel's tab, falling back to the conversation list if it was the showing pane.
    fn close_panel(&mut self, panel: Place) {
        self.open_panels.retain(|p| *p != panel);
        if self.active_chat.is_none() && self.place == panel {
            self.place = Place::Chat;
        }
    }

    /// The per-place first reads when a place is entered.
    ///
    /// A place whose facts are the server's re-reads on entry, for the same reason the friends
    /// graph does: the other devices of this account act on it too.
    fn entered_place(&mut self, target: Place) {
        match target {
            Place::Friends => self.commands.push(Command::Friends),
            Place::Rooms => self.commands.push(Command::Rooms {
                query: self.rooms.query.clone(),
            }),
            Place::Alerts | Place::Feed => self.commands.push(Command::Notifications),
            Place::Wallet => self.commands.push(Command::Wallet),
            Place::Search => {
                if self.search.suggestions.is_empty() {
                    self.commands.push(Command::Suggestions);
                }
            }
            Place::Chat | Place::Games | Place::Settings => {}
        }
    }

    /// Opens a conversation from outside the chat pane — a list row, a joined room, a search hit.
    ///
    /// The same [`crate::ui::chat::open`] the conversation list uses, driven with a scratch
    /// command buffer because the event that calls this arrives outside the frame that owns the
    /// real one. Opening a conversation is opening a tab: the chip lands on the strip and the
    /// thread becomes the active surface.
    fn open_conversation(&mut self, conversation_id: migo_core::Id) {
        let mut commands = std::mem::take(&mut self.commands);
        let mut navigate = None;
        let mut theme_choice = None;
        let mut open_place = None;
        let server = self.auth.server.clone();
        let mut context = Context {
            theme: self.theme,
            connection: &self.connection,
            account: self.account.as_ref(),
            server: &server,
            commands: &mut commands,
            navigate: &mut navigate,
            theme_choice: &mut theme_choice,
            open_place: &mut open_place,
        };
        crate::ui::chat::open(&mut context, &mut self.chat, conversation_id);
        self.commands = commands;
        self.activate_chat(conversation_id);
    }

    /// Rebuilds the merged activity stream from its durable halves.
    fn rebuild_activity(&mut self) {
        self.activity = crate::ui::space::rebuild(&self.alerts.items, &self.wallet.ledger);
    }
}

impl eframe::App for App {
    /// One frame.
    ///
    /// eframe 0.36 hands the application a [`egui::Ui`] covering the whole viewport rather than a
    /// bare context, so panels are shown *inside* that ui. The consequence worth knowing is that the
    /// order of the calls below is the layout: the tab strip claims the top of the viewport first,
    /// the banner sits beneath it, and the central panel then fills whatever is left. Reversing
    /// them would leave the strip floating over the screen it is supposed to sit above.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();

        // Cloned because the closures below borrow `self` mutably while a `&Ui` borrow would still be
        // alive; an `egui::Context` is a handle to shared state, so a clone is a refcount bump and
        // points at the same context, not a copy of it.
        let ctx = ui.ctx().clone();
        let delta = ctx.input(|i| i.stable_dt).min(0.25);
        if self.age_toasts(delta) {
            // The only unconditional repaint request in the program, and it is scoped to the one thing
            // that animates without input.
            ctx.request_repaint();
        }

        // Capture the server at the start of the frame so we can detect an in-frame change below.
        let server_before = self.auth.server.clone();

        let colors = palette(self.theme);
        // Screens are handed a context and a command buffer; nothing below this point can reach the
        // worker directly.
        let mut navigate: Option<Screen> = None;
        // A theme change requested by the banner's toggle or by a screen, applied after the frame.
        let mut theme_choice: Option<Theme> = None;
        let screen = self.screen;
        let signed_in = screen == Screen::Chat && self.account.is_some();
        // A conversation opened anywhere inside this frame — a list row, a search hit, a joined
        // room — lands as a chat tab; the value is compared after the panes have drawn.
        let selected_before = self.chat.selected;
        if signed_in {
            // The signed-in shell is the reference's: a tab strip, the orange banner, then the
            // active tab's surface in every pixel the window can spare. The strip claims the top
            // first, the banner sits beneath it, and the central panel fills the rest.
            self.tab_strip(ui);
            self.banner(ui, &mut theme_choice);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(colors.surface))
            .show(ui, |ui| {
                if signed_in {
                    let mut open_place = None;
                    let mut context = Context {
                        theme: self.theme,
                        connection: &self.connection,
                        account: self.account.as_ref(),
                        server: &self.auth.server,
                        commands: &mut self.commands,
                        navigate: &mut navigate,
                        theme_choice: &mut theme_choice,
                        open_place: &mut open_place,
                    };
                    if self.active_chat.is_some() {
                        // A chat tab: the thread is the whole pane, with nothing beside it.
                        crate::ui::chat::thread(ui, &mut context, &mut self.chat);
                    } else {
                        match self.place {
                            Place::Chat => crate::ui::chat::list(ui, &mut context, &mut self.chat),
                            Place::Rooms => crate::ui::rooms::show(
                                ui,
                                &mut context,
                                &mut self.rooms,
                                &mut self.chat,
                            ),
                            Place::Feed => {
                                let activity = std::mem::take(&mut self.activity);
                                crate::ui::space::show(
                                    ui,
                                    &mut context,
                                    &mut self.space,
                                    &activity,
                                );
                                self.activity = activity;
                            }
                            Place::Games => crate::ui::games::show(ui, &context),
                            Place::Friends => {
                                crate::ui::friends::show(ui, &mut context, &mut self.friends)
                            }
                            Place::Alerts => {
                                crate::ui::alerts::show(ui, &mut context, &mut self.alerts)
                            }
                            Place::Search => crate::ui::search::show(
                                ui,
                                &mut context,
                                &mut self.search,
                                &mut self.chat,
                            ),
                            Place::Wallet => {
                                crate::ui::wallet::show(ui, &mut context, &mut self.wallet)
                            }
                            Place::Settings => crate::ui::settings::show(
                                ui,
                                &mut context,
                                &mut self.settings_panel,
                            ),
                        }
                    }
                    if let Some(place) = open_place {
                        self.place = place;
                        if place.is_panel() && !self.open_panels.contains(&place) {
                            self.open_panels.push(place);
                        }
                        self.entered_place(place);
                    }
                    // Whatever opened a conversation this frame opened a tab: the chip lands on
                    // the strip and the thread becomes the active surface.
                    if self.chat.selected != selected_before {
                        if let Some(conversation_id) = self.chat.selected {
                            self.activate_chat(conversation_id);
                        }
                    }
                } else {
                    // The server is cloned for the context because the auth screen holds the
                    // endpoint mutably (its form edits it) and the context must not — one small
                    // struct per frame on the one screen that has a form, rather than reworking
                    // the context the other three panes share.
                    let server = self.auth.server.clone();
                    let mut open_place = None;
                    let mut context = Context {
                        theme: self.theme,
                        connection: &self.connection,
                        account: self.account.as_ref(),
                        server: &server,
                        commands: &mut self.commands,
                        navigate: &mut navigate,
                        theme_choice: &mut theme_choice,
                        open_place: &mut open_place,
                    };
                    crate::ui::auth::show(ui, &mut context, &mut self.auth, screen);
                }
            });

        widgets::toasts(&ctx, self.theme, &self.toasts);

        if let Some(target) = navigate {
            self.screen = target;
        }

        // A screen or the top bar asked for the other theme. Applied here rather than at the
        // click so the whole frame is drawn in one palette — flipping mid-frame would show a
        // button drawn in the old colours sitting on a panel in the new ones. The choice is
        // written back to the settings file, so it outlives the window.
        if let Some(theme) = theme_choice {
            self.theme = theme;
            theme::install(&ctx, theme);
            self.settings.theme = Some(theme);
            self.persist_settings();
        }

        // The server disclosure commits a new value to `auth.server` only on a successful
        // "Use this server" click, so detecting the change here is the right moment to persist.
        if self.auth.server != server_before {
            self.settings.server = self.auth.server.clone();
            self.persist_settings();
        }

        for command in self.commands.drain(..) {
            self.net.send(command);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // `Net` shuts the worker down and joins it on drop, so this is only about ordering: the worker
        // must finish any vault write before the process is allowed to leave.
        self.net.send(Command::Shutdown);
    }
}

/// Folds new display names into the cache, keeping existing entries.
///
/// A profile response may omit someone the cache already knows, and replacing the map wholesale would
/// blank a title that was already correct.
fn merge_names(
    cache: &mut HashMap<migo_core::Id, String>,
    incoming: HashMap<migo_core::Id, String>,
) {
    for (id, name) in incoming {
        if !name.is_empty() {
            cache.insert(id, name);
        }
    }
}
