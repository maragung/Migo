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
use crate::ui::wallet::{TrackingTx, WalletState};
use crate::ui::{widgets, Context, Place, Screen};

/// The whole application state.
pub struct App {
    theme: Theme,
    net: Net,
    screen: Screen,
    /// Which system tab the left panel is showing — the left pane's own state, never disturbed
    /// by what the right pane does.
    place: Place,
    /// The panel the right pane is showing, when it is showing one — the pane's own state, never
    /// disturbed by what the left panel does. None is the pane's resting state, which is the
    /// honest default: the system tabs' content lives on the left, so an empty pane owes the lists
    /// a quiet neighbour, not a second copy of the feed.
    right_panel: Option<Place>,
    /// The open conversation tabs, in open order — the right pane's closable chat chips.
    open_chats: Vec<migo_core::Id>,
    /// The conversation whose thread is showing, when a chat tab is the right pane's active one.
    active_chat: Option<migo_core::Id>,
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
            place: Place::Friends,
            right_panel: None,
            open_chats: Vec::new(),
            active_chat: None,
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
                    self.place = Place::Friends;
                    self.right_panel = None;
                    self.open_chats.clear();
                    self.active_chat = None;
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
                    self.place = Place::Friends;
                    self.right_panel = None;
                    self.open_chats.clear();
                    self.active_chat = None;
                    self.screen = Screen::Unlock;
                }
                Event::CaptchaChallenge(challenge) => self.auth.captcha.hold(challenge),
                Event::CaptchaUnavailable { reason } => self.auth.captcha.unavailable(reason),
                Event::CaptchaRefused => self.auth.captcha.refused(),
                Event::Conversations(list) => self.chat.set_conversations(list),
                // A conversation this client asked for: open it as a tab, the one way there is.
                // The list refresh that follows will fill its row in; the tab does not wait for it.
                Event::ConversationCreated { conversation_id } => {
                    self.open_conversation(conversation_id);
                }
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
                Event::Devices(result) => {
                    self.settings_panel.devices = crate::ui::settings::Fetch::from_result(result);
                }
                Event::Wallets(result) => {
                    self.settings_panel.wallets = crate::ui::settings::Fetch::from_result(result);
                }
                Event::Rooms(rows) => {
                    // The wire answers both the Rooms pane and Search's room query with this one
                    // event (the request carries the query; the answer does not), so the answer
                    // lands in both homes. The query gate keeps a plain pane refresh from
                    // overwriting a stale answer into a search that is no longer running.
                    if !self.search.query.trim().is_empty() {
                        self.search.rooms = Some(rows.clone());
                        self.search.busy = false;
                    }
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
                Event::ChainBalance {
                    network,
                    address,
                    balance,
                } => {
                    // A balance names the network it came from; a stale panel of another
                    // network is not updated by it.
                    if self.wallet.chain.network == network {
                        self.wallet.chain.address = address;
                        match balance {
                            Ok(wei) => {
                                self.wallet.chain.balance = Some(wei);
                                self.wallet.chain.error = None;
                            }
                            Err(error) => {
                                self.wallet.chain.error = Some(error);
                            }
                        }
                    }
                }
                Event::ChainPrepared(result) => match result {
                    Ok(tx) => {
                        self.wallet.chain.prepared = Some(tx);
                        self.wallet.chain.prepare_error = None;
                    }
                    Err(error) => {
                        self.wallet.chain.prepare_error = Some(error);
                    }
                },
                Event::ChainSent(result) => match result {
                    Ok(tx_hash) => {
                        // Acceptance is not confirmation: the send window is done, and the
                        // honest part — watching where the transaction actually ends — begins.
                        self.wallet.chain.tracking = Some(TrackingTx {
                            tx_hash,
                            state: "BROADCAST".to_owned(),
                        });
                        self.wallet.chain.prepared = None;
                        self.wallet.chain.prepare_error = None;
                        self.wallet.chain.send_error = None;
                        self.wallet.chain.recipient.clear();
                        self.wallet.chain.amount.clear();
                        self.wallet.chain.mainnet_acknowledged = false;
                        self.wallet.chain.send_open = false;
                    }
                    Err(error) => {
                        self.wallet.chain.send_error = Some(error);
                    }
                },
                Event::ChainState { tx_hash, state } => {
                    if let Some(tracking) = self.wallet.chain.tracking.as_mut() {
                        if tracking.tx_hash == tx_hash {
                            tracking.state = state;
                        }
                    }
                }
                Event::ChainSettled { tx_hash, outcome } => {
                    if self
                        .wallet
                        .chain
                        .tracking
                        .as_ref()
                        .is_some_and(|tracking| tracking.tx_hash == tx_hash)
                    {
                        self.wallet.chain.tracking = None;
                        // Each ending says itself, including the unresolved one.
                        let toast = match outcome.as_str() {
                            "CONFIRMED" => Toast::success(format!(
                                "AVAX send confirmed · {}",
                                &tx_hash[..16.min(tx_hash.len())]
                            )),
                            "EXPIRED" => Toast::info(format!(
                                "AVAX send expired without an answer · {}",
                                &tx_hash[..16.min(tx_hash.len())]
                            )),
                            other => Toast::error(format!(
                                "AVAX send {} · {}",
                                other.to_lowercase(),
                                &tx_hash[..16.min(tx_hash.len())]
                            )),
                        };
                        self.toasts.push(toast);
                    }
                }
                Event::ChainActivity(rows) => {
                    self.wallet.chain.activity = rows;
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

    /// The left panel's navigation strip: the five system tabs, and nothing else.
    ///
    /// In the new-ui-02 model the strip belongs to the left panel alone — the conversation chips
    /// moved to the right pane's own bar (see [`App::chat_bar`]), where a thread actually opens —
    /// so the strip never stands down for a chat and the left panel's state is its own. It
    /// scrolls horizontally rather than hiding anything, because a tab that is off-screen is
    /// still a tab: hiding it would strand the surface it names behind no control at all.
    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let colors = palette(self.theme);
        // Resolved before the panel closure so the chip loop never borrows the place it
        // mutates through the click handlers.
        let place = self.place;

        let mut selected: Option<Place> = None;

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
                                let outcome = widgets::tab_chip(
                                    ui,
                                    self.theme,
                                    candidate.label(),
                                    Some(candidate),
                                    place == candidate,
                                    false,
                                );
                                if outcome.clicked {
                                    selected = Some(candidate);
                                }
                            }
                        });
                    });
            });

        if let Some(target) = selected {
            self.select_place(target);
        }
    }

    /// The right pane's chat bar: the reference's slate strip — the cyan "‹ Panels" control that
    /// hands the pane back from the thread to whatever is beneath it (an open panel, or the pane's
    /// resting state), and one closable chip per open conversation.
    fn chat_bar(&mut self, ui: &mut egui::Ui) {
        // The reference's slate-800, worn in either theme: the bar is chrome, not surface, so it
        // does not follow the palette's surfaces the way a panel does.
        const BAR: egui::Color32 = egui::Color32::from_rgb(0x1e, 0x29, 0x3b);
        let colors = palette(self.theme);
        // Resolved before the panel closure so the chip loop never borrows the chat state it
        // mutates through the click handlers.
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
        let active_chat = self.active_chat;

        let mut back = false;
        let mut chat_pick: Option<migo_core::Id> = None;
        let mut chat_close: Option<migo_core::Id> = None;

        egui::Panel::top("chat-bar")
            .exact_size(38.0)
            .frame(egui::Frame::new().fill(BAR))
            .show(ui, |ui| {
                ui.add_space(space::XS);
                ui.horizontal_centered(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("\u{2039} Panels")
                                    .font(egui::FontId::proportional(crate::theme::font::SMALL))
                                    .color(colors.banner_ink)
                                    .strong(),
                            )
                            .fill(colors.accent)
                            .corner_radius(egui::CornerRadius::same(crate::theme::radius::SM)),
                        )
                        .clicked()
                    {
                        back = true;
                    }
                    egui::ScrollArea::horizontal()
                        .id_salt("chat-tabs")
                        .auto_shrink([false, false])
                        .show_viewport(ui, |ui, _viewport| {
                            ui.horizontal_centered(|ui| {
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
                            });
                        });
                });
            });

        if back {
            // The chips stay; only the pane's mode changes, exactly as the reference composes it.
            self.active_chat = None;
        }
        if let Some(conversation_id) = chat_pick {
            self.select_chat(conversation_id);
        }
        if let Some(conversation_id) = chat_close {
            self.close_chat(conversation_id);
        }
    }

    /// The right pane's panel header: one name, one close, no chips.
    ///
    /// The pane holds a single panel at a time (the banner's account menu opens each on its own),
    /// so there is nothing to switch the name with — it is a label, not a control, and the close
    /// is the bar's only button. That is the same slim bar the web client's one-window mode
    /// draws, because it is one product.
    fn panel_header(&mut self, ui: &mut egui::Ui, panel: Place) {
        // The reference's slate-800, worn in either theme: the bar is chrome, not surface, so it
        // does not follow the palette's surfaces the way a panel does.
        const BAR: egui::Color32 = egui::Color32::from_rgb(0x1e, 0x29, 0x3b);
        let colors = palette(self.theme);

        let mut close = false;

        egui::Panel::top("panel-header")
            .exact_size(38.0)
            .frame(egui::Frame::new().fill(BAR))
            .show(ui, |ui| {
                ui.add_space(space::XS);
                ui.horizontal_centered(|ui| {
                    ui.label(
                        egui::RichText::new(format!("\u{2726} {}", panel.right_label()))
                            .font(egui::FontId::proportional(crate::theme::font::SMALL))
                            .color(colors.banner_ink)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("\u{2715} Close")
                                        .font(egui::FontId::proportional(crate::theme::font::SMALL))
                                        .color(colors.banner_ink)
                                        .strong(),
                                )
                                .fill(colors.accent)
                                .corner_radius(egui::CornerRadius::same(crate::theme::radius::SM)),
                            )
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            });

        if close {
            // Closing the only panel leaves the pane at its resting state — the same fallback
            // the web client's Home chip is.
            self.right_panel = None;
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

    /// Selects a place: the system tabs drive the left panel, the panels the right pane.
    ///
    /// The two panes are independent, so a system tab never disturbs a thread on the right, and
    /// a panel never disturbs the strip — it hands the right pane back from any thread and shows
    /// itself there.
    fn select_place(&mut self, target: Place) {
        if target.is_system_tab() {
            self.place = target;
        } else {
            self.active_chat = None;
            self.right_panel = Some(target);
        }
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
            Place::Games | Place::Settings => {}
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
        let server = self.auth.server.clone();
        let mut context = Context {
            theme: self.theme,
            connection: &self.connection,
            account: self.account.as_ref(),
            server: &server,
            commands: &mut commands,
            navigate: &mut navigate,
            theme_choice: &mut theme_choice,
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
    /// order of the calls below is the layout: the left panel claims its third of the viewport
    /// first — its tab strip at the top, the banner beneath it, the lists filling the rest — and
    /// the central panel is the right pane, its own bar over the tab it names. Reversing them
    /// would leave the strip floating over the screen it is supposed to sit beside.
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
            // The signed-in shell is the reference's split: a left panel that owns the account's
            // lists — its tab strip over the orange banner — and a right pane that runs on its
            // own state, an open conversation, an open panel, or its resting state. The left
            // panel claims its share of the window first; the central panel is the right pane
            // and fills the rest.
            //
            // The floor has to stay below the share, or the formula inverts: the old 0.32 with
            // a 300px floor meant every window under 937px drew the panel at *more* than its
            // share, growing as the window shrank. 40% with a 280px floor keeps floor ≤ share
            // down to a 700px window, which is narrower than the app is usable at.
            let avail = ui.max_rect().width();
            let width = (avail * 0.4).clamp(280.0, 540.0);
            egui::Panel::left("left-pane")
                .exact_size(width)
                .frame(
                    egui::Frame::new()
                        .fill(colors.surface)
                        .stroke(egui::Stroke::new(1.0, colors.border)),
                )
                .show(ui, |ui| {
                    self.tab_strip(ui);
                    self.banner(ui, &mut theme_choice);
                    let mut context = Context {
                        theme: self.theme,
                        connection: &self.connection,
                        account: self.account.as_ref(),
                        server: &self.auth.server,
                        commands: &mut self.commands,
                        navigate: &mut navigate,
                        theme_choice: &mut theme_choice,
                    };
                    match self.place {
                        Place::Rooms => crate::ui::rooms::show(
                            ui,
                            &mut context,
                            &mut self.rooms,
                            &mut self.chat,
                        ),
                        Place::Feed => {
                            let activity = std::mem::take(&mut self.activity);
                            crate::ui::space::show(ui, &mut context, &mut self.space, &activity);
                            self.activity = activity;
                        }
                        Place::Games => crate::ui::games::show(ui, &context),
                        Place::Friends => {
                            crate::ui::friends::show(ui, &mut context, &mut self.friends)
                        }
                        // The panels are the right pane's tabs; the strip can never land here.
                        Place::Alerts | Place::Search | Place::Wallet | Place::Settings => {}
                    }
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(colors.surface))
            .show(ui, |ui| {
                if signed_in {
                    if self.active_chat.is_some() {
                        // A chat tab: the bar carries the chips, the thread is the whole pane.
                        self.chat_bar(ui);
                        let mut context = Context {
                            theme: self.theme,
                            connection: &self.connection,
                            account: self.account.as_ref(),
                            server: &self.auth.server,
                            commands: &mut self.commands,
                            navigate: &mut navigate,
                            theme_choice: &mut theme_choice,
                        };
                        crate::ui::chat::thread(ui, &mut context, &mut self.chat);
                    } else if let Some(panel) = self.right_panel {
                        // One open panel: the slim header names it and closes it, and the panel
                        // is the whole pane. No chips, and none of the system tabs' content —
                        // those live on the left, so this pane can never draw the same list
                        // twice.
                        self.panel_header(ui, panel);
                        let mut context = Context {
                            theme: self.theme,
                            connection: &self.connection,
                            account: self.account.as_ref(),
                            server: &self.auth.server,
                            commands: &mut self.commands,
                            navigate: &mut navigate,
                            theme_choice: &mut theme_choice,
                        };
                        match panel {
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
                            // The system tabs are the left panel's; only a panel ever reaches
                            // this branch.
                            Place::Friends
                            | Place::Rooms
                            | Place::Games
                            | Place::Feed => {}
                        }
                    } else {
                        // The pane at rest: no conversation, no panel. The lists are all on the
                        // left, so the honest content is a mark and the one-line way in — the
                        // desktop's own empty state, the same resting pane the web client's Home
                        // chip shows.
                        widgets::empty_state(
                            ui,
                            self.theme,
                            "Nothing open",
                            "Pick a conversation from the lists, or open a panel from the banner's menu.",
                        );
                    }
                    // Whatever opened a conversation this frame opened a tab: the chip lands on
                    // the right pane's bar and the thread becomes the active surface.
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
                    let mut context = Context {
                        theme: self.theme,
                        connection: &self.connection,
                        account: self.account.as_ref(),
                        server: &server,
                        commands: &mut self.commands,
                        navigate: &mut navigate,
                        theme_choice: &mut theme_choice,
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
