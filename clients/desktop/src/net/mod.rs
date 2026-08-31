//! The network worker: everything asynchronous, on its own thread, behind two channels.
//!
//! # Why the UI never awaits
//!
//! egui redraws by calling one function per frame. If that function ever awaited a socket, a slow
//! server would freeze the window — not a spinner, an unresponsive process the compositor greys out.
//! So the split is absolute: this module owns the tokio runtime, the sockets, the ratchets and the
//! vault, and the UI thread owns pixels. They meet at two unbounded channels carrying plain data.
//!
//! It is not a mutex around shared state, deliberately. A mutex the paint loop has to take is a mutex
//! that can be held by whatever is doing I/O, which is the same freeze by a longer route. Ownership
//! moves with the message instead, so there is nothing to contend for.
//!
//! # What crosses the channel
//!
//! [`Command`] carries intent — "send this text", "open this conversation". [`Event`] carries facts
//! already reduced to what a person sees: a decrypted [`crate::model::Message`], a connection state,
//! a toast. Ciphertext, envelopes, ratchet state and key material never leave this module, so no UI
//! code can accidentally render or log them.
//!
//! # Reconnection
//!
//! The worker reconnects on its own with exponential backoff and jitter, because a client that gives
//! up on the first dropped packet is a client people restart by hand. Jitter matters at the other end:
//! a node restarting with ten thousand clients attached gets them back in a spread rather than as one
//! synchronised thundering herd.

pub mod gateway;
pub mod rest;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use migo_core::{Id, OsRandom, Random, Timestamp};
use migo_protocol::{
    features, BadgesReq, ClientInfo, ConversationKind, EncryptionMode, FriendRespond, FriendTarget,
    GiftCatalogueReq, GiftSend, InboxReq, LeaderboardReq, LedgerReq, MessageKind, NotificationAck,
    Opcode, ProgressionReq, RelationshipListReq, RoomCreate, RoomJoinRequest, RoomLeaveRequest,
    RoomListRequest, SearchReq, SubscribeRequest, SuggestReq, Topic, TopicKind, WalletReq,
};
use tokio::sync::mpsc;

use crate::config::ServerEndpoint;
use crate::crypto::content::{self, Content};
use crate::crypto::envelope::Envelope;
use crate::crypto::session::{DeviceKeys, SessionStore, ONE_TIME_PREKEY_COUNT};
use crate::model::{
    self, Account, AlertRow, Body, Connection, Conversation, Delivery, GiftRow, LeaderRow,
    LedgerRow, Message, PersonRow, Progression, Relationship, RelationshipKind, RoomRow,
    SessionRow, ToastKind,
};
use crate::net::gateway::{Gateway, GatewayError};
use crate::net::rest::{CaptchaChallenge, CaptchaProof, DeviceRequest, Rest, RestError};
use crate::vault::{self, SavedSession};

/// When to warn that the one-time prekey pool is running down.
///
/// A fifth of the published pool. Low enough that the warning is not noise on a busy account, high
/// enough that there is still time to act before the pool is empty.
const ONE_TIME_PREKEY_LOW_WATER: usize = ONE_TIME_PREKEY_COUNT as usize / 5;

/// The server's error symbols that mean "the captcha proof is dead, whatever else is true".
///
/// Wrong, expired, or never sent: the three differ on the wire but demand the same response from
/// a form — drop the held challenge, fetch a fresh one, keep the form standing. Matched here
/// rather than in the UI because the symbol is wire vocabulary, and events are reduced facts.
const CAPTCHA_REFUSAL_SYMBOLS: [&str; 3] =
    ["INVALID_CAPTCHA", "CAPTCHA_EXPIRED", "CAPTCHA_REQUIRED"];

/// A captcha answer on its way from a form to the worker.
///
/// Owned because everything a command carries crosses a channel; the worker lends it to
/// [`CaptchaProof`] when the request body is built. The manual `Debug` keeps the answer out of
/// any trace this command path might grow, for the same reason [`crate::net::rest::Grant`]'s
/// keeps its tokens out — and even though an answer is worth far less than a token, a log line
/// that never contains it is a log line that cannot leak it.
pub struct CaptchaAnswer {
    /// The challenge being answered, exactly as the server issued it.
    pub challenge_id: String,
    /// What the user read off the image, already normalised: upper-cased, whitespace-free.
    pub answer: String,
}

impl std::fmt::Debug for CaptchaAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptchaAnswer")
            .field("challenge_id", &self.challenge_id)
            .field("answer", &"***")
            .finish()
    }
}

/// What the UI asks the worker to do.
#[derive(Debug)]
pub enum Command {
    /// Fetch a fresh image captcha challenge for the auth forms.
    ///
    /// Separate from register and sign-in because the challenge has to be on screen *before*
    /// either form can be finished; a form asks the moment it draws with nothing held.
    FetchCaptcha {
        /// The server to ask — the one the form is pointing at, which is not necessarily the
        /// one a session lives on, because no session exists yet.
        server: ServerEndpoint,
        /// The rendering to ask for: `None` for the server's default, `Some("image_alt")` for
        /// the gentler one. A string because it is the wire's own vocabulary, spelled out once
        /// at the click that sets it.
        mode: Option<String>,
    },
    /// Create an account, generate keys, and write a new vault.
    Register {
        server: ServerEndpoint,
        username: String,
        password: String,
        passphrase: String,
        /// The answered captcha challenge, when the form held one and the answer had the shape
        /// worth sending. `None` submits without a proof.
        captcha: Option<CaptchaAnswer>,
    },
    /// Sign in to an existing account, generating keys if this device has none yet.
    SignIn {
        server: ServerEndpoint,
        identifier: String,
        password: String,
        passphrase: String,
        /// The answered captcha challenge, as on [`Command::Register`].
        captcha: Option<CaptchaAnswer>,
    },
    /// Open the existing vault and resume its saved sign-in.
    Unlock { passphrase: String },
    /// End the session and forget every key on this device.
    SignOut,
    /// Refresh the conversation list.
    Conversations,
    /// Load history for one conversation, from `have_seq` upwards.
    History { conversation_id: Id, have_seq: u64 },
    /// Encrypt and send text.
    SendText { conversation_id: Id, text: String },
    /// Start a direct conversation with one username.
    StartDirect { username: String },
    /// Start a direct conversation with an account id already held — a search hit, a suggestion.
    StartDirectById { peer: Id },
    /// Report typing state. Best effort; dropped silently when offline.
    Typing { conversation_id: Id, typing: bool },
    /// Mark everything up to `seq` as read.
    MarkRead { conversation_id: Id, seq: u64 },
    /// Refresh the social graph: friends, pending requests, and the other relationship kinds.
    ///
    /// Follows the same shape as [`Command::Conversations`] because it is the same idea — a
    /// screen asks for the list, the worker reduces the wire answer to model rows, the screen
    /// gets one event.
    Friends,
    /// Send a friend request to an account id, typed by the user.
    ///
    /// Carries the raw text rather than an [`Id`] for the same reason [`Command::StartDirect`]
    /// does: the input field is a person typing, and the worker is where "what they typed is not
    /// an id" becomes a message worth reading instead of a parse panic.
    AddFriend { user_id: String },
    /// Accept or decline a pending request. The id comes from the relationship list, so it is
    /// already parsed.
    RespondFriend { user_id: Id, accept: bool },
    /// Fetch the device/session list over REST for the settings screen.
    Sessions,
    /// End one session of the account, by the id the list reported.
    RevokeSession { session_id: Id },
    /// Refresh the public room directory, optionally narrowed by a query.
    Rooms { query: String },
    /// Join a room, whose conversation opens like any other when the join is accepted.
    JoinRoom { room_id: Id },
    /// Create a room and enter it. Creation is entry: the reply is a join handle.
    CreateRoom {
        slug: String,
        name: String,
        /// True for a managed room (server-moderated); false for a public community room.
        managed: bool,
        topic: Option<String>,
    },
    /// Leave a room; the server closes its conversation for this account.
    LeaveRoom { room_id: Id },
    /// Read the durable notification inbox.
    Notifications,
    /// Mark every notification at or before one instant read.
    AcknowledgeAlerts { through_unix_ms: i64 },
    /// Read the wallet's whole economy: balance, statement, progression, badges, leaderboard,
    /// and the gift catalogue — six reads fired together, each arriving as its own event.
    Wallet,
    /// Buy and deliver a gift; the wallet re-reads after, because the server's arithmetic is the
    /// only arithmetic worth showing.
    SendGift { sku: String, recipient: Id },
    /// Search public profiles by username prefix.
    SearchPeople { query: String },
    /// Ask the social graph for its own suggestions.
    Suggestions,
    /// Stop the worker. Sent on window close.
    Shutdown,
}

