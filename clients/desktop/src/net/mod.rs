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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use migo_core::{Id, OsRandom, Random, Timestamp};
use migo_protocol::{features, ClientInfo, ConversationKind, EncryptionMode, MessageKind, Opcode};
use tokio::sync::mpsc;

use crate::config::ServerEndpoint;
use crate::crypto::content::{self, Content};
use crate::crypto::envelope::Envelope;
use crate::crypto::session::{DeviceKeys, SessionStore, ONE_TIME_PREKEY_COUNT};
use crate::model::{self, Account, Body, Connection, Conversation, Delivery, Message, ToastKind};
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
    /// Report typing state. Best effort; dropped silently when offline.
    Typing { conversation_id: Id, typing: bool },
    /// Mark everything up to `seq` as read.
    MarkRead { conversation_id: Id, seq: u64 },
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
}

impl Worker {
    fn new(sink: Sink, vault_path: PathBuf) -> Self {
        Self {
            sink,
            vault_path,
            signed: None,
            gateway: None,
            retry: None,
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
                self.request_conversations().await;
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
            Opcode::ConversationList => self.on_conversations(&frame),
            Opcode::ConversationCreate => self.on_conversation_created(&frame).await,
            Opcode::Sync => self.on_history(&frame),
            Opcode::KeyBundleFetch => self.on_bundles(&frame),
            Opcode::Typing => self.on_typing(&frame),
            Opcode::ProfileFetch => self.on_profiles(&frame),
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

    fn on_conversations(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::ConversationListResponse>(frame) else {
            return;
        };
        let mut out = Vec::with_capacity(response.conversations.len());
        for summary in response.conversations {
            let members = summary.members.clone().unwrap_or_default();
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
        let names = response
            .profiles
            .into_iter()
            .map(|profile| {
                let name = if profile.display_name.is_empty() {
                    profile.username
                } else {
                    profile.display_name
                };
                (profile.user_id, name)
            })
            .collect();
        self.sink.send(Event::Names(names));
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
