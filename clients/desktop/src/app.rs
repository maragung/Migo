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
    /// Which signed-in pane the top navigation bar has selected.
    place: Place,
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
            place: Place::Home,
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
                    self.place = Place::Home;
                    self.friends = FriendsState::default();
                    self.settings_panel = SettingsState::default();
                    self.rooms = RoomsState::default();
                    self.space = SpaceState::default();
                    self.alerts = AlertsState::default();
                    self.search = SearchState::default();
                    self.wallet = WalletState::default();
                    self.activity.clear();
                    self.screen = Screen::Chat;
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
                    self.place = Place::Home;
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
                    title,
                } => {
                    self.toasts.push(Toast::success(format!("Joined {title}")));
                    self.open_conversation(conversation_id);
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

    /// The top bar: brand on the left, the signed-in places beside it, connection and theme on
    /// the right.
    ///
    /// The navigation lives here rather than inside any pane because it belongs to none of
    /// them — it is the only widget that outlives a place switch, and letting a pane draw its
    /// own switcher is how the "which pane am I in" state ends up maintained twice.
    fn top_bar(&mut self, ui: &mut egui::Ui, signed_in: bool, theme_choice: &mut Option<Theme>) {
        let colors = palette(self.theme);
        ui.horizontal(|ui| {
            ui.add_space(space::MD);
            widgets::brand_mark(ui, self.theme);
            ui.label(
                egui::RichText::new("Migo")
                    .text_style(crate::theme::named(crate::theme::text_style::TITLE))
                    .color(colors.text)
                    .strong(),
            );

            if signed_in {
                ui.separator();
                self.nav_tabs(ui);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(space::MD);
                // Sun while dark, moon while light: the glyph names the theme one click would
                // arrive at. 🌙 (`U+1F319`) rather than ☾ (`U+263D`) because only the emoji
                // crescent is carried by any face in the font stack.
                if ui
                    .button(if self.theme.is_dark() {
                        "\u{2600}"
                    } else {
                        "\u{1F319}"
                    })
                    .on_hover_text(format!("Switch to {}", self.theme.flipped().label()))
                    .clicked()
                {
                    *theme_choice = Some(self.theme.flipped());
                }
                ui.add_space(space::MD);
                let color = match &self.connection {
                    Connection::Online => colors.positive,
                    Connection::Connecting => colors.warning,
                    Connection::Offline => colors.text_muted,
                    Connection::Failed(_) => colors.danger,
                };
                let label = self.connection.label();
                ui.horizontal(|ui| widgets::status_dot(ui, self.theme, color, label));
            });
        });
    }

    /// The signed-in places, laid out horizontally in the top bar.
    ///
    /// Every place in [`Place::ALL`], in information-architecture order — the same list the web
    /// client's rail and the Android client's bottom bar carry, because it is one product. A
    /// place whose facts are the server's re-reads on entry, for the same reason the friends
    /// graph does: the other devices of this account act on it too.
    fn nav_tabs(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        // Summed before the buttons, because the chat pane the badge describes is not the pane
        // being drawn when the tab is clicked from elsewhere.
        let unread: u32 = self.chat.conversations.iter().map(|c| c.unread).sum();
        let mut place = self.place;
        let mut entered: Option<Place> = None;
        for candidate in Place::ALL {
            let badge = if candidate == Place::Chat { unread } else { 0 };
            if widgets::tab_button(ui, theme, candidate.label(), place == candidate, badge) {
                place = candidate;
                entered = Some(candidate);
            }
        }
        self.place = place;
        if let Some(target) = entered {
            // The per-place first reads. A place that keeps what it holds does not re-read, so a
            // tour through the bar costs one read per place, not one per visit.
            match target {
                Place::Friends => self.commands.push(Command::Friends),
                Place::Rooms => self.commands.push(Command::Rooms {
                    query: self.rooms.query.clone(),
                }),
                Place::Alerts | Place::Space => self.commands.push(Command::Notifications),
                Place::Wallet => self.commands.push(Command::Wallet),
                Place::Search => {
                    if self.search.suggestions.is_empty() {
                        self.commands.push(Command::Suggestions);
                    }
                }
                Place::Home => {
                    if self.search.suggestions.is_empty() {
                        self.commands.push(Command::Suggestions);
                    }
                    if self.rooms.rooms.is_empty() {
                        self.commands.push(Command::Rooms {
                            query: String::new(),
                        });
                    }
                    if self.alerts.items.is_empty() {
                        self.commands.push(Command::Notifications);
                    }
                    if self.wallet.coins.is_none() {
                        self.commands.push(Command::Wallet);
                    }
                }
                Place::Chat | Place::Settings => {}
            }
        }
    }

    /// Opens a conversation from outside the chat pane — a Home row, a joined room, a search hit.
    ///
    /// The same [`crate::ui::chat::open`] the sidebar uses, driven with a scratch command buffer
    /// because the event that calls this arrives outside the frame that owns the real one.
    fn open_conversation(&mut self, conversation_id: migo_core::Id) {
        let mut commands = std::mem::take(&mut self.commands);
        let mut navigate = None;
        let mut theme_choice = None;
        let mut open_place = Some(Place::Chat);
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
        self.place = Place::Chat;
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
    /// order of the calls below is the layout: the top bar claims the top of the viewport first,
    /// and the central panel then fills whatever is left. Reversing them would leave the bar
    /// floating over the screen it is supposed to sit above.
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
        // A theme change requested by the top bar's toggle or by a screen, applied after the frame.
        let mut theme_choice: Option<Theme> = None;
        let screen = self.screen;
        let signed_in = screen == Screen::Chat && self.account.is_some();
        egui::Panel::top("nav")
            .exact_size(44.0)
            .frame(egui::Frame::new().fill(colors.surface_raised))
            .show(ui, |ui| self.top_bar(ui, signed_in, &mut theme_choice));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(colors.surface))
            .show(ui, |ui| {
                if signed_in {
                    // The pane fills the whole central panel. The navigation that once claimed a
                    // fixed strip on the left lives in the top bar now, so the thing being
                    // switched to gets every pixel the window can spare.
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
                    match self.place {
                        Place::Home => {
                            let data = crate::ui::home::HomeData {
                                rooms: &self.rooms.rooms,
                                people: &self.search.suggestions,
                                alerts: &self.alerts.items,
                                leaders: &self.wallet.leaders,
                                coins: self.wallet.coins,
                            };
                            crate::ui::home::show(ui, &mut context, &mut self.chat, data);
                        }
                        Place::Chat => crate::ui::chat::show(ui, &mut context, &mut self.chat),
                        Place::Rooms => crate::ui::rooms::show(ui, &mut context, &mut self.rooms),
                        Place::Space => {
                            let activity = std::mem::take(&mut self.activity);
                            crate::ui::space::show(ui, &mut context, &mut self.space, &activity);
                            self.activity = activity;
                        }
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
                        Place::Settings => {
                            crate::ui::settings::show(ui, &mut context, &mut self.settings_panel)
                        }
                    }
                    if let Some(place) = open_place {
                        self.place = place;
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