/// What the worker tells the UI.
#[derive(Debug)]
pub enum Event {
    /// The connection state changed.
    Connection(Connection),
    /// A vault exists on disk, so the first screen should offer to unlock it.
    ///
    /// Carries nothing: the account name and the server address are inside the sealed body, so there
    /// is nothing about the account this event could truthfully report before the passphrase arrives.
    VaultFound,
    /// No vault exists, so the first screen should offer to register or sign in.
    VaultMissing,
    /// Signed in. Carries the safety number, which is derived from local keys only.
    SignedIn(Account),
    /// Signed out, by request or because the server revoked the session.
    SignedOut,
    /// A captcha challenge arrived for the auth forms.
    ///
    /// Carries the wire view as-is: the picture stays base64 until the form decodes it, because
    /// the decode belongs with the texture it feeds, on the UI thread.
    CaptchaChallenge(CaptchaChallenge),
    /// A captcha challenge could not be fetched. `reason` is safe to show as-is.
    ///
    /// Separate from the connection state on purpose: a form whose challenge will not load is
    /// not a form whose sign-in failed, and reporting it as one would release a busy flag that
    /// was never set.
    CaptchaUnavailable { reason: String },
    /// The server refused a submit over the captcha: wrong, expired, or missing proof.
    ///
    /// One event for all three because they differ only in the telling. What each means to a
    /// form is identical: the challenge it holds is dead, so drop it and fetch another, and
    /// keep the form on screen for the next attempt.
    CaptchaRefused,
    /// The full conversation list.
    Conversations(Vec<Conversation>),
    /// A page of history, oldest first.
    History {
        conversation_id: Id,
        messages: Vec<Message>,
    },
    /// One live message.
    Message(Message),
    /// The server accepted an outgoing message and assigned it a sequence number.
    Accepted {
        message_id: Id,
        conversation_id: Id,
        seq: u64,
    },
    /// An outgoing message could not be sent.
    SendFailed { message_id: Id },
    /// Someone started or stopped typing.
    Typing {
        conversation_id: Id,
        user_id: Id,
        typing: bool,
    },
    /// Display names for account ids, so the UI can title a direct conversation.
    Names(HashMap<Id, String>),
    /// The social graph moved: friendships and pending requests, reduced to model rows.
    ///
    /// Followed by names and presence for the same ids wherever the server discloses them,
    /// because a friends list of bare ids is a list of strangers.
    Relationships(Vec<Relationship>),
    /// Someone acted on the social graph and this account was the audience: a request arrived,
    /// or one of ours was accepted.
    ///
    /// Carries the actor and what the state string said, as far as it said anything: `Some(true)`
    /// an acceptance, `Some(false)` a request, `None` a state this build has no name for. All
    /// three mean the graph moved — the difference is only whether there is anything true to
    /// say about it beyond "look again".
    FriendChanged { user_id: Id, accepted: Option<bool> },
    /// An account's presence changed, from a presence event or a profile fetch. `Unknown` is
    /// never carried: unobserved is the absence of an event, not one.
    PresenceChanged { user_id: Id, state: model::Presence },
    /// The device/session list for the settings screen, or the reason it could not be had.
    ///
    /// A failure is a fact the panel must keep showing — "could not check" and "no other
    /// devices" are different states and only one of them should reassure anybody — so it rides
    /// the same event rather than dying as a toast.
    Sessions(Result<Vec<SessionRow>, String>),
    /// The public room directory, reduced to rows.
    Rooms(Vec<RoomRow>),
    /// A join (or create) was accepted and its conversation is ready to open.
    RoomJoined {
        conversation_id: Id,
        room_id: Id,
        title: String,
    },
    /// A leave was accepted; the rooms pane drops the room from its joined set.
    RoomLeft { room_id: Id },
    /// The durable notification inbox, newest first.
    Alerts(Vec<AlertRow>),
    /// A notification was pushed: the cue to re-read whatever inbox-shaped surface is showing.
    AlertPushed,
    /// The caller's wallet: the MIG coin balance and the points balance.
    Balance { coins: u64, points: u64 },
    /// The wallet's statement, newest first.
    Ledger(Vec<LedgerRow>),
    /// The caller's XP progression.
    ProgressionArrived(Progression),
    /// The caller's badges, by code.
    Badges(Vec<String>),
    /// The XP leaderboard page.
    Leaderboard(Vec<LeaderRow>),
    /// The gift catalogue: SKU, name, price, category.
    Gifts(Vec<GiftRow>),
    /// Accounts found by search or offered as suggestions.
    People(Vec<PersonRow>),
    /// Something worth a line at the bottom of the window.
    Toast { text: String, kind: ToastKind },
}

/// The UI's handle on the worker.
pub struct Net {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<Event>,
    /// Kept so the worker thread is joined on drop rather than abandoned mid-write to the vault.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Net {
    /// Starts the worker thread and returns a handle to it.
    ///
    /// `ctx` is cloned into the worker so an arriving event can wake the UI. Without it egui would
    /// only notice a new message the next time something else caused a repaint — which, on an idle
    /// window, is never.
    pub fn spawn(ctx: egui::Context, vault_path: PathBuf) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std_mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("migo-net".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    // Two threads: one for the socket, one for anything that blocks briefly. The
                    // default is one per core, which for a client that holds a single connection is
                    // several megabytes of stacks doing nothing.
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = event_tx.send(Event::Toast {
                            text: "could not start the network thread".to_owned(),
                            kind: ToastKind::Error,
                        });
                        ctx.request_repaint();
                        return;
                    }
                };
                let sink = Sink {
                    events: event_tx,
                    ctx,
                };
                runtime.block_on(Worker::new(sink, vault_path).run(command_rx));
            })
            .ok();
        Self {
            commands: command_tx,
            events: event_rx,
            thread,
        }
    }

    /// Queues a command. Silently dropped once the worker has stopped, which only happens at exit.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Takes the next event, or `None` if there is nothing waiting.
    ///
    /// Never blocks: this is called from the paint loop, and a blocking read there is the freeze this
    /// whole module exists to avoid.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            // Joined rather than detached: the worker may be part-way through writing the vault, and
            // a process that exits during that write loses the identity key.
            let _ = thread.join();
        }
    }
}

/// The worker's end of the event channel, paired with the egui context to wake.
struct Sink {
    events: std_mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Sink {
    fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            self.ctx.request_repaint();
        }
    }

    fn toast(&self, text: impl Into<String>, kind: ToastKind) {
        self.send(Event::Toast {
            text: text.into(),
            kind,
        });
    }
}

/// One signed-in session's worth of state.
struct Signed {
    server: ServerEndpoint,
    rest: Rest,
    account: Account,
    access_token: String,
    sessions: SessionStore,
    /// Prekey bundles already fetched, by device id, so a conversation does not refetch per message.
    bundles: HashMap<Id, migo_crypto::x3dh::PrekeyBundle>,
    /// Which devices belong to which account, learned from KEY_BUNDLE responses.
    devices: HashMap<Id, Vec<Id>>,
    /// Members of each conversation, from the conversation list.
    members: HashMap<Id, Vec<Id>>,
    /// Account ids whose user topics this session has already subscribed to for presence.
    ///
    /// Tracked so a relationship refresh does not re-send a SUBSCRIBE for every friend on every
    /// reconnect; cleared when the gateway reconnects, because subscriptions live and die with
    /// the session that held them.
    watched: HashSet<Id>,
}

/// The reconnect schedule after a lost gateway connection.
///
/// Exponential backoff with jitter and a cap, driven from the worker's main select loop rather than
/// from inside the failure handler. That placement is the point. Sleeping inside the handler would
/// leave the command channel unserviced for the whole backoff, so a user who closes the window during
/// an outage would wait up to half a minute for the process to notice; here the sleep is one arm of a
/// select and `Shutdown` still wins. It also removes an async cycle — the handler no longer calls the
/// connect path that can call the handler.
#[derive(Debug, Clone, Copy)]
struct Retry {
    /// Attempts already made since the connection was lost.
    attempts: u32,
    /// The un-jittered delay, doubled after each failure and capped.
    backoff: Duration,
    /// What to actually wait: [`Self::backoff`] plus this round's jitter.
    wait: Duration,
}

impl Retry {
    /// Attempts before giving up and telling the user to check the address.
    const LIMIT: u32 = 8;
    /// The first pause: long enough not to hammer a server that is restarting, short enough that a
    /// handful of dropped packets never reaches the user as a visible outage.
    const BASE: Duration = Duration::from_millis(500);
    /// A long outage must not turn into half an hour of silence after it ends.
    const CAP: Duration = Duration::from_secs(30);

    /// The schedule for the first attempt after a connection is lost.
    fn first(random: &mut dyn Random) -> Self {
        Self {
            attempts: 0,
            backoff: Self::BASE,
            wait: Self::BASE + Self::jitter(random),
        }
    }

