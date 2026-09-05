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
//! battery, but this application has three things that change without input. Arriving events are
//! handled by the worker calling `request_repaint` on the context it was given. Toast fade-out is
//! handled here, by requesting a repaint only while a toast is on screen. And the signed-in taskbar
//! carries a clock and a session timer, so a signed-in frame asks for one repaint a second — coarse
//! enough to be cheap, fine enough that the minute never reads stale.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::config::ServerEndpoint;
use crate::model::{Account, Connection, Toast, ToastKind};
use crate::net::{Command, Event, Net};
use crate::settings::{self, Settings};
use crate::theme::{self, font, palette, radius, space, Theme};
use crate::ui::alerts::AlertsState;
use crate::ui::auth::AuthState;
use crate::ui::chat::{ChatState, RoomNotice, MAX_ROOM_NOTICES};
use crate::ui::desktop::{self, Desktop, TaskAction, TaskEntry};
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
    /// The signed-in shell's own state: which windows are open, which tab the Contacts window is
    /// on, and when the session started. Everything else a window manager remembers — position,
    /// size, stacking — is egui's, keyed by the window ids [`crate::ui::desktop`] mints.
    desktop: Desktop,
    connection: Connection,
    account: Option<Account>,
    auth: AuthState,
    chat: ChatState,
    friends: FriendsState,
    settings_panel: SettingsState,
    profile_panel: crate::ui::profile::ProfileState,
    admins_panel: crate::ui::admins::AdminsState,
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
        // The saved interface scale, the same way. Applied before the first frame so the first
        // thing a person sees is the size they chose, not a flash of the default snapping to it.
        cc.egui_ctx.set_zoom_factor(settings.zoom());

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
            desktop: Desktop::default(),
            connection: Connection::Offline,
            account: None,
            auth,
            chat: ChatState::default(),
            friends: FriendsState::default(),
            settings_panel: SettingsState::default(),
            profile_panel: crate::ui::profile::ProfileState::default(),
            admins_panel: crate::ui::admins::AdminsState::default(),
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
                    // A session starts with a fresh desktop, and with none of the previous
                    // session's graph or device list: those describe an account, and this may be
                    // a different one signing in over the same window.
                    self.desktop = Desktop::new_session();
                    self.friends = FriendsState::default();
                    self.settings_panel = SettingsState::default();
                    self.profile_panel = crate::ui::profile::ProfileState::default();
                    self.admins_panel = crate::ui::admins::AdminsState::default();
                    self.rooms = RoomsState::default();
                    self.space = SpaceState::default();
                    self.alerts = AlertsState::default();
                    self.search = SearchState::default();
                    self.wallet = WalletState::default();
                    self.activity.clear();
                    self.screen = Screen::Chat;
                    // The taskbar carries the balance, so the session's first reads include the
                    // wallet the same way they include the conversation list.
                    self.commands.push(Command::Wallet);
                    // The account menu's owner gate: one whoami read per session, so the admins
                    // window's existence is offered only to the account the deployment names.
                    // The whoami never fails on standing, so the non-owner's answer is a
                    // quiet `Closed` — a fact, not a refusal to catch.
                    self.commands.push(Command::Admins);
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
                    self.profile_panel = crate::ui::profile::ProfileState::default();
                    self.admins_panel = crate::ui::admins::AdminsState::default();
                    self.rooms = RoomsState::default();
                    self.space = SpaceState::default();
                    self.alerts = AlertsState::default();
                    self.search = SearchState::default();
                    self.wallet = WalletState::default();
                    self.activity.clear();
                    // The desktop goes with the session too: every window on it was the
                    // account's, and the logout dialog is moot once the logout has happened.
                    self.desktop = Desktop::default();
                    self.screen = Screen::Unlock;
                }
                Event::CaptchaChallenge(challenge) => self.auth.captcha.hold(challenge),
                Event::CaptchaUnavailable { reason } => self.auth.captcha.unavailable(reason),
                Event::CaptchaRefused => self.auth.captcha.refused(),
                Event::Conversations(list) => self.chat.set_conversations(list),
                // A conversation this client asked for: open it as a window, the one way there
                // is. The list refresh that follows will fill its title in; the window does not
                // wait for it.
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
                        // Only count it unread when the conversation has no window on the
                        // desktop. A badge on a window someone is reading is noise; a window
                        // that is open but buried under another still counts as being read,
                        // because the person chose to keep it open.
                        if incoming && !self.desktop.chats.contains(&conversation_id) {
                            conversation.unread = conversation.unread.saturating_add(1);
                        }
                        // A message for a conversation with no window mints the window — the
                        // user's rule for chat windows: a room, group, or private chat's window
                        // comes into being when a packet arrives for it, not only when someone
                        // clicks. The mint does not steal the top spot: the taskbar button's
                        // unread badge is the attention signal, and the person's click still
                        // raises.
                        if incoming {
                            self.desktop.open_chat(conversation_id);
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
                Event::Receipt {
                    conversation_id,
                    user_id,
                    kind: migo_protocol::ReceiptKind::Read,
                    seq,
                } => {
                    // Who read is kept out of the marker for now — the tick pair says "read", and
                    // naming the reader in a two-person direct chat is redundant while in a room
                    // it would want a name lookup this handler has no room to run. The watermark
                    // is the fact that changes the screen.
                    let _ = user_id;
                    self.chat.note_read(conversation_id, seq);
                }
                // A Delivered (or unrecognised) watermark. Delivered is a server-side fact about
                // transport, not a reader: it advances nothing the user can see here, and an
                // unknown kind is a newer server's vocabulary — the same forward-compatibility
                // rule the opcode router applies.
                Event::Receipt { .. } => {}
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
                Event::OwnProfile(result) => {
                    // The fetch's own answer, arriving as either a card or the reason there is
                    // none. Filed rather than toasted: the pane is on screen when it asks, so the
                    // sentence belongs beside the form that caused it.
                    match result {
                        Ok(profile) => {
                            self.profile_panel.file(profile);
                        }
                        Err(reason) => {
                            self.profile_panel.fail(reason);
                        }
                    }
                }
                Event::ProfileSaved(profile) => {
                    self.profile_panel.file(profile);
                }
                Event::AvatarChangeFailed { reason } => {
                    // The avatar button's own refusal: filed beside the form the person is
                    // looking at, the same sentence-shape a refused profile save takes.
                    self.profile_panel.fail(reason);
                }
                Event::Admins(answer) => {
                    // The standing-and-list answer. The account menu's gate reads it too: an
                    // answer that says the account holds neither role keeps the menu entry
                    // hidden, and one that arrives after the entry was opened files the
                    // sentence the pane draws.
                    self.admins_panel.answer = answer;
                    self.admins_panel.settled();
                }
                Event::AdminChangeFailed { reason } => {
                    self.admins_panel.fail(reason);
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
                    // The one wire moment that names both halves: the conversation learns which
                    // room it is, so the notice tail and the live counts can find it.
                    if let Some(conversation) = self
                        .chat
                        .conversations
                        .iter_mut()
                        .find(|c| c.conversation_id == conversation_id)
                    {
                        conversation.room_id = Some(room_id);
                    }
                    self.toasts.push(Toast::success(format!("Joined {title}")));
                    self.open_conversation(conversation_id);
                }
                Event::RoomLeft { room_id } => {
                    self.rooms.joined.remove(&room_id);
                    self.toasts.push(Toast::info("Left the room"));
                    // The conversation is closed server-side; its notice tail goes with it.
                    self.chat.room_notices.remove(&room_id);
                }
                // A membership change in a watched room: the notice line lands in the room's
                // thread, and the member total — when the event carries it — folds into the
                // rooms pane's live count, so the directory row moves with the room.
                Event::RoomMember {
                    room_id,
                    user_id,
                    change,
                    member_count,
                } => {
                    let verb = notice_verb(change);
                    let tail = self.chat.room_notices.entry(room_id).or_default();
                    let seq = tail.last().map(|n| n.seq + 1).unwrap_or(0);
                    tail.push(RoomNotice { user_id, verb, seq });
                    if tail.len() > MAX_ROOM_NOTICES {
                        let cut = tail.len() - MAX_ROOM_NOTICES;
                        tail.drain(0..cut);
                    }
                    let live = self.rooms.live.entry(room_id).or_default();
                    live.member_count = member_count;
                }
                // A watched room's counters moved: fold the deltas onto the live record, absent
                // meaning unchanged, and reset the tail when the room goes.
                Event::RoomState {
                    room_id,
                    online_count,
                    member_count,
                } => {
                    let live = self.rooms.live.entry(room_id).or_default();
                    if online_count.is_some() {
                        live.online_count = online_count;
                    }
                    if member_count.is_some() {
                        live.member_count = member_count;
                    }
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

    /// The taskbar's window list, in the order the windows were opened: the Contacts window,
    /// then the conversation windows, then the side windows.
    ///
    /// Read-only on purpose — the list is a view of [`Desktop`], and building it from anything
    /// else would give the taskbar a second opinion about what is open.
    fn task_entries(&self) -> Vec<TaskEntry> {
        let me = self.account.as_ref().map(|a| a.account_id);
        let mut entries = Vec::new();
        if self.desktop.contacts_open {
            entries.push(TaskEntry {
                id: desktop::contacts_id(),
                label: "Contacts".to_owned(),
                kind: "",
                unread: 0,
            });
        }
        for conversation_id in &self.desktop.chats {
            let (label, kind, unread) = self
                .chat
                .conversations
                .iter()
                .find(|c| c.conversation_id == *conversation_id)
                .map(|c| {
                    let title = me
                        .map(|me| c.display_title(me, &self.chat.names))
                        .unwrap_or_else(|| crate::model::short_id(*conversation_id));
                    // The kind word is the taskbar's own taxonomy: a room is what the server
                    // names a room, a group is a conversation with more than two members, and
                    // everything else is a private chat.
                    let kind = if c.room_id.is_some() {
                        "Room"
                    } else if c.members.len() > 2 {
                        "Group"
                    } else {
                        "Chat"
                    };
                    (title, kind, c.unread)
                })
                .unwrap_or_else(|| (crate::model::short_id(*conversation_id), "Chat", 0));
            entries.push(TaskEntry {
                id: desktop::chat_id(*conversation_id),
                label,
                kind,
                unread,
            });
        }
        for place in &self.desktop.sides {
            entries.push(TaskEntry {
                id: desktop::side_id(*place),
                label: place.right_label().to_owned(),
                kind: "",
                unread: 0,
            });
        }
        entries
    }

    /// The Contacts window: the account bar, the three tabs, and the tab's content — the
    /// reference's left panel, promoted from a pane to a window of its own.
    ///
    /// The window's close button is allowed to close it, because the taskbar's Migo button is
    /// the way back; a contacts list that cannot be got out of the way is not a window, it is a
    /// wall.
    fn contacts_window(
        &mut self,
        ctx: &egui::Context,
        navigate: &mut Option<Screen>,
        theme_choice: &mut Option<Theme>,
        zoom_choice: &mut Option<f32>,
    ) {
        let colors = palette(self.theme);
        let mut open = self.desktop.contacts_open;
        let tab = self.desktop.contacts_tab;
        let mut picked: Option<Place> = None;

        desktop::floating(
            self.theme,
            "Contacts",
            desktop::contacts_id(),
            desktop::CONTACTS_POS,
            desktop::CONTACTS_SIZE,
            egui::vec2(280.0, 360.0),
        )
        .open(&mut open)
        .show(ctx, |ui| {
            self.account_bar(ctx, ui, theme_choice);
            ui.add_space(space::XS);
            // The tab strip on the window's own nav teal — the same chip the old shell's strip
            // drew, on the same fill, so the promotion from pane to window changed where the
            // tabs live and nothing about how they read.
            egui::Frame::new()
                .fill(colors.nav)
                .corner_radius(egui::CornerRadius::same(radius::SM))
                .inner_margin(egui::Margin::same(space::XS as i8))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        for candidate in Place::CONTACTS_TABS {
                            let outcome = widgets::tab_chip(
                                ui,
                                self.theme,
                                candidate.label(),
                                Some(candidate),
                                tab == candidate,
                                false,
                            );
                            if outcome.clicked {
                                picked = Some(candidate);
                            }
                        }
                    });
                });
            widgets::divider(ui, self.theme);
            self.place_content(ui, tab, navigate, theme_choice, zoom_choice);
        });

        self.desktop.contacts_open = open;
        if let Some(target) = picked {
            self.desktop.contacts_tab = target;
            self.entered_place(target);
        }
    }

    /// The account bar the Contacts window carries where the old shell had its banner: the
    /// orange band that owns the account — avatar, name, the one live fact about the
    /// connection, the balance, the theme toggle, and the menu that opens every side window.
    fn account_bar(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        theme_choice: &mut Option<Theme>,
    ) {
        let colors = palette(self.theme);
        let username = self
            .account
            .as_ref()
            .map(|account| account.username.clone())
            .unwrap_or_default();
        let coins = self.wallet.coins;
        // The state word for the bar's dot, resolved here so the layout closure never borrows
        // the connection it is drawn beside.
        let (dot_color, dot_label) = match &self.connection {
            Connection::Online => (colors.positive, "Connected"),
            Connection::Connecting => (colors.warning, "Connecting"),
            Connection::Offline => (colors.banner_ink, "Offline"),
            Connection::Fallback(_) => (colors.accent, "Connected"),
            Connection::Failed(_) => (colors.danger, "Disconnected"),
        };
        let connection_detail = self.connection.label().to_owned();

        let mut opened: Option<Place> = None;
        let mut logout = false;
        let mut flip_theme = false;

        egui::Frame::new()
            .fill(colors.banner_b)
            .corner_radius(egui::CornerRadius::same(radius::SM))
            .inner_margin(egui::Margin::same(space::SM as i8))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    widgets::banner_avatar(ui, self.theme, &username, 32.0);
                    ui.add_space(space::SM);
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(widgets::elide(&username, 20))
                                .font(egui::FontId::proportional(font::SUBTITLE))
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
                                    .font(egui::FontId::proportional(font::TINY))
                                    .color(colors.banner_ink),
                            )
                            .on_hover_text(&connection_detail);
                        });
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Sun while dark, moon while light: the glyph names the theme one click
                        // would arrive at, drawn as ink on the band.
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
                        // reference's dropdown, carrying the side windows it offers.
                        let mut menu = None;
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
                                        menu = Some(Place::Profile);
                                        ui.close();
                                    }
                                    if ui.button("My Credits & TopUp").clicked() {
                                        menu = Some(Place::Wallet);
                                        ui.close();
                                    }
                                    if ui.button("Alerts").clicked() {
                                        menu = Some(Place::Alerts);
                                        ui.close();
                                    }
                                    if ui.button("Search").clicked() {
                                        menu = Some(Place::Search);
                                        ui.close();
                                    }
                                    if ui.button("Games").clicked() {
                                        menu = Some(Place::Games);
                                        ui.close();
                                    }
                                    // Settings keeps its own entry now that "My Profile" opens the
                                    // profile window: server, theme, devices, and the way out.
                                    if ui.button("Settings").clicked() {
                                        menu = Some(Place::Settings);
                                        ui.close();
                                    }
                                    // The owner's own management page. Offered only when the
                                    // sign-in standing check said this account is the owner — the
                                    // surface's existence is not public information, and the
                                    // server refuses every read and write here for anybody
                                    // else anyway. A non-owner never sees the word.
                                    if matches!(
                                        self.admins_panel.answer,
                                        crate::net::AdminsAnswer::Owner(_)
                                    ) && ui.button("Global Admins").clicked()
                                    {
                                        menu = Some(Place::Admins);
                                        ui.close();
                                    }
                                    if ui.button("Exit / Logout").clicked() {
                                        out = true;
                                        ui.close();
                                    }
                                },
                            );
                        });
                        opened = menu;
                        logout = out;
                        // The balance chip: the session's real $MIG, dark on the band.
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

        if let Some(target) = opened {
            self.open_side(ctx, target);
        }
        if logout {
            self.desktop.logout_dialog = true;
        }
        if flip_theme {
            *theme_choice = Some(self.theme.flipped());
        }
    }

    /// Opens a side window, giving the place its first reads exactly once.
    ///
    /// A place that is already open is raised rather than re-read: the window is on the desktop,
    /// so the facts it shows are the facts it asked for when it arrived, and what the menu click
    /// owes the user is the window itself, on top.
    ///
    /// A Contacts tab has no window of its own to raise — it is a tab — so asking for one here is
    /// routed to the Contacts window: the tab switches, the window comes up. Nothing in this shell
    /// asks that way today, but the guard is what keeps the two window kinds from drifting into
    /// one another as the menus grow.
    fn open_side(&mut self, ctx: &egui::Context, place: Place) {
        if !place.is_side_window() {
            self.desktop.contacts_tab = place;
            self.desktop.contacts_open = true;
            self.entered_place(place);
            desktop::focus(ctx, desktop::contacts_id());
            return;
        }
        if self.desktop.open_side(place) {
            self.entered_place(place);
        } else {
            desktop::focus(ctx, desktop::side_id(place));
        }
    }

    /// One conversation's window: the thread, the whole window.
    ///
    /// The title is resolved here rather than cached, because the names a title is made of can
    /// arrive after the window is minted — a window titled by an id's tail that never improves
    /// would read as a bug the server cannot fix.
    fn chat_window(
        &mut self,
        ctx: &egui::Context,
        index: usize,
        conversation_id: migo_core::Id,
        navigate: &mut Option<Screen>,
        theme_choice: &mut Option<Theme>,
        zoom_choice: &mut Option<f32>,
    ) {
        let me = self.account.as_ref().map(|a| a.account_id);
        let title = self
            .chat
            .conversations
            .iter()
            .find(|c| c.conversation_id == conversation_id)
            .map(|c| {
                me.map(|me| c.display_title(me, &self.chat.names))
                    .unwrap_or_else(|| crate::model::short_id(conversation_id))
            })
            .unwrap_or_else(|| crate::model::short_id(conversation_id));

        let mut open = true;
        desktop::floating(
            self.theme,
            &title,
            desktop::chat_id(conversation_id),
            Desktop::chat_cascade(index),
            desktop::CHAT_SIZE,
            egui::vec2(380.0, 320.0),
        )
        .open(&mut open)
        .show(ctx, |ui| {
            let mut context = Context {
                theme: self.theme,
                connection: &self.connection,
                account: self.account.as_ref(),
                server: &self.auth.server,
                commands: &mut self.commands,
                navigate,
                theme_choice,
                zoom_choice,
            };
            crate::ui::chat::thread(ui, &mut context, &mut self.chat, conversation_id);
        });

        // The window's own close button: closing the window closes the conversation, which is
        // the reference's whole model. The thread's history stays in the store, so reopening is
        // one click away.
        if !open {
            self.desktop.close_chat(conversation_id);
        }
    }

    /// One side window: a small floating pane opened from the account menu — profile, wallet,
    /// alerts, search, games, settings, and the owner's admins page.
    fn side_window(
        &mut self,
        ctx: &egui::Context,
        index: usize,
        place: Place,
        navigate: &mut Option<Screen>,
        theme_choice: &mut Option<Theme>,
        zoom_choice: &mut Option<f32>,
    ) {
        let mut open = true;
        desktop::floating(
            self.theme,
            place.right_label(),
            desktop::side_id(place),
            Desktop::side_cascade(index),
            desktop::SIDE_SIZE,
            egui::vec2(300.0, 240.0),
        )
        .open(&mut open)
        .show(ctx, |ui| {
            self.place_content(ui, place, navigate, theme_choice, zoom_choice);
        });

        if !open {
            self.desktop.close_side(place);
        }
    }

    /// One place's content, drawn into whatever surface is asking for it — the Contacts
    /// window's active tab, or the side window the place opened as.
    ///
    /// The same `show` functions the old two-pane shell called, unchanged: the screens were
    /// already pure functions of state, so moving them from panes into windows costs nothing
    /// and changes nothing about how they behave.
    fn place_content(
        &mut self,
        ui: &mut egui::Ui,
        place: Place,
        navigate: &mut Option<Screen>,
        theme_choice: &mut Option<Theme>,
        zoom_choice: &mut Option<f32>,
    ) {
        let mut context = Context {
            theme: self.theme,
            connection: &self.connection,
            account: self.account.as_ref(),
            server: &self.auth.server,
            commands: &mut self.commands,
            navigate,
            theme_choice,
            zoom_choice,
        };
        match place {
            Place::Friends => crate::ui::friends::show(ui, &mut context, &mut self.friends),
            Place::Rooms => {
                crate::ui::rooms::show(ui, &mut context, &mut self.rooms, &mut self.chat)
            }
            Place::Feed => {
                let activity = std::mem::take(&mut self.activity);
                crate::ui::space::show(ui, &mut context, &mut self.space, &activity);
                self.activity = activity;
            }
            Place::Alerts => crate::ui::alerts::show(ui, &mut context, &mut self.alerts),
            Place::Search => {
                crate::ui::search::show(ui, &mut context, &mut self.search, &mut self.chat)
            }
            Place::Wallet => crate::ui::wallet::show(ui, &mut context, &mut self.wallet),
            Place::Profile => crate::ui::profile::show(ui, &mut context, &mut self.profile_panel),
            Place::Admins => crate::ui::admins::show(ui, &mut context, &mut self.admins_panel),
            Place::Games => crate::ui::games::show(ui, &context),
            Place::Settings => {
                crate::ui::settings::show(ui, &mut context, &mut self.settings_panel)
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
            Place::Profile => self.commands.push(Command::OwnProfile),
            Place::Admins => self.commands.push(Command::Admins),
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
    /// real one. Opening a conversation is opening a window: it lands on the desktop and, when
    /// the frame is running, is raised to the top.
    fn open_conversation(&mut self, conversation_id: migo_core::Id) {
        let mut commands = std::mem::take(&mut self.commands);
        let mut navigate = None;
        let mut theme_choice = None;
        let mut zoom_choice = None;
        let server = self.auth.server.clone();
        let mut context = Context {
            theme: self.theme,
            connection: &self.connection,
            account: self.account.as_ref(),
            server: &server,
            commands: &mut commands,
            navigate: &mut navigate,
            theme_choice: &mut theme_choice,
            zoom_choice: &mut zoom_choice,
        };
        crate::ui::chat::open(&mut context, &mut self.chat, conversation_id);
        self.commands = commands;
        self.desktop.open_chat(conversation_id);
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
    /// bare context, so panels and windows are shown *inside* that ui. The signed-in frame is the
    /// reference's desktop-OS shell in three passes, and the order is the layout: the taskbar
    /// claims the bottom edge, the central panel paints the desktop surface (or the auth screen,
    /// before there is a session to desktop), and then the floating windows are shown against the
    /// context — above the panels, because every [`egui::Window`] lives on a layer above them.
    ///
    /// The windows are drawn before the frame's two key reads, so both see the frame they belong
    /// to: the open-diff (a conversation opened by any door this frame becomes its window, raised)
    /// and the Escape key (which closes the conversation window that is on top, and nothing else).
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();

        // Cloned because the closures below borrow `self` mutably while a `&Ui` borrow would still be
        // alive; an `egui::Context` is a handle to shared state, so a clone is a refcount bump and
        // points at the same context, not a copy of it.
        let ctx = ui.ctx().clone();
        let delta = ctx.input(|i| i.stable_dt).min(0.25);
        if self.age_toasts(delta) {
            // The one unconditional repaint request in the program, and it is scoped to the one thing
            // that animates without input.
            ctx.request_repaint();
        }

        // Capture the server at the start of the frame so we can detect an in-frame change below.
        let server_before = self.auth.server.clone();

        let colors = palette(self.theme);
        // Screens are handed a context and a command buffer; nothing below this point can reach the
        // worker directly.
        let mut navigate: Option<Screen> = None;
        // A theme change requested by the account bar's toggle or by a screen, applied after the frame.
        let mut theme_choice: Option<Theme> = None;
        // An interface-scale change requested from the settings panel, applied after the frame.
        let mut zoom_choice: Option<f32> = None;
        let screen = self.screen;
        let signed_in = screen == Screen::Chat && self.account.is_some();

        if signed_in {
            // The taskbar first, so the desktop surface and every window know where its edge is.
            let entries = self.task_entries();
            let coins = self.wallet.coins;
            let session = self.desktop.session_start.map(|start| start.elapsed());
            let contacts_open = self.desktop.contacts_open;
            let actions = desktop::taskbar(ui, self.theme, contacts_open, &entries, coins, session);
            for action in actions {
                match action {
                    TaskAction::Toggle(id) => desktop::toggle(&ctx, id),
                    TaskAction::ShowContacts => self.desktop.contacts_open = true,
                    TaskAction::Logout => self.desktop.logout_dialog = true,
                }
            }
            // The taskbar carries a clock and a session timer, both of which move without input.
            // One repaint a second keeps the minute honest without turning the sleep into a spin.
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(if signed_in {
                colors.desktop
            } else {
                colors.surface
            }))
            .show(ui, |ui| {
                if signed_in {
                    // The desktop surface: the ground the windows float on, brand and all.
                    desktop::surface(ui);
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
                        zoom_choice: &mut zoom_choice,
                    };
                    crate::ui::auth::show(ui, &mut context, &mut self.auth, screen);
                }
            });

        if signed_in {
            // How many conversations have ever been opened, read before the windows draw: a
            // conversation can be opened from places the shell does not control (a room row, a
            // search hit — anything that calls `chat::open`), so the counter is the one honest
            // signal that a window wants to exist when this frame is done.
            let open_seq_before = self.chat.open_seq;

            self.contacts_window(&ctx, &mut navigate, &mut theme_choice, &mut zoom_choice);
            // The conversation windows are drawn in open order, which is the cascade: a window's
            // index is its birthplace, and the list is cloned because closing one rewrites it.
            let chats = self.desktop.chats.clone();
            for (index, conversation_id) in chats.iter().copied().enumerate() {
                self.chat_window(
                    &ctx,
                    index,
                    conversation_id,
                    &mut navigate,
                    &mut theme_choice,
                    &mut zoom_choice,
                );
            }
            for (index, place) in self.desktop.sides.clone().iter().copied().enumerate() {
                self.side_window(
                    &ctx,
                    index,
                    place,
                    &mut navigate,
                    &mut theme_choice,
                    &mut zoom_choice,
                );
            }

            // Whatever door opened a conversation this frame opens its window and raises it.
            if self.chat.open_seq != open_seq_before {
                if let Some(conversation_id) = self.chat.selected {
                    self.desktop.open_chat(conversation_id);
                    desktop::focus(&ctx, desktop::chat_id(conversation_id));
                }
            }

            // Escape closes the conversation window that is on top — the reference's windowing
            // reflex, and the one key this shell owns. Only a conversation window: Escape in a
            // menu belongs to the menu (any open popup keeps the key), Escape in the logout
            // dialog cancels the dialog, and a side window or the Contacts window is closed by
            // its own button, not by a key that could take the lists away by accident.
            if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                && !self.desktop.logout_dialog
                && !ctx.any_popup_open()
            {
                let top =
                    ctx.memory_mut(|memory| memory.areas_mut().top_layer_id(egui::Order::Middle));
                if let Some(layer) = top {
                    let closed = self
                        .desktop
                        .chats
                        .iter()
                        .copied()
                        .find(|id| desktop::chat_id(*id) == layer.id);
                    if let Some(conversation_id) = closed {
                        self.desktop.close_chat(conversation_id);
                    }
                }
            }

            // The logout confirmation, over everything: it gates the whole session, so it is
            // drawn last and on the foreground layer, and its answer is the frame's last word.
            if desktop::logout_dialog(&ctx, self.theme, &mut self.desktop.logout_dialog) {
                self.commands.push(Command::SignOut);
            }
        }

        widgets::toasts(&ctx, self.theme, &self.toasts);

        if let Some(target) = navigate {
            self.screen = target;
        }

        // A screen or the account bar asked for the other theme. Applied here rather than at the
        // click so the whole frame is drawn in one palette — flipping mid-frame would show a
        // button drawn in the old colours sitting on a panel in the new ones. The choice is
        // written back to the settings file, so it outlives the window.
        if let Some(theme) = theme_choice {
            self.theme = theme;
            theme::install(&ctx, theme);
            self.settings.theme = Some(theme);
            self.persist_settings();
        }

        // The settings panel's scale control. Same after-the-frame timing as the theme, and the
        // factor is rounded back to a whole percent before it is saved: the record is `Option<u8>`
        // on purpose, and a choice made from the panel's steps is already whole.
        if let Some(zoom) = zoom_choice {
            ctx.set_zoom_factor(zoom);
            self.settings.ui_scale = Some((zoom * 100.0).round() as u8);
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

/// The sentence half of a membership notice: what happened, without who.
///
/// The same map the web and Android clients draw from, so the three clients say the same thing
/// about the same event. `Unknown` never reaches here — the net layer already collapsed it onto
/// the `joined` flag — but the match stays total so a newer enum value cannot compile its way
/// into silence.
fn notice_verb(change: migo_protocol::MemberChange) -> &'static str {
    match change {
        migo_protocol::MemberChange::Joined => "joined the room",
        migo_protocol::MemberChange::Left => "left",
        migo_protocol::MemberChange::Disconnected => "disconnected",
        migo_protocol::MemberChange::Reconnected => "came back",
        migo_protocol::MemberChange::Kicked => "was kicked",
        migo_protocol::MemberChange::Banned => "was banned",
        migo_protocol::MemberChange::Unknown => "left",
    }
}