    /// The schedule after an attempt failed. Returns `None` once the limit is reached.
    fn after_failure(self, random: &mut dyn Random) -> Option<Self> {
        let attempts = self.attempts + 1;
        if attempts >= Self::LIMIT {
            return None;
        }
        let backoff = (self.backoff * 2).min(Self::CAP);
        Some(Self {
            attempts,
            backoff,
            wait: backoff + Self::jitter(random),
        })
    }

    /// Up to half a second of spread, so ten thousand clients do not return in lockstep and knock the
    /// node over again the moment it comes back.
    fn jitter(random: &mut dyn Random) -> Duration {
        let mut bytes = [0u8; 2];
        random.fill_bytes(&mut bytes);
        Duration::from_millis(u64::from(u16::from_le_bytes(bytes) % 500))
    }
}

/// The worker itself.
struct Worker {
    sink: Sink,
    vault_path: PathBuf,
    signed: Option<Signed>,
    gateway: Option<Gateway>,
    /// Armed while the gateway is down and a reconnect is still worth trying.
    retry: Option<Retry>,
    /// The room a leave is in flight for. The wire's acknowledgement names no room, so the
    /// request's own id is the only thing that can say which room the ack answers.
    pending_leave: Option<Id>,
}

impl Worker {
    fn new(sink: Sink, vault_path: PathBuf) -> Self {
        Self {
            sink,
            vault_path,
            signed: None,
            gateway: None,
            retry: None,
            pending_leave: None,
        }
    }

    /// The worker's whole life: report what the vault looks like, then serve commands and frames.
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        if vault::exists(&self.vault_path) {
            // The username and server are inside the encrypted body, so they are not knowable until
            // the passphrase arrives. The unlock screen shows the file, not the account.
            self.sink.send(Event::VaultFound);
        } else {
            self.sink.send(Event::VaultMissing);
        }

        loop {
            // Read out of `self` before either future is built: the frame future borrows the gateway
            // mutably, so the reconnect timer must not also capture `self`.
            let wait = self.retry.map(|plan| plan.wait);

            // All three arms in one select so a command is served promptly even while a frame is
            // pending, an inbound frame is not delayed behind an idle command channel, and a reconnect
            // backoff never blocks either of the other two.
            let frame = async {
                match self.gateway.as_mut() {
                    Some(gateway) => Some(gateway.next_frame().await),
                    // No connection: park forever rather than spin. The command arm still wakes us.
                    None => {
                        std::future::pending::<()>().await;
                        None
                    }
                }
            };
            let due = async move {
                match wait {
                    Some(wait) => tokio::time::sleep(wait).await,
                    // Nothing scheduled: park, so this arm never completes and never spins.
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                command = commands.recv() => {
                    match command {
                        Some(Command::Shutdown) | None => break,
                        Some(command) => self.handle(command).await,
                    }
                }
                Some(result) = frame => {
                    match result {
                        Ok(frame) => self.on_frame(frame).await,
                        Err(error) => self.on_disconnect(error),
                    }
                }
                () = due => self.reconnect().await,
            }
        }

        if let Some(gateway) = self.gateway.take() {
            gateway.close().await;
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::FetchCaptcha { server, mode } => {
                self.fetch_captcha(server, mode).await;
            }
            Command::Register {
                server,
                username,
                password,
                passphrase,
                captcha,
            } => {
                self.bootstrap(server, username, password, passphrase, captcha, true)
                    .await;
            }
            Command::SignIn {
                server,
                identifier,
                password,
                passphrase,
                captcha,
            } => {
                self.bootstrap(server, identifier, password, passphrase, captcha, false)
                    .await;
            }
            Command::Unlock { passphrase } => self.unlock(passphrase).await,
            Command::SignOut => self.sign_out().await,
            Command::Conversations => self.request_conversations().await,
            Command::History {
                conversation_id,
                have_seq,
            } => {
                self.request_history(conversation_id, have_seq).await;
            }
            Command::SendText {
                conversation_id,
                text,
            } => {
                self.send_text(conversation_id, text).await;
            }
            Command::StartDirect { username } => self.start_direct(username).await,
            Command::StartDirectById { peer } => self.start_direct_by_id(peer).await,
            Command::Typing {
                conversation_id,
                typing,
            } => {
                self.send_typing(conversation_id, typing).await;
            }
            Command::MarkRead {
                conversation_id,
                seq,
            } => {
                self.mark_read(conversation_id, seq).await;
            }
            Command::Friends => self.request_relationships().await,
            Command::AddFriend { user_id } => self.add_friend(user_id).await,
            Command::RespondFriend { user_id, accept } => {
                self.respond_friend(user_id, accept).await;
            }
            Command::Sessions => self.fetch_sessions().await,
            Command::RevokeSession { session_id } => {
                self.revoke_session(session_id).await;
            }
            Command::Rooms { query } => self.request_rooms(query).await,
            Command::JoinRoom { room_id } => self.join_room(room_id).await,
            Command::CreateRoom {
                slug,
                name,
                managed,
                topic,
            } => {
                self.create_room(slug, name, managed, topic).await;
            }
            Command::LeaveRoom { room_id } => self.leave_room(room_id).await,
            Command::Notifications => self.request_notifications().await,
            Command::AcknowledgeAlerts { through_unix_ms } => {
                self.acknowledge_alerts(through_unix_ms).await;
            }
            Command::Wallet => self.request_wallet().await,
            Command::SendGift { sku, recipient } => {
                self.send_gift(sku, recipient).await;
            }
            Command::SearchPeople { query } => self.search_people(query).await,
            Command::Suggestions => self.request_suggestions().await,
            Command::Shutdown => {}
        }
    }

    /// Registers or signs in, generating keys and writing the vault.
    async fn bootstrap(
        &mut self,
        server: ServerEndpoint,
        identifier: String,
        password: String,
        passphrase: String,
        captcha: Option<CaptchaAnswer>,
        register: bool,
    ) {
        self.sink.send(Event::Connection(Connection::Connecting));

        let rest = match Rest::new(&crate::config::rest_base_url(&server)) {
            Ok(rest) => rest,
            Err(error) => return self.fail(error.to_string()),
        };

        // Reuse the existing device id when a vault is already present, so signing in again does not
        // orphan the identity key every peer has already verified.
        let existing = vault::load(&self.vault_path, &passphrase).ok();
        let device_id = existing
            .as_ref()
            .and_then(|keys| keys.session.as_ref())
            .map(|s| s.device_id);
        let device = DeviceRequest::describe(device_id);

        // Lent to the wire body rather than moved into it, so the answer's bytes stay in the
        // command's own allocation until the request is done.
        let proof = captcha.as_ref().map(|answer| CaptchaProof {
            challenge_id: &answer.challenge_id,
            answer: &answer.answer,
        });
        let grant = if register {
            rest.register(&identifier, &password, device, proof).await
        } else {
            rest.login(&identifier, &password, device, proof).await
        };
        let grant = match grant {
            Ok(grant) => grant,
            Err(error) => {
                // A captcha refusal is not a dead form: the attempt consumed the challenge
                // either way, so tell the UI to drop it and draw a fresh one, and let the
                // ordinary failure path below keep the form standing for the retry.
                if matches!(
                    &error,
                    RestError::Server { symbol, .. }
                        if CAPTCHA_REFUSAL_SYMBOLS.contains(&symbol.as_str())
                ) {
                    self.sink.send(Event::CaptchaRefused);
                }
                return self.fail(error.to_string());
            }
        };

        // A vault whose passphrase just opened keeps its keys; otherwise this device is new and needs
        // a fresh identity. Generating one unconditionally would silently replace the key peers have
        // verified, and every safety number would change with no explanation.
        let mut keys = existing.unwrap_or_else(DeviceKeys::generate);
        keys.session = Some(SavedSession {
            server_url: crate::config::rest_base_url(&server),
            account_id: grant.account_id,
            device_id: grant.device_id,
            username: identifier.clone(),
            refresh_token: grant.refresh_token.clone(),
        });
        if let Err(error) = vault::save(&self.vault_path, &passphrase, &keys) {
            return self.fail(error.to_string());
        }

        self.establish(
            server,
            rest,
            keys,
            grant.account_id,
            grant.device_id,
            grant.session_id,
            identifier,
            grant.access_token,
        )
        .await;
    }

    /// Fetches a captcha challenge for a form.
    ///
    /// Its failures are reported through [`Event::CaptchaUnavailable`] rather than
    /// [`Self::fail`], because a challenge that will not load is not a failed sign-in: `fail`
    /// would flip the connection state of a form that never submitted, and release a busy flag
    /// that was never set.
    async fn fetch_captcha(&mut self, server: ServerEndpoint, mode: Option<String>) {
        let outcome = match Rest::new(&crate::config::rest_base_url(&server)) {
            Ok(rest) => rest.request_captcha(mode.as_deref()).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(challenge) => self.sink.send(Event::CaptchaChallenge(challenge)),
            Err(error) => self.sink.send(Event::CaptchaUnavailable {
                reason: error.to_string(),
            }),
        }
    }

    /// Opens the vault and resumes its saved sign-in.
    async fn unlock(&mut self, passphrase: String) {
        self.sink.send(Event::Connection(Connection::Connecting));

        let keys = match vault::load(&self.vault_path, &passphrase) {
            Ok(keys) => keys,
            Err(error) => return self.fail(error.to_string()),
        };
        let Some(saved) = keys.session.clone() else {
            return self.fail("this vault has no saved sign-in; sign in again".to_owned());
        };
        // Reconstruct the server endpoint from the legacy string field, falling back to the dev
        // policy for any shape the parser does not recognise. The saved URL is the one this device
        // last successfully used, so the form is not consulted on unlock.
        let server = crate::config::server_endpoint_from_url(&saved.server_url);
        let rest = match Rest::new(&crate::config::rest_base_url(&server)) {
            Ok(rest) => rest,
            Err(error) => return self.fail(error.to_string()),
        };
        let grant = match rest.refresh(&saved.refresh_token, saved.device_id).await {
            Ok(grant) => grant,
            Err(error) => return self.fail(error.to_string()),
        };

        // The server rotates the refresh token on every exchange, so the vault has to be rewritten or
        // the next unlock would present a token the server has already retired — which it treats as
        // refresh reuse, and rightly so.
        let mut keys = keys;
        keys.session = Some(SavedSession {
            refresh_token: grant.refresh_token.clone(),
            ..saved.clone()
        });
        if let Err(error) = vault::save(&self.vault_path, &passphrase, &keys) {
            return self.fail(error.to_string());
        }

        self.establish(
            server,
            rest,
            keys,
            grant.account_id,
            grant.device_id,
            grant.session_id,
            saved.username,
            grant.access_token,
        )
        .await;
    }

    /// Brings up the gateway connection and publishes this device's public keys.
    #[allow(clippy::too_many_arguments)]
    async fn establish(
        &mut self,
        server: ServerEndpoint,
        rest: Rest,
        keys: DeviceKeys,
        account_id: Id,
        device_id: Id,
        session_id: Id,
        username: String,
        access_token: String,
    ) {
        let safety_number = model::safety_number(&keys.identity_public().fingerprint());
        let account = Account {
            account_id,
            device_id,
            session_id,
            username,
            safety_number,
        };
        let sessions = SessionStore::new(keys);

        self.signed = Some(Signed {
            server,
            rest,
            account: account.clone(),
            access_token,
            sessions,
            bundles: HashMap::new(),
            devices: HashMap::new(),
            members: HashMap::new(),
            watched: HashSet::new(),
        });

        self.sink.send(Event::SignedIn(account));
        self.connect().await;
    }

    /// Connects the gateway, retrying with backoff until it succeeds or the session ends.
    async fn connect(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let url = crate::config::gateway_url(&signed.server);
        let hello = migo_protocol::Hello {
            protocol_version: migo_protocol::PROTOCOL_VERSION,
            client: ClientInfo {
                platform: migo_protocol::Platform::Desktop,
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                os_version: None,
                device_model: None,
            },
            // Only what this client actually implements. Claiming a feature it cannot honour would
            // make the server send frames it then ignores, and the user would see silence rather than
            // an error.
            features: features::E2E_V1
                | features::PRESENCE
                | features::TYPING
                | features::COMPRESSION,
            locale: "en".to_owned(),
            bandwidth_mode: migo_protocol::BandwidthMode::Auto,
            access_token: Some(signed.access_token.clone()),
            device_id: Some(signed.account.device_id),
            resume: None,
        };

        self.sink.send(Event::Connection(Connection::Connecting));
        match Gateway::connect(&url, hello).await {
            Ok((gateway, _welcome)) => {
                self.gateway = Some(gateway);
                self.retry = None;
                self.sink.send(Event::Connection(Connection::Online));
                self.publish_keys().await;
                // A fresh session holds no subscriptions, so the accounts this device watches for
                // presence go back to "never subscribed" and the own-topic subscribe below is the
                // only one that can be sent unconditionally.
                if let Some(signed) = self.signed.as_mut() {
                    signed.watched.clear();
                }
                self.subscribe_self().await;
                self.announce_presence().await;
                self.request_conversations().await;
                // The graph is refreshed on every reconnect for the same reason the conversation
                // list is: the other devices of this account act on it too, and a client that
                // never re-reads shows a friendship that ended an hour ago.
                self.request_relationships().await;
                // The dashboard's own facts ride the same reconnect: the rooms, the suggestions,
                // the inbox, and the wallet are the session's other four screens' first reads,
                // and a reconnect is a session boundary to them too.
                self.request_rooms(String::new()).await;
                self.request_suggestions().await;
                self.request_notifications().await;
                self.request_wallet().await;
            }
            Err(error) => {
                self.sink
                    .send(Event::Connection(Connection::Failed(error.to_string())));
            }
        }
    }

    /// One scheduled reconnect attempt, run from the select loop.
    ///
    /// Takes the plan first, so a failure to re-arm cannot leave a timer firing in a tight loop.
    async fn reconnect(&mut self) {
        let Some(plan) = self.retry.take() else {
            return;
        };
        if self.signed.is_none() {
            return;
        }
        self.connect().await;
        if self.gateway.is_some() {
            return;
        }
        match plan.after_failure(&mut OsRandom) {
            Some(next) => self.retry = Some(next),
            None => self.sink.send(Event::Connection(Connection::Failed(
                "could not reconnect; check the server address and try signing in again".to_owned(),
            ))),
        }
    }

    /// Publishes this device's public identity and prekeys.
    async fn publish_keys(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let remaining = signed.sessions.one_time_remaining();
        let keys = signed.sessions.keys();
        let signed_prekey = keys.signed_prekey_signed();
        let message = migo_protocol::KeyPublish {
            identity_key: keys.identity_public().to_bytes().to_vec(),
            signed_prekey_id: signed_prekey.key_id,
            signed_prekey: signed_prekey.public_key.to_vec(),
            signed_prekey_signature: signed_prekey.signature.to_vec(),
            one_time_prekeys: keys
                .one_time_public()
                .into_iter()
                .map(|(key_id, public_key)| migo_protocol::PrekeyEntry {
                    key_id,
                    public_key: public_key.to_vec(),
                })
                .collect(),
        };
        self.request(Opcode::KeyPublish, &message).await;

        // Worth saying out loud before the pool empties. A peer can still start a session from the
        // signed prekey without a one-time key, so nothing breaks — but that first message loses the
        // one-time key's forward secrecy, and the user is the only one who can fix it, by signing in
        // again with the passphrase so a fresh pool can be generated and sealed into the vault. This
        // worker deliberately does not hold the passphrase after unlock, so it cannot do that itself.
        if remaining < ONE_TIME_PREKEY_LOW_WATER {
            self.sink.toast(
                format!("Only {remaining} one-time keys left. Sign in again to replenish them."),
                ToastKind::Info,
            );
        }
    }

    async fn request_conversations(&mut self) {
        let message = migo_protocol::ConversationListRequest {
            limit: 100,
            cursor: None,
        };
        self.request(Opcode::ConversationList, &message).await;
    }

    async fn request_history(&mut self, conversation_id: Id, have_seq: u64) {
        let message = migo_protocol::SyncRequest {
            conversation_id,
            have_seq,
            limit: 100,
            to_seq: None,
            backwards: None,
        };
        self.request(Opcode::Sync, &message).await;
    }

    async fn start_direct(&mut self, username: String) {
        // A username has to become an account id before a conversation can name its members, and the
        // profile lookup is the only thing that can do it. The conversation is created when the
        // response arrives.
        let message = migo_protocol::ProfileRequest {
            user_ids: Vec::new(),
        };
        let _ = message;
        // PROFILE_FETCH takes ids, not names, so a username search is a REST concern rather than a
        // gateway one. Until that endpoint exists on the server, accept an id typed directly and say
        // so plainly rather than failing silently.
        match Id::parse(username.trim()) {
            Ok(peer) => {
                let Some(signed) = self.signed.as_ref() else {
                    return;
                };
                let create = migo_protocol::ConversationCreateRequest {
                    kind: ConversationKind::Direct,
                    members: vec![signed.account.account_id, peer],
                    title: None,
                };
                self.request(Opcode::ConversationCreate, &create).await;
            }
            Err(_) => self.sink.toast(
                "enter the account id of the person to message",
                ToastKind::Info,
            ),
        }
    }

    /// Starts a direct conversation with an account id already held.
    ///
    /// The parsed-id path of [`Self::start_direct`] without the parsing: a search hit or a
    /// suggestion already names an account, and round-tripping its text form would be a parse of
    /// a value this worker minted.
    async fn start_direct_by_id(&mut self, peer: Id) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let create = migo_protocol::ConversationCreateRequest {
            kind: ConversationKind::Direct,
            members: vec![signed.account.account_id, peer],
            title: None,
        };
        self.request(Opcode::ConversationCreate, &create).await;
    }

    async fn send_typing(&mut self, conversation_id: Id, typing: bool) {
        let message = migo_protocol::TypingEvent {
            conversation_id,
            state: if typing {
                migo_protocol::TypingState::Start
            } else {
                migo_protocol::TypingState::Stop
            },
            user_id: None,
        };
        // Best effort by design: a lost typing indicator costs nothing, and retrying one would put a
        // stale "typing…" on someone's screen after the message had already arrived.
        self.request(Opcode::Typing, &message).await;
    }

    async fn mark_read(&mut self, conversation_id: Id, seq: u64) {
        let message = migo_protocol::MessageReceipt {
            conversation_id,
            kind: migo_protocol::ReceiptKind::Read,
            seq,
            user_id: None,
            at: None,
        };
        self.request(Opcode::MessageReceipt, &message).await;
    }

    /// Subscribes this session to its own account's user topic.
    ///
    /// That topic is where the server puts the events addressed to this account rather than to
    /// one of its conversations: friend requests, acceptances, notifications. The gateway grants
    /// it by right (a user's own presence stream is theirs), so a refusal here would mean the
    /// session is not really authenticated — which the handshake would already have caught.
    async fn subscribe_self(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let request = SubscribeRequest {
            topics: vec![Topic {
                kind: TopicKind::User,
                id: signed.account.account_id,
            }],
        };
        self.request(Opcode::Subscribe, &request).await;
    }

    /// Announces this device as online.
    ///
    /// What the web client does after its connect, done here for the same reason: presence on
    /// this server is per-device and client-reported, so a client that never speaks PRESENCE_SET
    /// reads as unobserved to everyone watching. Best effort — a set that fails costs one green
    /// dot, and the reconnect path will try again anyway.
    async fn announce_presence(&mut self) {
        let message = migo_protocol::PresenceUpdate {
            state: migo_protocol::PresenceState::Online,
            custom_status: None,
        };
        self.request(Opcode::PresenceSet, &message).await;
    }

    /// Subscribes to the user topics of accounts this device wants presence for.
    ///
    /// Only unwatched ids are sent, and the batch is capped so one refresh on a large graph
    /// cannot produce an unbounded frame. A refusal is silent by design: the server answers
    /// "no" without a reason so SUBSCRIBE cannot be used to probe, and a client that grilled
    /// the user about every declined watch would be inventing reasons the server chose not to
    /// give.
    async fn watch_users(&mut self, ids: Vec<Id>) {
        const WATCH_BATCH: usize = 128;
        let topics: Vec<Topic> = {
            let Some(signed) = self.signed.as_mut() else {
                return;
            };
            ids.into_iter()
                .filter(|id| signed.watched.insert(*id))
                .take(WATCH_BATCH)
                .map(|id| Topic {
                    kind: TopicKind::User,
                    id,
                })
                .collect()
        };
        if topics.is_empty() {
            return;
        }
        let request = SubscribeRequest { topics };
        self.request(Opcode::Subscribe, &request).await;
    }

    /// Requests the account's whole social graph.
    async fn request_relationships(&mut self) {
        let message = RelationshipListReq { limit: 200 };
        self.request(Opcode::RelationshipList, &message).await;
    }

    /// Sends a friend request to whatever the user typed, if it names an account.
    async fn add_friend(&mut self, user_id: String) {
        match Id::parse(user_id.trim()) {
            Ok(target) => {
                let message = FriendTarget { user_id: target };
                self.request(Opcode::FriendRequest, &message).await;
            }
            Err(_) => self
                .sink
                .toast("enter the account id of the person to add", ToastKind::Info),
        }
    }

    /// Accepts or declines a pending request.
    async fn respond_friend(&mut self, user_id: Id, accept: bool) {
        let message = FriendRespond { user_id, accept };
        self.request(Opcode::FriendRespond, &message).await;
    }

    /// Fetches names and presence for a set of accounts in one PROFILE_FETCH.
    ///
    /// The friends list and the direct-conversation titles both render through the names map, so
    /// one fetch here serves two screens. Requests are capped because a profile answer costs the
    /// server a row read per account, and an account with a thousand relationships should not
    /// turn one refresh into a thousand-row fan-out.
    async fn fetch_profiles(&mut self, ids: Vec<Id>) {
        const PROFILE_BATCH: usize = 128;
        if ids.is_empty() {
            return;
        }
        // Deduped before the cap so one account holding two relationship kinds (a friend and a
        // favourite, say) costs one profile read, not one of the batch's slots.
        let mut ids = ids;
        ids.sort_unstable();
        ids.dedup();
        ids.truncate(PROFILE_BATCH);
        let message = migo_protocol::ProfileRequest { user_ids: ids };
        self.request(Opcode::ProfileFetch, &message).await;
    }

    /// Fetches the device/session list over REST and reduces it to rows the settings screen can
    /// draw without knowing what JSON is.
    async fn fetch_sessions(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed.rest.sessions(&signed.access_token).await;
        let event = match outcome {
            Ok(list) => {
                let rows = list
                    .into_iter()
                    .map(|session| SessionRow {
                        session_id: session.session_id,
                        device: session
                            .device
                            .as_ref()
                            .and_then(|device| {
                                let name = device.display_name.as_deref();
                                name.filter(|name| !name.is_empty()).or_else(|| {
                                    device
                                        .platform
                                        .as_deref()
                                        .filter(|platform| !platform.is_empty())
                                })
                            })
                            .map(str::to_owned)
                            .unwrap_or_else(|| model::short_id(session.session_id)),
                        created_at: session.created_at,
                        last_active_at: session.last_active_at,
                        current: session.current,
                    })
                    .collect::<Vec<_>>();
                Event::Sessions(Ok(rows))
            }
            Err(error) => Event::Sessions(Err(error.to_string())),
        };
        self.sink.send(event);
    }

    /// Ends one session of the account, then re-reads the list.
    ///
    /// Revoking the session this window runs on would leave a signed-out client holding
    /// decrypted history, so the settings panel does not offer the button for it; if a revoke
    /// arrives here anyway (or the server ends the session out from under the list), the
    /// refresh that follows is what reconciles the UI with the truth.
    async fn revoke_session(&mut self, session_id: Id) {
        let outcome = match self.signed.as_ref() {
            Some(signed) => {
                signed
                    .rest
                    .revoke_session(&signed.access_token, session_id)
                    .await
            }
            None => return,
        };
        match outcome {
            Ok(()) => {
                self.sink.toast("Session ended", ToastKind::Success);
                self.fetch_sessions().await;
            }
            Err(error) => {
                self.sink.toast(error.to_string(), ToastKind::Error);
            }
        }
    }

    /// Encrypts one text message for every device of every other member, and sends it.
    async fn send_text(&mut self, conversation_id: Id, text: String) {
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        let mut random = OsRandom;
        let message_id = Id::generate_at(Timestamp::now(), &mut random);

        // Show it immediately, marked as sending. A message that appears only after the server
        // acknowledges it makes a slow link feel broken; one that appears at once and then gains a
        // tick tells the truth about what has happened so far.
        self.sink.send(Event::Message(Message {
            message_id,
            conversation_id,
            seq: 0,
            sender_id: signed.account.account_id,
            outgoing: true,
            body: Body::Text(text.clone()),
            sent_at: Timestamp::now(),
            delivery: Delivery::Sending,
        }));

        let plaintext = match content::encode(&Content::text(text), true) {
            Ok(bytes) => bytes,
            Err(_) => return self.sink.send(Event::SendFailed { message_id }),
        };

        // The recipient set is every device of every other member. One envelope per device is what
        // makes a compromised phone unable to read what the laptop received.
        let peers: Vec<Id> = signed
            .members
            .get(&conversation_id)
            .map(|members| {
                members
                    .iter()
                    .copied()
                    .filter(|id| *id != signed.account.account_id)
                    .collect()
            })
            .unwrap_or_default();

        let mut targets: Vec<Id> = Vec::new();
        for peer in &peers {
            match signed.devices.get(peer) {
                Some(devices) => targets.extend(devices.iter().copied()),
                None => {
                    // No bundle yet. Ask for one; the message is retried when it arrives.
                    let request = migo_protocol::KeyBundleRequest {
                        user_id: *peer,
                        device_id: None,
                    };
                    self.request(Opcode::KeyBundleFetch, &request).await;
                    self.sink.toast(
                        "fetching keys for this conversation, try again in a moment",
                        ToastKind::Info,
                    );
                    self.sink.send(Event::SendFailed { message_id });
                    return;
                }
            }
        }

        if targets.is_empty() {
            self.sink
                .toast("no other device to send to yet", ToastKind::Info);
            self.sink.send(Event::SendFailed { message_id });
            return;
        }

        // One MESSAGE_SEND per recipient device, sharing the message id so the conversation shows one
        // message rather than one per device.
        for device in targets {
            let Some(signed) = self.signed.as_mut() else {
                return;
            };
            let bundle = signed.bundles.get(&device).cloned();
            let envelope = match signed.sessions.seal(device, bundle.as_ref(), &plaintext) {
                Ok(envelope) => envelope,
                Err(_) => continue,
            };
            let bytes = match envelope.encode() {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let message = migo_protocol::MessageSend {
                message_id,
                conversation_id,
                kind: MessageKind::Text,
                envelope: bytes,
                reply_to: None,
                expires_in_ms: None,
                sender_key_id: None,
            };
            self.request(Opcode::MessageSend, &message).await;
        }
    }

    /// Requests the public room directory, narrowed by a query when one is held.
    async fn request_rooms(&mut self, query: String) {
        let query = query.trim().to_owned();
        let message = RoomListRequest {
            limit: 50,
            query: (!query.is_empty()).then_some(query),
            category: None,
            language: None,
            country: None,
            cursor: None,
        };
        self.request(Opcode::RoomList, &message).await;
    }

    /// Creates a room and enters it. The wire's create call resolves with a join handle —
    /// creation is entry, the creator is the first member and its Owner — so the reply is
    /// handled by the join path and nothing here has a second flow to keep in step.
    async fn create_room(
        &mut self,
        slug: String,
        name: String,
        managed: bool,
        topic: Option<String>,
    ) {
        let message = RoomCreate {
            slug,
            name,
            kind: if managed { 2 } else { 1 },
            topic,
            max_members: None,
        };
        self.request(Opcode::RoomCreate, &message).await;
    }

    /// Leaves a room. The server closes the conversation for this account; the conversation list
    /// re-reads behind the acknowledgement, and the rooms pane drops the room from its joined set.
    async fn leave_room(&mut self, room_id: Id) {
        self.pending_leave = Some(room_id);
        let message = RoomLeaveRequest { room_id };
        self.request(Opcode::RoomLeave, &message).await;
    }

    /// Joins a room. The reply names both halves — the room and the conversation — and the
    /// conversation list re-reads behind it, exactly as a started direct chat does.
    async fn join_room(&mut self, room_id: Id) {
        let message = RoomJoinRequest {
            room_id,
            invite_code: None,
        };
        self.request(Opcode::RoomJoin, &message).await;
    }

    /// Requests the durable notification inbox.
    async fn request_notifications(&mut self) {
        // The server keeps no pagination cursor for the inbox, so the page is asked for plainly.
        let message = InboxReq {
            limit: 50,
            cursor: None,
        };
        self.request(Opcode::NotificationList, &message).await;
    }

    /// Marks every notification at or before one instant read.
    ///
    /// The wire carries an id rather than a timestamp, and the server reads the id's embedded time
    /// prefix as the watermark — so this synthesises an id whose prefix *is* the instant: the six
    /// leading bytes of the millisecond count, then zeros. It names an instant, not an entity.
    async fn acknowledge_alerts(&mut self, through_unix_ms: i64) {
        let ms = through_unix_ms.max(0) as u64;
        let mut bytes = [0u8; 16];
        bytes[0] = (ms >> 40) as u8;
        bytes[1] = (ms >> 32) as u8;
        bytes[2] = (ms >> 24) as u8;
        bytes[3] = (ms >> 16) as u8;
        bytes[4] = (ms >> 8) as u8;
        bytes[5] = ms as u8;
        let message = NotificationAck {
            id: migo_core::Id::from_bytes(bytes),
        };
        self.request(Opcode::NotificationAck, &message).await;
    }

    /// Fires the wallet's whole economy: six reads, each arriving as its own event.
    async fn request_wallet(&mut self) {
        self.request(Opcode::BalanceFetch, &WalletReq {}).await;
        let ledger = LedgerReq { limit: Some(10) };
        self.request(Opcode::LedgerHistory, &ledger).await;
        if let Some(signed) = self.signed.as_ref() {
            let me = signed.account.account_id;
            self.request(Opcode::Progression, &ProgressionReq { of_account: me })
                .await;
            self.request(Opcode::Badges, &BadgesReq { of_account: me })
                .await;
        }
        let board = LeaderboardReq {
            board: "xp".to_owned(),
            limit: Some(10),
        };
        self.request(Opcode::Leaderboard, &board).await;
        self.request(Opcode::GiftCatalogue, &GiftCatalogueReq {})
            .await;
    }

    /// Buys and delivers a gift. On acceptance the wallet re-reads, so the balance and the
    /// statement move to the server's arithmetic rather than a local guess.
    async fn send_gift(&mut self, sku: String, recipient: Id) {
        let message = GiftSend {
            gift: sku,
            recipient,
            conversation_id: None,
        };
        self.request(Opcode::GiftSend, &message).await;
    }

    /// Searches public profiles by username prefix.
    async fn search_people(&mut self, query: String) {
        let message = SearchReq {
            query: query.trim().to_owned(),
            limit: Some(10),
        };
        self.request(Opcode::Search, &message).await;
    }

    /// Asks the social graph for its own suggestions.
    async fn request_suggestions(&mut self) {
        let message = SuggestReq { limit: Some(8) };
        self.request(Opcode::Suggestions, &message).await;
    }

    /// The room directory came back: reduce it to rows.
    fn on_rooms(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::RoomListResponse>(frame) else {
            return;
        };
        let rows = response
            .rooms
            .into_iter()
            .map(|room| RoomRow {
                room_id: room.room_id,
                name: room.name,
                topic: room.topic.filter(|topic| !topic.is_empty()),
                member_count: room.member_count,
                online_count: room.online_count,
                category: room.category,
                verified: room.verified.unwrap_or(false),
            })
            .collect();
        self.sink.send(Event::Rooms(rows));
    }

    /// A join was accepted: note the conversation's (empty) member set, re-read the list, and tell
    /// the UI which thread to open.
    async fn on_room_joined(&mut self, frame: &migo_protocol::Frame) {
        let Ok(joined) = gateway::decode::<migo_protocol::RoomJoinResponse>(frame) else {
            return;
        };
        if let Some(signed) = self.signed.as_mut() {
            // A room's member set is served by the roster, not the join; the empty set here only
            // seeds the map so a send before the list re-reads does not mis-address.
            signed.members.entry(joined.conversation_id).or_default();
        }
        self.sink.send(Event::RoomJoined {
            conversation_id: joined.conversation_id,
            room_id: joined.room.room_id,
            title: joined.room.name,
        });
        self.request_conversations().await;
    }

    /// A leave was accepted: the rooms pane drops the room, and the conversation list re-reads so
    /// the closed conversation stops being offered.
    async fn on_room_left(&mut self, frame: &migo_protocol::Frame) {
        let Ok(acknowledged) = gateway::decode::<migo_protocol::Acknowledged>(frame) else {
            return;
        };
        let Some(room_id) = self.pending_leave.take() else {
            return;
        };
        if !acknowledged.ok {
            return;
        }
        self.sink.send(Event::RoomLeft { room_id });
        self.request_conversations().await;
    }

    /// The inbox came back: reduce it to rows.
    fn on_alerts(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::InboxResponse>(frame) else {
            return;
        };
        let rows = response
            .items
            .into_iter()
            .map(|item| AlertRow {
                id: item.id,
                kind: item.kind,
                title: item.title.filter(|title| !title.is_empty()),
                at: item.at,
            })
            .collect();
        self.sink.send(Event::Alerts(rows));
    }

    /// A notification was pushed. The push is droppable by design and carries no plaintext, so it
    /// is a cue to re-read, never a row: the event says "look again" and nothing more.
    fn on_alert_pushed(&mut self, frame: &migo_protocol::Frame) {
        let Ok(_event) = gateway::decode::<migo_protocol::NotificationEvent>(frame) else {
            return;
        };
        self.sink.send(Event::AlertPushed);
    }

    /// The wallet came back.
    fn on_balance(&mut self, frame: &migo_protocol::Frame) {
        let Ok(wallet) = gateway::decode::<migo_protocol::WalletView>(frame) else {
            return;
        };
        self.sink.send(Event::Balance {
            coins: wallet.balance,
            points: wallet.points,
        });
    }

    /// The statement came back: the sign comes from each line's reason, never its amount.
    fn on_ledger(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::LedgerResponse>(frame) else {
            return;
        };
        let rows = response
            .entries
            .into_iter()
            .map(|entry| LedgerRow {
                credit: model::ledger_credit(&entry.reason),
                reason: entry.reason,
                amount: entry.amount,
                balance_after: entry.balance_after,
                at: entry.at,
            })
            .collect();
        self.sink.send(Event::Ledger(rows));
    }

    /// The progression came back.
    fn on_progression(&mut self, frame: &migo_protocol::Frame) {
        let Ok(wire) = gateway::decode::<migo_protocol::ProgressionWire>(frame) else {
            return;
        };
        self.sink.send(Event::ProgressionArrived(Progression {
            level: wire.level,
            xp_into_level: wire.xp_into_level,
            xp_for_next_level: wire.xp_for_next_level,
        }));
    }

    /// The badges came back, by code.
    fn on_badges(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::BadgesResponse>(frame) else {
            return;
        };
        let codes = response
            .badges
            .into_iter()
            .map(|badge| badge.badge_code)
            .collect();
        self.sink.send(Event::Badges(codes));
    }

    /// The leaderboard came back.
    fn on_leaderboard(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::LeaderboardResponse>(frame) else {
            return;
        };
        let rows = response
            .ranks
            .into_iter()
            .map(|rank| LeaderRow {
                position: rank.position,
                account_id: rank.account_id,
                xp: rank.xp,
                level: rank.level,
            })
            .collect();
        self.sink.send(Event::Leaderboard(rows));
    }

    /// The gift catalogue came back.
    fn on_gifts(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::GiftCatalogueResponse>(frame) else {
            return;
        };
        let rows = response
            .gifts
            .into_iter()
            .map(|gift| GiftRow {
                sku: gift.sku,
                name: gift.name,
                price: gift.price,
                category: gift.category,
            })
            .collect();
        self.sink.send(Event::Gifts(rows));
    }

    /// A gift was sent (or refused): toast the outcome, then re-read the money-side facts.
    async fn on_gift_sent(&mut self, frame: &migo_protocol::Frame) {
        let Ok(result) = gateway::decode::<migo_protocol::GiftSendResult>(frame) else {
            return;
        };
        if result.ok {
            self.sink.toast("Gift sent", ToastKind::Success);
            self.request_wallet().await;
        }
    }

    /// People came back, from search or suggestions: one event for both, because a row cannot tell
    /// them apart and neither can the screen that draws it.
    fn on_people(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::SearchResponse>(frame) else {
            return;
        };
        let rows = response
            .results
            .into_iter()
            .map(|person| PersonRow {
                account_id: person.account_id,
                username: person.username,
                display_name: person.display_name,
                mutual_friends: person.mutual_friends,
            })
            .collect();
        self.sink.send(Event::People(rows));
    }

    /// Signs out: forgets the keys locally first, then tells the server.
    ///
    /// The order matters. Local first means an unreachable server cannot leave a signed-out client
    /// still holding the material to decrypt its history.
    async fn sign_out(&mut self) {
        if let Some(gateway) = self.gateway.take() {
            gateway.close().await;
        }
        if let Some(mut signed) = self.signed.take() {
            signed.sessions.clear();
            let _ = signed
                .rest
                .logout(&signed.access_token, signed.account.session_id)
                .await;
        }
        self.retry = None;
        self.sink.send(Event::Connection(Connection::Offline));
        self.sink.send(Event::SignedOut);
        self.sink.toast(
            "Signed out. Every key on this device has been forgotten.",
            ToastKind::Success,
        );
    }

    /// Sends a request frame, reporting a transport failure as a disconnect.
    async fn request<T: migo_protocol::Encode>(&mut self, opcode: Opcode, value: &T) {
        let Some(gateway) = self.gateway.as_mut() else {
            self.sink.toast("not connected", ToastKind::Error);
            return;
        };
        let correlation = gateway.correlate();
        if let Err(error) = gateway.send(opcode, correlation, value).await {
            self.on_disconnect(error);
        }
    }

    /// Dispatches one inbound frame.
    async fn on_frame(&mut self, frame: migo_protocol::Frame) {
        if gateway::is_error(&frame) {
            let error = gateway::refusal(&frame);
            self.sink.toast(error.to_string(), ToastKind::Error);
            return;
        }
        let Some(opcode) = Opcode::from_wire(frame.header.opcode) else {
            // An opcode this build does not know is not an error: the server may be newer. Ignoring it
            // is exactly what forward compatibility means.
            return;
        };
        match opcode {
            Opcode::Ping => self.on_ping(&frame).await,
            Opcode::MessageEvent => self.on_message(&frame),
            Opcode::MessageSend => self.on_accepted(&frame),
            Opcode::ConversationList => self.on_conversations(&frame).await,
            Opcode::ConversationCreate => self.on_conversation_created(&frame).await,
            Opcode::Sync => self.on_history(&frame),
            Opcode::KeyBundleFetch => self.on_bundles(&frame),
            Opcode::Typing => self.on_typing(&frame),
            Opcode::ProfileFetch => self.on_profiles(&frame),
            Opcode::RelationshipList => self.on_relationships(&frame).await,
            // The acknowledgement of a FRIEND_REQUEST or FRIEND_RESPOND. Both mean the graph
            // moved and the list in the UI is now stale, so both take the same action: re-read.
            Opcode::FriendRequest | Opcode::FriendRespond => self.on_social_ack(&frame).await,
            Opcode::FriendEvent => self.on_friend_event(&frame),
            Opcode::PresenceEvent => self.on_presence(&frame),
            Opcode::RoomList => self.on_rooms(&frame),
            Opcode::RoomJoin | Opcode::RoomCreate => self.on_room_joined(&frame).await,
            Opcode::RoomLeave => self.on_room_left(&frame).await,
            Opcode::NotificationList => self.on_alerts(&frame),
            Opcode::NotificationEvent => self.on_alert_pushed(&frame),
            Opcode::BalanceFetch => self.on_balance(&frame),
            Opcode::LedgerHistory => self.on_ledger(&frame),
            Opcode::Progression => self.on_progression(&frame),
            Opcode::Badges => self.on_badges(&frame),
            Opcode::Leaderboard => self.on_leaderboard(&frame),
            Opcode::GiftCatalogue => self.on_gifts(&frame),
            Opcode::GiftSend => self.on_gift_sent(&frame).await,
            Opcode::Search | Opcode::Suggestions => self.on_people(&frame),
            // Everything else is either an acknowledgement with nothing to show or a feature this
            // client did not negotiate.
            _ => {}
        }
    }

    async fn on_ping(&mut self, frame: &migo_protocol::Frame) {
        let Ok(ping) = gateway::decode::<migo_protocol::Ping>(frame) else {
            return;
        };
        let pong = migo_protocol::Pong {
            client_time: ping.client_time,
            server_time: Timestamp::now(),
        };
        if let Some(gateway) = self.gateway.as_mut() {
            let correlation = frame.header.correlation;
            let _ = gateway.send(Opcode::Ping, correlation, &pong).await;
        }
    }

    fn on_message(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::MessageEvent>(frame) else {
            return;
        };
        let Some(message) = self.decrypt(&event) else {
            return;
        };
        self.sink.send(Event::Message(message));
    }

    fn on_accepted(&mut self, frame: &migo_protocol::Frame) {
        let Ok(accepted) = gateway::decode::<migo_protocol::MessageAccepted>(frame) else {
            return;
        };
        self.sink.send(Event::Accepted {
            message_id: accepted.message_id,
            conversation_id: accepted.conversation_id,
            seq: accepted.seq,
        });
    }

    async fn on_conversations(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::ConversationListResponse>(frame) else {
            return;
        };
        let me = self.signed.as_ref().map(|signed| signed.account.account_id);
        let mut out = Vec::with_capacity(response.conversations.len());
        // The peers whose names a direct conversation's title is drawn from. Gathered across the
        // whole list so one PROFILE_FETCH titles every direct chat, rather than one fetch per
        // row — the list arrives all at once, so the fetch is per-list.
        let mut peers: Vec<Id> = Vec::new();
        for summary in response.conversations {
            let members = summary.members.clone().unwrap_or_default();
            // A direct conversation's title is the peer's name and comes from a profile lookup
            // (the server carries no `title` for it), so those are the members worth naming.
            let untitled = summary.title.as_ref().is_none_or(|t| t.is_empty());
            if untitled && members.len() == 2 {
                if let Some(me) = me {
                    if let Some(peer) = members.iter().find(|id| **id != me) {
                        peers.push(*peer);
                    }
                }
            }
            if let Some(signed) = self.signed.as_mut() {
                signed
                    .members
                    .insert(summary.conversation_id, members.clone());
            }
            let preview = summary
                .last_message
                .as_ref()
                .and_then(|event| self.decrypt(event))
                .map(|message| message.body.preview());
            out.push(Conversation {
                conversation_id: summary.conversation_id,
                title: summary.title,
                members,
                encrypted: summary.encryption == EncryptionMode::EndToEnd,
                last_seq: summary.last_seq,
                preview,
                updated_at: summary.last_message.as_ref().map(|event| event.created_at),
                unread: u32::try_from(summary.last_seq.saturating_sub(summary.read_seq))
                    .unwrap_or(u32::MAX),
            });
        }
        self.sink.send(Event::Conversations(out));
        self.fetch_profiles(peers).await;
    }

    async fn on_conversation_created(&mut self, frame: &migo_protocol::Frame) {
        let Ok(summary) = gateway::decode::<migo_protocol::ConversationSummary>(frame) else {
            return;
        };
        if let Some(signed) = self.signed.as_mut() {
            signed.members.insert(
                summary.conversation_id,
                summary.members.clone().unwrap_or_default(),
            );
        }
        self.request_conversations().await;
    }

    fn on_history(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::SyncResponse>(frame) else {
            return;
        };
        let messages: Vec<Message> = response
            .messages
            .iter()
            .filter_map(|event| self.decrypt(event))
            .collect();
        self.sink.send(Event::History {
            conversation_id: response.conversation_id,
            messages,
        });
    }

    fn on_bundles(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::KeyBundleResponse>(frame) else {
            return;
        };
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        for wire in response.bundles {
            let bundle = crate::crypto::session::bundle_from_wire(
                &wire.identity_key,
                wire.signed_prekey_id,
                &wire.signed_prekey,
                &wire.signed_prekey_signature,
                match (wire.one_time_prekey_id, wire.one_time_prekey.as_ref()) {
                    (Some(id), Some(bytes)) => Some((id, bytes.as_slice())),
                    // A device out of one-time prekeys still gets a session, just without the fourth
                    // DH input. Refusing to talk to it would be worse than the forward-secrecy loss
                    // for that one first message.
                    _ => None,
                },
            );
            match bundle {
                Ok(bundle) => {
                    signed.bundles.insert(wire.device_id, bundle);
                    let devices = signed.devices.entry(wire.user_id).or_default();
                    if !devices.contains(&wire.device_id) {
                        devices.push(wire.device_id);
                    }
                }
                Err(_) => self.sink.toast(
                    "a key bundle from the server did not verify; not sending to that device",
                    ToastKind::Error,
                ),
            }
        }
    }

    fn on_typing(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::TypingEvent>(frame) else {
            return;
        };
        let Some(user_id) = event.user_id else { return };
        self.sink.send(Event::Typing {
            conversation_id: event.conversation_id,
            user_id,
            typing: event.state == migo_protocol::TypingState::Start,
        });
    }

    fn on_profiles(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::ProfileResponse>(frame) else {
            return;
        };
        let mut names = HashMap::with_capacity(response.profiles.len());
        for profile in &response.profiles {
            let name = if profile.display_name.is_empty() {
                profile.username.clone()
            } else {
                profile.display_name.clone()
            };
            // Presence rides the same answer the names do: the profile row is the server's own
            // statement of where the account stood when it was read, which is the best seed a
            // presence UI can get before any event arrives. `Unknown` is skipped rather than
            // sent — an event that says "never heard" is no event at all.
            if let Some(state) = profile.presence {
                let state = model::Presence::from_wire(state.to_wire());
                if state != model::Presence::Unknown {
                    self.sink.send(Event::PresenceChanged {
                        user_id: profile.user_id,
                        state,
                    });
                }
            }
            names.insert(profile.user_id, name);
        }
        self.sink.send(Event::Names(names));
    }

    /// The relationship list came back: reduce it to model rows, then ask for the two things a
    /// list of ids cannot show on its own — the names and the live presence of those accounts.
    async fn on_relationships(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::RelationshipList>(frame) else {
            return;
        };
        let entries: Vec<Relationship> = response
            .entries
            .into_iter()
            .map(|entry| Relationship {
                user_id: entry.user_id,
                kind: RelationshipKind::from_wire(entry.kind),
            })
            .collect();
        // Names for every edge, presence watches only for friendships: a pending request has no
        // presence to show (the dot would say "stranger is offline" at best), and a block is
        // exactly the account whose whereabouts this client must stop asking about.
        let named: Vec<Id> = entries.iter().map(|entry| entry.user_id).collect();
        let friends: Vec<Id> = entries
            .iter()
            .filter(|entry| entry.kind == RelationshipKind::Friend)
            .map(|entry| entry.user_id)
            .collect();
        self.sink.send(Event::Relationships(entries));
        self.fetch_profiles(named).await;
        self.watch_users(friends).await;
    }

    /// A FRIEND_REQUEST or FRIEND_RESPOND was accepted by the server: the graph moved, so the
    /// list the UI holds is stale whatever the specifics were.
    async fn on_social_ack(&mut self, frame: &migo_protocol::Frame) {
        let Ok(acknowledged) = gateway::decode::<migo_protocol::Acknowledged>(frame) else {
            return;
        };
        if acknowledged.ok {
            self.request_relationships().await;
        }
    }

    /// The other side of a social event this account was the audience of.
    ///
    /// The state string is a hint, not a source of truth (the server's own doc says so): the UI
    /// learns the new shape of the graph from the re-read that follows, and this event exists to
    /// say *that* something happened. An unknown state is still a graph movement, so it still
    /// carries an event — just one with nothing to claim about what moved.
    fn on_friend_event(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::FriendEvent>(frame) else {
            return;
        };
        let accepted = match event.state.as_str() {
            "request" => Some(false),
            "accepted" => Some(true),
            _ => None,
        };
        self.sink.send(Event::FriendChanged {
            user_id: event.user_id,
            accepted,
        });
    }

    /// A presence event off a subscribed user topic.
    fn on_presence(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::PresenceEvent>(frame) else {
            return;
        };
        let state = model::Presence::from_wire(event.state.to_wire());
        if state == model::Presence::Unknown {
            return;
        }
        self.sink.send(Event::PresenceChanged {
            user_id: event.user_id,
            state,
        });
    }

    /// Decrypts one MESSAGE_EVENT into something the UI can show.
    ///
    /// A message that will not decrypt becomes [`Body::Undecryptable`] rather than disappearing. A
    /// silently dropped message is indistinguishable from one that was never sent, and the gap in the
    /// sequence numbers would go unexplained; a visible placeholder tells the user something arrived
    /// that this device cannot read, which is the truth.
    fn decrypt(&mut self, event: &migo_protocol::MessageEvent) -> Option<Message> {
        let signed = self.signed.as_mut()?;
        let outgoing = event.sender_id == signed.account.account_id;
        let mine = event.sender_device == signed.account.device_id;

        let body = if mine {
            // Our own message, echoed back. The ratchet cannot open what it sealed, so there is
            // nothing to decrypt — the UI already has this text from the optimistic insert.
            Body::Text(String::new())
        } else {
            match Envelope::decode(&event.envelope)
                .and_then(|envelope| signed.sessions.open(event.sender_device, &envelope))
                .map_err(|error| error.to_string())
                .and_then(|plaintext| {
                    content::decode(&plaintext).map_err(|_| "unreadable content".to_owned())
                }) {
                Ok(content) => body_of(content),
                Err(reason) => Body::Undecryptable(reason),
            }
        };

        if mine {
            // Suppress the echo entirely: ACCEPTED already moved the message from sending to sent.
            return None;
        }

        Some(Message {
            message_id: event.message_id,
            conversation_id: event.conversation_id,
            seq: event.seq,
            sender_id: event.sender_id,
            outgoing,
            body,
            sent_at: event.created_at,
            delivery: Delivery::Received,
        })
    }

    /// Handles a lost connection: drop the socket, report it, arm the retry.
    ///
    /// Deliberately synchronous and deliberately short. The waiting and the retrying happen in
    /// [`Self::reconnect`], driven by the select loop, so this can be called from the middle of a send
    /// path without that path having to await a reconnection it did not ask for.
    fn on_disconnect(&mut self, error: GatewayError) {
        self.gateway = None;
        if self.signed.is_none() {
            self.retry = None;
            self.sink.send(Event::Connection(Connection::Offline));
            return;
        }
        self.sink
            .send(Event::Connection(Connection::Failed(error.to_string())));
        // Only arm a fresh schedule if none is running; a second failure mid-backoff must not reset
        // the delay back to the base and turn the backoff into a busy loop.
        if self.retry.is_none() {
            self.retry = Some(Retry::first(&mut OsRandom));
        }
    }

    /// Reports a failure that left the client signed out.
    fn fail(&mut self, text: String) {
        self.sink
            .send(Event::Connection(Connection::Failed(text.clone())));
        self.sink.toast(text, ToastKind::Error);
    }
}

/// Projects decrypted [`Content`] onto the UI's [`Body`].
fn body_of(content: Content) -> Body {
    match content {
        Content::Text { text, .. } => Body::Text(text),
        Content::MediaRef {
            mime_type,
            size_bytes,
            ..
        } => Body::Media {
            mime_type,
            size_bytes,
        },
        Content::VoiceNoteRef { duration_ms, .. } => Body::VoiceNote { duration_ms },
        Content::Reaction {
            emoji,
            target_message_id,
            ..
        } => Body::Reaction {
            emoji,
            target: target_message_id,
        },
        Content::ControlEvent { .. } => Body::Unsupported { content_type: 5 },
        Content::Unsupported { content_type } => Body::Unsupported { content_type },
    }
}
