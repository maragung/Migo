//! The chat screen: one conversation's window — header, thread, composer.
//!
//! # Why the message store is a map, not a list
//!
//! Messages are held per conversation in a [`HashMap`], each vector sorted by sequence number, rather
//! than as one flat list filtered on every frame. A paint loop runs sixty times a second; filtering
//! a growing list that often is work that scales with the entire history to draw one screen of it.
//!
//! # Why insertion is a sorted merge
//!
//! Messages do not arrive in order. A live message can land before the history request that covers the
//! same range returns, the same message can arrive twice, and an outgoing message is inserted
//! optimistically with no sequence number at all and gains one later. So insertion deduplicates by
//! message id and keeps the vector ordered, rather than pushing and hoping. Anything less and the
//! thread visibly reorders itself while someone is reading it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use egui::{Align, Key, Layout, RichText, Ui};
use migo_core::Id;
use migo_protocol::ConversationRole;

use crate::model::{self, Body, Conversation, Delivery, Message};
use crate::net::Command;
use crate::theme::{font, palette, space};
use crate::ui::widgets::{self, BubbleTone};
use crate::ui::{ChatLogAction, Context};

/// The emoji a reaction picker offers — the same three the web and Android clients offer, in
/// the same order, so the same thread shows the same vocabulary on every screen it is read
/// on.
pub const REACTIONS: [&str; 3] = ["\u{1F44D}", "\u{2764}\u{FE0F}", "\u{1F602}"];

/// Everything the chat screen holds between frames.
#[derive(Default)]
pub struct ChatState {
    /// Conversations, most recently active first.
    pub conversations: Vec<Conversation>,
    /// Which conversation is open.
    pub selected: Option<Id>,
    /// Messages per conversation, each vector sorted by sequence number.
    pub messages: HashMap<Id, Vec<Message>>,
    /// Display names for account ids, so a direct conversation can be titled by the other person.
    pub names: HashMap<Id, String>,
    /// Who is currently typing, per conversation.
    pub typing: HashMap<Id, Vec<Id>>,
    /// The composer's contents, per conversation. A conversation is a window of its own now
    /// (see [`crate::ui::desktop`]), so a draft belongs to the conversation it was typed into —
    /// switching windows must not carry half a sentence from one thread into another, and
    /// closing a window must not cost the words someone was still composing in it.
    pub drafts: HashMap<Id, String>,
    /// The open inline edit, per message id — at most one is open anywhere, because the map
    /// holds the message being fixed, but keying by id (rather than holding a bare Option)
    /// lets a closed editor remove itself without knowing which conversation's thread drew it.
    pub edits: HashMap<Id, EditDraft>,
    /// The accounts this caller has personally muted, as the relationship graph last read
    /// them — the set the muted-provider owns on the web, kept here because a chat window
    /// filters rooms against it and the friends pane is not always mounted.
    ///
    /// Room chatter only, never direct messages: a mute is a volume control for the noise of
    /// a crowd, and the person muted in a room can still be heard one to one.
    pub muted: HashSet<Id>,
    /// The last typing state reported, per conversation, so a keystroke does not send one frame
    /// per character. Keyed the same way the drafts are, for the same reason.
    pub typing_sent: HashMap<Id, bool>,
    /// True until the first message that must be scrolled into view has been.
    pub scroll_to_end: bool,
    /// Room membership notices, keyed by room id: who came, who went, who was shown the door.
    ///
    /// A live tail, not history — the same cap the web and Android clients keep — reset when the
    /// room changes. The name a line reads out is resolved at draw time from `names`, the way a
    /// typing line's is, so a display name that arrives late still lands on the line.
    pub room_notices: HashMap<Id, Vec<RoomNotice>>,
    /// The highest sequence number a peer has read, per conversation: another member's Read
    /// watermark, the same `readUpTo` the web and Android clients track.
    ///
    /// Own receipts never land here (the net layer drops them), so the value is always someone
    /// else's read — which is exactly what an outgoing message's read marker claims. Monotonic:
    /// a receipt is a watermark, so an older one arriving late never drags it backwards.
    pub read_up_to: HashMap<Id, u64>,
    /// How many times a conversation has been opened, ever. Incremented by [`open`] and read by
    /// nobody but the shell, which compares it across a frame: a conversation can be opened from
    /// places the shell does not control (a room row, a search hit), so this is the one honest
    /// signal that *some* conversation asked to become a window during the frame.
    pub open_seq: u64,
    /// The observed E2EE identity of every peer device the worker has reported, keyed by
    /// account — §47/§164's verification surface, filed as the key-bundle fetches answer.
    ///
    /// One row per *device*: the identity key a bundle carries belongs to a device, so a peer
    /// with a phone and a laptop shows two safety numbers, and both have to be verified with the
    /// person. Each number is the *pair* number — this device's and their device's keys in one
    /// value — so the same string is on the peer's own screen for this device, whatever client
    /// they hold. Empty until a bundle fetch answers; the private thread draws its block only
    /// then, because a placeholder that promised "safe" before any key was seen would be the
    /// opposite of the block's purpose.
    pub peers: HashMap<Id, Vec<PeerIdentity>>,
    /// The attachments' own state: decoded images, their textures, what has been asked for,
    /// what failed, and which voice note is playing.
    pub media: MediaState,
    /// Reactions by target message: who reacted, with what. Chips on the message they name,
    /// never rows of their own — a reaction is a fact about another message, and drawing it
    /// as one would bury the thing it answers.
    pub reactions: HashMap<Id, Vec<(Id, String)>>,
    /// The conversation being recorded into, and when the recording began. `None` when no
    /// recording runs. The conversation id rides along so a bar left behind by a window
    /// switch clears when its own conversation's recording ends, not whichever one is open.
    pub recording: Option<(Id, std::time::Instant)>,
    /// The attach panel's own state, per conversation: whether it is open and the path typed
    /// into it. egui offers no file dialog, so the path is typed — the same trade the avatar
    /// picker makes — and it is kept per conversation the way drafts are.
    pub attach: HashMap<Id, AttachPanel>,
    /// The transcript panel's own state, per conversation, keyed the way the attach panel is:
    /// the floppy in the header folds out a typed-path save row under it, and the path is kept
    /// between frames so closing the row on a mistake does not cost the whole path.
    pub log_panels: HashMap<Id, LogPanel>,
    /// The thread search's own state, per conversation, keyed the way the attach panel and the
    /// transcript panel are: the magnifier in the header folds a live filter row out under it,
    /// and the query is kept per conversation the way a draft is, so switching windows and
    /// back does not cost the words already typed.
    pub searches: HashMap<Id, SearchPanel>,
    /// Group rosters, per conversation: the panel the header's roster button folds out. The
    /// worker asks the wire for it on demand; `None` means "not asked yet", and the panel's
    /// first open is the ask.
    pub rosters: HashMap<Id, Vec<crate::model::RosterMember>>,
    /// The new-group form, when the friends pane's "+ Group" left it open. One at a time on
    /// the whole screen — the form is a dialog the friends pane draws, and two forms would be
    /// one group's members picked into another's.
    pub new_group: Option<NewGroupForm>,
    /// The roster panel's open flag, per conversation, kept the way the attach and search
    /// panels are kept: the header's people button folds the roster out under it, and the
    /// flag is the conversation's own so switching windows closes nothing.
    pub roster_open: HashMap<Id, bool>,
    /// The rename row's state, per conversation: the founder's rename affordance in the roster
    /// panel folds a typed field out under it, and the draft is kept the way every other
    /// per-conversation draft is.
    pub renames: HashMap<Id, RenamePanel>,
    /// The invite row's state, per conversation: every member's invite affordance, folding a
    /// typed account-id field out under the roster, one name at a time — the web panel's
    /// username search is a REST surface the gateway path does not offer, so the desktop's
    /// invitation is an account id the person already knows.
    pub invites: HashMap<Id, InvitePanel>,
    /// A running kick vote's tally, per conversation: the newest tally the wire sent. The
    /// panel draws it under the roster; a closed tally is dropped rather than kept, because a
    /// question the server has stopped asking is not a fact to render.
    pub votes: HashMap<Id, crate::model::VoteTally>,
    /// Group membership notices, keyed by conversation — the group twin of the room notices,
    /// the same live tail, the same cap, the same draw at the scroll's end.
    pub group_notices: HashMap<Id, Vec<RoomNotice>>,
    /// The conversations whose composer is armed for disappearing sends — the web's
    /// `expiresAfterMs` state, held as a set because the desktop offers the one lifetime the
    /// web's `DISAPPEARING_MS` fixes. Armed is per conversation: the promise is about a
    /// thread's future, not the screen's, so a window switch does not carry an arm from one
    /// person's chat into another's. Rooms never hold an arm — a room is server-readable, so
    /// its transcripts are the server's memory, not a promise a sender can make.
    pub disappearing: HashSet<Id>,
}

/// The thread search's state for one conversation.
#[derive(Default)]
pub struct SearchPanel {
    /// Whether the search row is showing under the header.
    pub open: bool,
    /// The query as typed, kept between frames so closing the row on a mistake does not cost
    /// the words — the same patience the attach panel's path and the transcript row's path
    /// are given.
    pub query: String,
    /// One frame's flag: the magnifier just opened the row, and the field asks for focus the
    /// first frame it is drawn — the web client's field autofocuses, and a search opened
    /// without the cursor in it is a question half-asked.
    pub claim_focus: bool,
}

/// The new-group form's state, as the friends pane draws it.
#[derive(Default)]
pub struct NewGroupForm {
    /// The group's title, as typed.
    pub title: String,
    /// The friends picked as founding members, in pick order.
    pub picked: Vec<Id>,
    /// The account id typed into the manual field, for the friend the list does not show.
    pub manual: String,
    /// One frame's flag: the form just opened, and the title field asks for focus.
    pub claim_focus: bool,
}

/// The rename row's state for one conversation.
#[derive(Default)]
pub struct RenamePanel {
    /// Whether the row is showing in the roster panel.
    pub open: bool,
    /// The title as typed, seeded from the conversation's current title when the row opens.
    pub title: String,
}

/// The invite row's state for one conversation.
#[derive(Default)]
pub struct InvitePanel {
    /// Whether the row is showing in the roster panel.
    pub open: bool,
    /// The account id as typed.
    pub account_id: String,
}

/// The transcript panel's state for one conversation.
#[derive(Default)]
pub struct LogPanel {
    /// Whether the save row is showing under the header.
    pub open: bool,
    /// The path typed into it.
    pub path: String,
}

/// One fetched image, as the worker decoded it: the pixels and their size.
///
/// The blob arrives once and the texture is built from it lazily, because a texture is a GPU
/// resource whose lifetime belongs to the paint loop, not to the event that produced the
/// pixels.
pub struct ImageBlob {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The attach panel's state for one conversation.
#[derive(Default)]
pub struct AttachPanel {
    /// Whether the panel is showing under the composer.
    pub open: bool,
    /// The path typed into it, kept between frames so closing the panel on a mistake does
    /// not cost the whole path.
    pub path: String,
}

/// The attachments' state between frames.
#[derive(Default)]
pub struct MediaState {
    /// Decoded images by media id, from the worker's `MediaImage` events.
    pub images: HashMap<Id, ImageBlob>,
    /// The GPU textures built from those blobs, one per image, built the first frame an
    /// image is drawn and dropped when a re-fetch replaces the blob (or sign-out clears
    /// the whole chat state).
    pub textures: HashMap<Id, egui::TextureHandle>,
    /// Media ids a fetch has been issued for, so a thread that draws the same bubble on
    /// every frame asks once, not sixty times a second.
    pub requested: HashSet<Id>,
    /// Fetch failures by media id, drawn in the bubble they belong to.
    pub failures: HashMap<Id, String>,
    /// The voice note currently playing, if one is.
    pub playing: Option<Id>,
    /// Save destinations typed against a document bubble, kept between frames so a retry of
    /// a failed save does not cost the path.
    pub save_paths: HashMap<Id, String>,
}

/// One peer device's observed E2EE identity, as the worker reported it from a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The device the identity key belongs to.
    pub device_id: Id,
    /// The pair safety number: this device's and that device's identity keys in one number,
    /// already grouped for reading aloud. The same string the peer's screen shows for this
    /// device, which is what makes reading it to each other a meaningful check.
    pub safety_number: String,
    /// Whether the identity key differed from the last one this vault sealed for the device
    /// when it was observed. Sticky for the session once set: the sentence it draws — verify
    /// before trusting — stays true until the person has actually done it.
    pub changed: bool,
}

/// One room membership line in the thread's tail.
#[derive(Debug, Clone)]
pub struct RoomNotice {
    /// Who the change is about.
    pub user_id: Id,
    /// The sentence, minus the name: "joined the room", "disconnected", and the rest.
    pub verb: &'static str,
    /// Arrival order, stable across the repaint loop.
    pub seq: u64,
}

/// How many recent membership changes a room keeps on screen at once. Public because the app's
/// event handler trims to it when a notice arrives — the same bound, written in one place.
pub const MAX_ROOM_NOTICES: usize = 50;

impl ChatState {
    /// Replaces the conversation list, keeping the open conversation selected if it survived.
    pub fn set_conversations(&mut self, conversations: Vec<Conversation>) {
        self.conversations = conversations;
        // Most recent first. The server returns them ordered, but a locally created conversation is
        // spliced in before the next list arrives, so the order is asserted here rather than assumed.
        self.conversations.sort_by(|a, b| {
            b.updated_at
                .map(|t| t.as_millis())
                .unwrap_or(0)
                .cmp(&a.updated_at.map(|t| t.as_millis()).unwrap_or(0))
        });
        if let Some(open) = self.selected {
            if !self.conversations.iter().any(|c| c.conversation_id == open) {
                self.selected = None;
            }
        }
    }

    /// Inserts or updates one message, keeping the thread ordered and free of duplicates.
    pub fn absorb(&mut self, message: Message) {
        // A reaction is not a row: it is a chip on the message it names. Filed here — the
        // one gate every message passes through, live and history both — and answered with
        // an early return, so a reaction never scrolls the thread, never marks it unread,
        // and never renders as a message of its own. The pair `(sender, emoji)` is the
        // dedupe key: the same sender's same emoji twice (an echo racing a re-fetch) is one
        // chip, while a second emoji from the same sender is a second chip, which is what
        // every other client shows.
        if let Body::Reaction { emoji, target } = &message.body {
            let chips = self.reactions.entry(*target).or_default();
            let pair = (message.sender_id, emoji.clone());
            if !chips.contains(&pair) {
                chips.push(pair);
            }
            return;
        }
        let thread = self.messages.entry(message.conversation_id).or_default();
        if let Some(existing) = thread
            .iter_mut()
            .find(|m| m.message_id == message.message_id)
        {
            // Same message again. Keep the higher sequence number and the further-along delivery state:
            // the optimistic insert has seq 0 and `Sending`, and the server's answer has the real seq.
            // Overwriting wholesale would let a re-delivered event drag a `Sent` tick back to
            // `Sending`.
            if message.seq > existing.seq {
                existing.seq = message.seq;
            }
            if delivery_rank(message.delivery) > delivery_rank(existing.delivery) {
                existing.delivery = message.delivery;
            }
            if !matches!(message.body, Body::Text(ref t) if t.is_empty()) {
                existing.body = message.body;
            }
            // A tombstone or an edit stamp is never withdrawn by a later echo: once the
            // sender has pulled a message back or fixed it, every later delivery of the
            // same id still says so.
            if message.deleted {
                existing.deleted = true;
            }
            if message.edited {
                existing.edited = true;
            }
            // A deadline, once learned, is kept: the optimistic insert stamps it from this
            // clock, and the edit's echo re-seals the original's lifetime, so a later
            // delivery agreeing is the ordinary case and a `None` from an un-opened echo
            // must not un-promise what the row already holds.
            if message.expires_at.is_some() {
                existing.expires_at = message.expires_at;
            }
            return;
        }
        thread.push(message);
        thread.sort_by_key(|m| (m.seq, m.sent_at.as_millis()));
        self.scroll_to_end = true;
    }

    /// The local half of a disappearing message: the drop when a sealed lifetime passes.
    ///
    /// The server sweeps its own store on a one-minute tick, but the deadline is the client's
    /// to honour — the sweeper publishes nothing, so a client that waited for the server
    /// would show a message a full minute past the moment it promised to vanish. Each row's
    /// deadline was computed at decrypt (or at the optimistic insert) from the lifetime the
    /// sender sealed inside the content, because the wire never echoes it back.
    ///
    /// The row is removed outright, not tombstoned: unlike a deletion — which names a message
    /// that may still be unread, so its place in the transcript is kept — an expiry is the
    /// message saying it never wanted to be remembered. The seq numbering gains the same gap
    /// a hard purge leaves, and the sync path's truncation handling already treats a gap as
    /// honest.
    ///
    /// Returns whether anything was dropped, so the caller can repaint on the drop and coast
    /// between; a quiet thread costs one scan per tick.
    pub fn sweep_expired(&mut self) -> bool {
        let now = migo_core::Timestamp::now();
        let mut dropped = false;
        for thread in self.messages.values_mut() {
            let before = thread.len();
            thread.retain(|message| message.expires_at.is_none_or(|deadline| deadline > now));
            if thread.len() != before {
                dropped = true;
            }
        }
        dropped
    }

    /// Merges a page of history.
    pub fn absorb_history(&mut self, conversation_id: Id, messages: Vec<Message>) {
        for message in messages {
            let mut message = message;
            message.conversation_id = conversation_id;
            self.absorb(message);
        }
    }

    /// Marks an outgoing message as accepted by the server.
    pub fn accept(&mut self, conversation_id: Id, message_id: Id, seq: u64) {
        if let Some(thread) = self.messages.get_mut(&conversation_id) {
            if let Some(message) = thread.iter_mut().find(|m| m.message_id == message_id) {
                message.seq = seq;
                message.delivery = Delivery::Sent;
            }
            thread.sort_by_key(|m| (m.seq, m.sent_at.as_millis()));
        }
    }

    /// Marks an outgoing message as failed.
    pub fn reject(&mut self, message_id: Id) {
        for thread in self.messages.values_mut() {
            if let Some(message) = thread.iter_mut().find(|m| m.message_id == message_id) {
                message.delivery = Delivery::Failed;
            }
        }
    }

    /// Folds a peer's Read watermark: everything they have read, up to `seq`.
    ///
    /// Monotonic, because a receipt is a cumulative claim — "I have read through here" — so a
    /// late-arriving older watermark must not undo a newer one. The marker an outgoing row draws
    /// reads this, the same way the web client's `readUpTo` does.
    pub fn note_read(&mut self, conversation_id: Id, seq: u64) {
        let watermark = self.read_up_to.entry(conversation_id).or_insert(seq);
        if seq > *watermark {
            *watermark = seq;
        }
    }

    /// The highest sequence number held for a conversation, for the next sync request.
    pub fn have_seq(&self, conversation_id: Id) -> u64 {
        self.messages
            .get(&conversation_id)
            .and_then(|thread| thread.iter().map(|m| m.seq).max())
            .unwrap_or(0)
    }

    /// Files one observed peer device: an update for a device already known, a new row
    /// otherwise, ordered by device id so the verification block reads the same from one frame
    /// to the next.
    ///
    /// The change flag is sticky (`|=`), never cleared by a re-observation: the worker reports
    /// a change once, when the fingerprint first differs, and a window closed and reopened
    /// mid-change would otherwise drop the warning while the old number was still on somebody's
    /// screen.
    pub fn note_peer_identity(
        &mut self,
        user_id: Id,
        device_id: Id,
        safety_number: String,
        changed: bool,
    ) {
        let devices = self.peers.entry(user_id).or_default();
        match devices
            .iter_mut()
            .find(|device| device.device_id == device_id)
        {
            Some(known) => {
                known.safety_number = safety_number;
                known.changed |= changed;
            }
            None => {
                devices.push(PeerIdentity {
                    device_id,
                    safety_number,
                    changed,
                });
                devices.sort_unstable_by_key(|device| device.device_id);
            }
        }
    }
}

/// Orders delivery states so a later one never overwrites an earlier one.
fn delivery_rank(state: Delivery) -> u8 {
    match state {
        Delivery::Failed => 0,
        Delivery::Sending => 1,
        Delivery::Sent => 2,
        Delivery::Received => 3,
    }
}

/// One conversation's window: header, messages, composer, with nothing beside them.
///
/// The conversation is passed in explicitly rather than read from the state's `selected`,
/// because a conversation is a window of its own now — several are on the desktop at once, and
/// this thread is called once per open window with the id that window was minted for. The
/// `selected` field is the *last* conversation any door opened, not the one being drawn, and
/// reading it here would make every window show whichever thread was opened most recently.
pub fn thread(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
    // The disappearing sweep, once per second: the deadline a sealed lifetime set is this
    // client's to honour, so a repaint cadence is asked for and the drop checked before the
    // thread borrows its messages. Called from `thread` rather than each window's pane
    // because the promise spans every conversation at once — a message expiring in a window
    // nobody has open must still leave the conversation list's preview and the store.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs(1));
    state.sweep_expired();
    thread_pane(ui, context, state, conversation_id);
}

/// Opens a conversation and asks for anything missing from its history.
///
/// Public because the shell's other places are doors into threads too: a Home digest row, a
/// joined room, a search hit. All of them open a conversation the one way there is.
///
/// The draft is deliberately left alone: drafts are per conversation now, so opening is not
/// writing — the words someone had half-composed are still there when the window comes back.
pub fn open(context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
    state.selected = Some(conversation_id);
    state.open_seq = state.open_seq.saturating_add(1);
    state.scroll_to_end = true;
    context.issue(Command::History {
        conversation_id,
        have_seq: state.have_seq(conversation_id),
    });

    // Report the read watermark on open rather than on scroll. A read receipt is a disclosure about
    // the reader, so it is sent when they have actually opened the conversation and not merely because
    // a row scrolled past.
    let seq = state
        .conversations
        .iter()
        .find(|c| c.conversation_id == conversation_id)
        .map(|c| c.last_seq)
        .unwrap_or(0);
    if seq > 0 {
        context.issue(Command::MarkRead {
            conversation_id,
            seq,
        });
    }
    if let Some(conversation) = state
        .conversations
        .iter_mut()
        .find(|c| c.conversation_id == conversation_id)
    {
        conversation.unread = 0;
    }

    // A private conversation's window asks for the peer's key bundles the moment it opens, so
    // their safety numbers are on screen before anything is sent — a verification surface that
    // arrives after the conversation has started is a surface nobody looks at. The bundle
    // response is also where a changed identity key is noticed, and that warning belongs to the
    // window the conversation opens with, not to the first send.
    if let (Some(me), Some(conversation)) = (
        context.account.map(|account| account.account_id),
        state
            .conversations
            .iter()
            .find(|c| c.conversation_id == conversation_id),
    ) {
        if conversation.encrypted && conversation.members.len() == 2 {
            if let Some(peer) = conversation.members.iter().find(|id| **id != me) {
                context.issue(Command::PeerKeys { user_id: *peer });
            }
        }
    }
}

/// The open conversation: header, messages, composer — with the composer pinned.
///
/// The composer is laid out as a *bottom panel inside the window*, claimed before the thread
/// is drawn, so it sits on the window's bottom edge whatever the history above it is doing. A
/// composer laid out in sequence after the messages lands wherever the messages ended:
/// mid-window on a short history, and below the window's bottom edge once a long draft grew
/// the multiline field past the height a sequential layout had reserved for it. The panel
/// settles the split by measurement every frame instead — the composer takes exactly what it
/// needs, and the thread's scroll takes everything that remains, which is the whole Growing
/// Area a chat window owes its history.
fn thread_pane(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
    thread_header(ui, context, state, conversation_id);
    widgets::divider(ui, context.theme);

    // The floppy's fold-out, before anything claims the window's edges: a save row that hands
    // the write to the shell (the write is a `ChatLogAction`, not a command — the disk is not
    // the worker's), drawn only while the header's button left it open.
    let log_open = state
        .log_panels
        .get(&conversation_id)
        .is_some_and(|panel| panel.open);
    if log_open {
        transcript_save_row(ui, context, state, conversation_id);
    }

    // The search's fold-out, beside the transcript row it shares the header with.
    let search_open = state
        .searches
        .get(&conversation_id)
        .is_some_and(|panel| panel.open);
    if search_open {
        search_row(ui, context, state, conversation_id);
    }

    // The roster's fold-out: the group's people, their roles, and the levers a member or a
    // founder holds. Drawn only for a group and only while the header's people button left
    // it open; the panel's own first draw is the roster read's ask.
    let roster_open = state
        .roster_open
        .get(&conversation_id)
        .copied()
        .unwrap_or(false);
    if roster_open {
        group_roster_panel(ui, context, state, conversation_id);
    }

    // The search's needle, taken as an owned value before the scroll area borrows `state`:
    // the thread's loop asks it per row with no borrow of the panel map behind it. `None` is
    // "no question asked" — an empty or whitespace query filters nothing, exactly as the web
    // client's field does — while a real needle that answers nothing is a question the
    // thread below answers with its own honest line.
    let needle = state
        .searches
        .get(&conversation_id)
        .filter(|panel| panel.open)
        .and_then(|panel| search_needle(&panel.query));

    // Read before the scroll area borrows `state`, and by member count rather than by conversation
    // kind: a group of two reads like a direct chat and should look like one.
    let conversation = state
        .conversations
        .iter()
        .find(|c| c.conversation_id == conversation_id);
    let group = conversation.is_some_and(|c| c.members.len() > 2);
    // A room, by the server's own word: the one conversation kind a personal mute filters.
    // Direct and group messages reach the person by name and are never muted out — the
    // muted set is a volume control for the noise of a crowd, not a door.
    let is_room = conversation.is_some_and(|c| c.room_id.is_some());
    // The monogram a direct chat's incoming bubbles carry: the peer's title, resolved the same
    // way the header resolves it so the two never disagree about who is who.
    let peer_seed = (!group)
        .then(|| {
            context
                .account
                .zip(conversation)
                .map(|(account, conversation)| {
                    conversation.display_title(account.account_id, &state.names)
                })
        })
        .flatten();

    // The bottom edge first: the typing line, then the composer, pinned. The typing line draws
    // itself only when someone is typing, so the composer is the panel's one permanent row.
    egui::Panel::bottom(egui::Id::new("chat-composer").with(conversation_id))
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            typing_line(ui, context, state, conversation_id);
            composer(ui, context, state, conversation_id, is_room);
        });

    // The thread: the growing area, in everything the window has left.
    egui::ScrollArea::vertical()
        .id_salt(("thread", conversation_id.to_string()))
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.add_space(space::MD);
            safety_block(ui, context, state, conversation_id);
            let empty = Vec::new();
            let thread = state.messages.get(&conversation_id).unwrap_or(&empty);
            if thread.is_empty() {
                widgets::empty_state(
                    ui,
                    context.theme,
                    "No messages yet",
                    "Anything you send here is encrypted on this device before it leaves.",
                );
            } else {
                let mut last_day: Option<String> = None;
                // How many rows the needle let through, for the "nothing matches" line below:
                // a filter that answers nothing must say so, because the thread's own empty
                // state ("No messages yet") would be a lie under one — there are messages,
                // none of them answer.
                let mut drawn = 0usize;
                for message in thread {
                    // The personal mute, before anything else: a muted account's room chatter
                    // is not drawn, the same rule the web client's muteFilter applies — and
                    // only in a room, because a direct message is the one place a muted
                    // person can still be heard.
                    if is_room && state.muted.contains(&message.sender_id) {
                        continue;
                    }
                    // The live filter, before the day label: a separator whose every message
                    // was filtered away is a rule over nothing, and the day a survivor
                    // belongs to is still drawn by the survivor itself.
                    if let Some(needle) = &needle {
                        if !body_matches(needle, &message.body) {
                            continue;
                        }
                    }
                    drawn += 1;
                    let day = day_label(message.sent_at);
                    if last_day.as_deref() != Some(day.as_str()) {
                        day_separator(ui, context, &day);
                        last_day = Some(day);
                    }
                    // Only in a conversation with more than two people, and only for messages someone
                    // else wrote. In a direct chat the header already names the one possible sender, and
                    // repeating it above every bubble is noise that pushes the text further apart.
                    let sender = (group && !message.outgoing).then(|| {
                        state
                            .names
                            .get(&message.sender_id)
                            .cloned()
                            .unwrap_or_else(|| model::short_id(message.sender_id))
                    });
                    // An avatar on the incoming side only. Outgoing bubbles are already anchored by
                    // their alignment and accent fill; a self-avatar beside them would be decoration.
                    let avatar_seed = if message.outgoing {
                        None
                    } else if group {
                        sender.as_deref()
                    } else {
                        peer_seed.as_deref()
                    };
                    // The read marker rides only outgoing messages with a server sequence: a peer's
                    // watermark claims "I read through N", which a message still Sending has no seq
                    // to be measured against yet.
                    let read = message.outgoing
                        && message.seq > 0
                        && state
                            .read_up_to
                            .get(&conversation_id)
                            .is_some_and(|mark| message.seq <= *mark);
                    // The media and reactions state is threaded through by field, beside the
                    // immutable `messages` borrow the loop reads: a bubble that fetches its
                    // image or files a reaction chip writes those maps while the thread is
                    // being walked, and the field-by-field borrow is what says the two never
                    // fight over the same data.
                    message_row(
                        ui,
                        context,
                        message,
                        sender.as_deref(),
                        avatar_seed,
                        read,
                        &mut state.media,
                        &mut state.reactions,
                        &mut state.edits,
                    );
                    ui.add_space(space::SM);
                }
                if needle.is_some() && drawn == 0 {
                    let colors = palette(context.theme);
                    ui.label(
                        RichText::new("No loaded message matches.")
                            .font(egui::FontId::proportional(font::SMALL))
                            .color(colors.text_muted),
                    );
                }
            }
            // The room's own life, as the scroll's final lines: who came, who went, who dropped.
            // A live tail, not history — the notices arrived while the room was open, in arrival
            // order, and a reader who wants the durable roster opens the rooms pane.
            room_notices(ui, context, state, conversation_id);
            // A group's own life, the same tail with the group's own verbs: who joined, who
            // left, who was removed or voted out. Keyed by the conversation, because a group
            // has no room id to key by.
            group_notices_tail(ui, context, state, conversation_id);
            ui.add_space(space::SM);
        });
}

/// The transcript save row, under the header, while the header's floppy left it open.
///
/// egui offers no file dialog, so the destination is typed — the same trade the attach panel's
/// fold-out and the document bubble's save row make, with the same `/path` hint. The write
/// itself never happens here: it is pushed as a [`ChatLogAction`] for the shell to apply after
/// the frame, because a layout closure that could reach the disk could block the paint loop on
/// a slow one, and the boundary is what keeps it structurally unable to.
fn transcript_save_row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatState,
    conversation_id: Id,
) {
    let colors = palette(context.theme);
    let panel = state.log_panels.entry(conversation_id).or_default();
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Save transcript")
                        .text_style(crate::theme::named(crate::theme::text_style::OVERLINE))
                        .color(colors.text_muted),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut panel.path)
                        .hint_text("/path/to/transcript.txt")
                        .desired_width(200.0),
                );
                let typed = panel.path.trim().to_owned();
                if widgets::primary_button(ui, context.theme, "Save", !typed.is_empty()).clicked() {
                    context.chat_log.push(ChatLogAction::SaveTranscript {
                        conversation_id,
                        path: PathBuf::from(typed),
                    });
                    // The row folds itself away on success the way the attach panel does: the
                    // write's outcome arrives as a toast, and a row still open under it would
                    // be the question twice.
                    panel.open = false;
                }
            });
        });
}

/// A thread-search query as the thread's filter asks it: trimmed and lowercased, or `None`
/// when nothing was really asked.
///
/// `None`, not an empty needle, is the distinction that matters: an empty or whitespace-only
/// query is "no question" (the thread draws unfiltered), while a real needle that matches
/// nothing is "a question with no answer" (the thread says so). Collapsing the two would make
/// the honest empty state impossible to draw — the web client's field makes the same cut,
/// filtering nothing until the trimmed query has length.
#[must_use]
fn search_needle(query: &str) -> Option<String> {
    let trimmed = query.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_lowercase())
}

/// Whether one message's body answers a prepared [`search_needle`].
///
/// Text matches, case-insensitively; nothing else does. A media caption is a label on an
/// object, not words someone wrote, and a voice note's words are not in the body at all — the
/// web client's filter makes the same cut, so the same thread searches the same on every
/// screen it is read on. The haystack's lowercase is allocated per ask, not cached: a repaint
/// lays out every row's text from scratch anyway, and one allocation beside that many is
/// noise — but the needle is prepared once per frame, never per row.
#[must_use]
fn body_matches(needle: &str, body: &Body) -> bool {
    match body {
        Body::Text(text) => text.to_lowercase().contains(needle),
        _ => false,
    }
}

/// The thread search row, under the header, while the header's magnifier left it open.
///
/// A live filter, not a submitted query: every keystroke narrows the thread below, because a
/// search over messages this session already holds costs no round trip and owes nobody a
/// submit — the same per-keystroke rule the web client's field follows. The hint says what
/// the filter honestly covers — the loaded messages, not the server — the web client's own
/// sentence.
fn search_row(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
    let colors = palette(context.theme);
    let panel = state.searches.entry(conversation_id).or_default();
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Search this conversation")
                        .text_style(crate::theme::named(crate::theme::text_style::OVERLINE))
                        .color(colors.text_muted),
                );
                let response = ui.add(
                    egui::TextEdit::singleline(&mut panel.query)
                        .hint_text("Filter loaded messages")
                        .desired_width((ui.available_width() - space::XL).max(120.0)),
                );
                if panel.claim_focus {
                    response.request_focus();
                    panel.claim_focus = false;
                }
            });
        });
}

/// The verification block at the head of a private conversation's thread: the peer's safety
/// number per device they hold, and — when a device's identity key differs from the last one
/// this vault sealed for it — the warning that says so, above the number it is about.
///
/// §47/§164's own surface. The numbers are the peer's, one row per *device*, because the
/// identity key a bundle carries belongs to a device: a peer with a phone and a laptop shows
/// two, and both have to be verified with the person. Yours is in Settings, under Account, and
/// the tiny line says where to find it — the numbers this block draws are theirs, not yours.
///
/// Drawn only for an encrypted conversation of exactly two members — by member count, the same
/// rule the header's monogram follows, because a group of two reads like a direct chat and
/// should verify like one — and only once a bundle fetch has answered. Before that there is
/// nothing honest to show, and a placeholder that promised safety before any key was seen would
/// be the opposite of the block's purpose.
fn safety_block(ui: &mut Ui, context: &Context<'_>, state: &ChatState, conversation_id: Id) {
    let colors = palette(context.theme);
    let Some(me) = context.account.map(|account| account.account_id) else {
        return;
    };
    let Some(conversation) = state
        .conversations
        .iter()
        .find(|c| c.conversation_id == conversation_id)
    else {
        return;
    };
    if !conversation.encrypted || conversation.members.len() != 2 {
        return;
    }
    let Some(peer) = conversation.members.iter().find(|id| **id != me) else {
        return;
    };
    let Some(devices) = state.peers.get(peer) else {
        return;
    };
    if devices.is_empty() {
        return;
    }
    let who = state
        .names
        .get(peer)
        .cloned()
        .unwrap_or_else(|| model::short_id(*peer));

    for device in devices {
        if device.changed {
            ui.label(
                RichText::new(format!(
                    "\u{26A0} {who}'s identity key changed since it was last seen. Verify the \
                     safety number with them, in a call or in person, before trusting this \
                     conversation."
                ))
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.danger),
            );
            ui.add_space(space::XS);
        }
        ui.label(
            RichText::new(format!(
                "Safety number \u{00B7} their device {}",
                model::short_id(device.device_id)
            ))
            .text_style(crate::theme::named(crate::theme::text_style::OVERLINE))
            .color(colors.text_muted),
        );
        ui.add_space(space::XS);
        ui.label(
            RichText::new(&device.safety_number)
                .font(egui::FontId::monospace(font::SMALL))
                .color(colors.text),
        );
        ui.add_space(space::SM);
    }
    ui.label(
        RichText::new(
            "One number per device they hold. Each is this conversation between this device and \
             theirs — the same number is on their screen for this device, whatever client they \
             hold. Read them to each other, in a call or in person. If they differ, stop and do \
             not trust the conversation.",
        )
        .font(egui::FontId::proportional(font::TINY))
        .color(colors.text_muted),
    );
    ui.add_space(space::MD);
}

/// The room membership tail: "Ana joined the room", "Bo disconnected", newest last.
///
/// Drawn inside the thread's scroll as its final lines, after the messages — the ambient
/// "someone came in" a chat shows, not a durable record. Names resolve the way the typing line's
/// do, at draw time, so a profile that arrives late still lands on its line.
fn room_notices(ui: &mut Ui, context: &Context<'_>, state: &ChatState, conversation_id: Id) {
    let colors = palette(context.theme);
    // Room-kind conversations are the only ones with notices, so the lookup costs one map miss
    // on every direct chat — cheaper than threading the room id down from the header.
    let Some(room_id) = state
        .conversations
        .iter()
        .find(|c| c.conversation_id == conversation_id)
        .and_then(|c| c.room_id)
    else {
        return;
    };
    let Some(notices) = state.room_notices.get(&room_id) else {
        return;
    };
    for notice in notices {
        let who = state
            .names
            .get(&notice.user_id)
            .cloned()
            .unwrap_or_else(|| model::short_id(notice.user_id));
        ui.horizontal(|ui| {
            ui.add_space(space::LG);
            ui.label(
                RichText::new(format!("{who} {}", notice.verb))
                    .font(egui::FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
        });
    }
}

/// A group's membership notices, the group twin of the room notices: the same live tail, the
/// same cap, the same draw at the scroll's end — keyed by the conversation because a group
/// has no room id to key by, with the group's own verbs ("joined the group", not "joined
/// the room") filled in by the shell when the event arrived.
fn group_notices_tail(ui: &mut Ui, context: &Context<'_>, state: &ChatState, conversation_id: Id) {
    let colors = palette(context.theme);
    let Some(notices) = state.group_notices.get(&conversation_id) else {
        return;
    };
    for notice in notices {
        let who = state
            .names
            .get(&notice.user_id)
            .cloned()
            .unwrap_or_else(|| model::short_id(notice.user_id));
        ui.horizontal(|ui| {
            ui.add_space(space::LG);
            ui.label(
                RichText::new(format!("{who} {}", notice.verb))
                    .font(egui::FontId::proportional(font::TINY))
                    .color(colors.text_muted),
            );
        });
    }
}

/// The group's roster panel: every member with role and mute, and the levers the signed-in
/// member holds — invite for everyone, rename/mute/kick for founders, the vote for all.
///
/// The panel's facts are the roster the wire answered, not the conversation row's member
/// preview: roles and mutes live only on the roster, and the founder gates read them, so the
/// panel opens with an ask (the shell issues it the moment the toggle opens) and draws what
/// the answer filed. Until the answer lands the panel says so, because a roster that guessed
/// would be a list of names with wrong authority beside them.
fn group_roster_panel(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatState,
    conversation_id: Id,
) {
    let colors = palette(context.theme);
    let me = context.account.map(|account| account.account_id);
    let Some(conversation) = state
        .conversations
        .iter()
        .find(|c| c.conversation_id == conversation_id)
    else {
        return;
    };
    // Owned facts read before the panel's frame: the frame's closure mutates `state` (the
    // rename and invite panels are its own), so the conversation and roster borrows have to
    // end here, as values, not live into it.
    let listed_members = conversation.members.len();
    let current_title = conversation.title.clone().unwrap_or_default();
    // Who the signed-in member is on this roster: a founder holds rename, mute, and kick;
    // a member holds invite and the vote. Read from the roster, not assumed from the
    // create — a founder who left passed the role on, and the last founder out promotes
    // the earliest remaining member.
    let active_count = state
        .rosters
        .get(&conversation_id)
        .map(|rows| rows.iter().filter(|m| m.left_at.is_none()).count())
        .unwrap_or(0);
    let i_am_founder = state
        .rosters
        .get(&conversation_id)
        .and_then(|rows| me.and_then(|me| rows.iter().find(|m| m.account_id == me)))
        .is_some_and(|m| m.role == ConversationRole::Founder);

    // Deferred commands, past the borrows above: a click is intent, and intent is applied
    // after the panel has finished drawing.
    let mut invite_send: Option<Vec<Id>> = None;
    let mut rename_send: Option<String> = None;
    let mut mute_send: Option<(Id, bool)> = None;
    let mut kick_send: Option<Id> = None;
    let mut vote_send: Option<Id> = None;
    let mut leave = false;

    ui.add_space(space::SM);
    egui::Frame::new()
        .fill(colors.surface_raised)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Members")
                        .text_style(crate::theme::named(crate::theme::text_style::OVERLINE))
                        .color(colors.text_muted),
                );
                widgets::pill(
                    ui,
                    &format!("{active_count} of {}", listed_members.max(active_count)),
                    colors.text_muted,
                    colors.surface,
                );
                // The founder's rename, folded out under the roster: the same patience every
                // typed field in this file is given.
                if i_am_founder
                    && ui
                        .add(
                            egui::Button::new(
                                RichText::new("\u{270F}")
                                    .font(egui::FontId::proportional(font::BODY))
                                    .color(colors.text),
                            )
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE),
                        )
                        .on_hover_text("Rename group")
                        .clicked()
                {
                    let panel = state.renames.entry(conversation_id).or_default();
                    panel.open = !panel.open;
                    if panel.open {
                        panel.title = current_title.clone();
                    }
                }
            });
            ui.add_space(space::XS);

            let Some(rows) = state.rosters.get(&conversation_id) else {
                ui.label(
                    RichText::new("Reading the group's roster…")
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
                return;
            };
            for member in rows {
                let name = state
                    .names
                    .get(&member.account_id)
                    .cloned()
                    .unwrap_or_else(|| model::short_id(member.account_id));
                let departed = member.left_at.is_some();
                ui.horizontal(|ui| {
                    widgets::avatar(ui, context.theme, &name, 22.0);
                    ui.label(
                        RichText::new(name)
                            .font(egui::FontId::proportional(font::SMALL))
                            .color(if departed {
                                colors.text_muted
                            } else {
                                colors.text
                            }),
                    );
                    if member.role == ConversationRole::Founder {
                        widgets::pill(ui, "founder", colors.text_muted, colors.surface);
                    }
                    if member.muted_until.is_some() && !departed {
                        widgets::pill(ui, "muted", colors.warning, colors.surface);
                    }
                    if departed {
                        widgets::pill(ui, "left", colors.text_muted, colors.surface);
                    }
                    // The founder's levers, on the active members that are not this account:
                    // the server refuses a self-mute and a self-kick, and the other founder
                    // is beyond both — the button is withheld rather than sent to fail,
                    // because a refusal the person can see coming is kinder than one that
                    // arrives.
                    let targetable = i_am_founder
                        && me != Some(member.account_id)
                        && member.role != ConversationRole::Founder
                        && !departed;
                    if targetable {
                        let muted = member.muted_until.is_some();
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(if muted { "Unmute" } else { "Mute" })
                                        .font(egui::FontId::proportional(font::TINY))
                                        .color(colors.text_muted),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .clicked()
                        {
                            mute_send = Some((member.account_id, !muted));
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Remove")
                                        .font(egui::FontId::proportional(font::TINY))
                                        .color(colors.text_muted),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .on_hover_text("Costs 1 Kick Point, or 1 $MIG when none are held")
                            .clicked()
                        {
                            kick_send = Some(member.account_id);
                        }
                    }
                    // The vote, every member's lever, never aimed at this account and never
                    // at a founder: the same gates the server holds, mirrored so the button
                    // says what the wire would allow. Departed members are past voting out.
                    if me != Some(member.account_id)
                        && member.role != ConversationRole::Founder
                        && !departed
                        && ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Vote remove")
                                        .font(egui::FontId::proportional(font::TINY))
                                        .color(colors.text_muted),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .on_hover_text("Free — the vote costs nothing")
                            .clicked()
                    {
                        vote_send = Some(member.account_id);
                    }
                });
                ui.add_space(space::XS);
            }

            // A running vote's tally, if the wire has one open: "2 of 4 needed" reads as a
            // question the group is still answering, and the newest tally per conversation
            // is the one that matters.
            if let Some(tally) = state.votes.get(&conversation_id) {
                ui.label(
                    RichText::new(format!(
                        "Vote to remove {}: {} of {} needed",
                        state
                            .names
                            .get(&tally.target_id)
                            .cloned()
                            .unwrap_or_else(|| model::short_id(tally.target_id)),
                        tally.votes,
                        tally.needed,
                    ))
                    .font(egui::FontId::proportional(font::TINY))
                    .color(colors.warning),
                );
            }

            // The invite row, every member's right: an account id typed by hand. The friends
            // the panel could offer are the friends pane's rows, not this pane's, and a
            // one-at-a-time field keeps the wire's "any current member may invite" honest.
            let invite_open = state
                .invites
                .get(&conversation_id)
                .is_some_and(|panel| panel.open);
            if invite_open {
                let panel = state.invites.entry(conversation_id).or_default();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut panel.account_id)
                        .hint_text("account id")
                        .desired_width(ui.available_width() - 96.0),
                );
                let submitted =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Invite").clicked() || submitted)
                    && !panel.account_id.trim().is_empty()
                {
                    if let Ok(id) = Id::parse(panel.account_id.trim()) {
                        invite_send = Some(vec![id]);
                        panel.account_id.clear();
                    }
                }
            } else if ui.button("+ Invite").clicked() {
                let panel = state.invites.entry(conversation_id).or_default();
                panel.open = true;
                panel.account_id.clear();
            }

            // The founder's rename row, seeded from the current title when it opened.
            let rename_open = state
                .renames
                .get(&conversation_id)
                .is_some_and(|panel| panel.open);
            if rename_open && i_am_founder {
                let panel = state.renames.entry(conversation_id).or_default();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut panel.title)
                        .hint_text("group name")
                        .desired_width(ui.available_width() - 96.0),
                );
                let submitted =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Rename").clicked() || submitted) && !panel.title.trim().is_empty() {
                    rename_send = Some(panel.title.trim().to_owned());
                }
            }

            // Leaving, the lever every member holds. The window's copy of the thread goes
            // when the ack arrives, so no confirmation is spent here — the click is the
            // confirmation, the same as the web client's leave.
            if ui.button("Leave group").clicked() {
                leave = true;
            }
        });
    ui.add_space(space::SM);

    if let Some(members) = invite_send {
        context.issue(Command::InviteToGroup {
            conversation_id,
            members,
        });
    }
    if let Some(title) = rename_send {
        context.issue(Command::RenameGroup {
            conversation_id,
            title,
        });
        if let Some(panel) = state.renames.get_mut(&conversation_id) {
            panel.open = false;
        }
    }
    if let Some((target_id, mute)) = mute_send {
        // A mute runs an hour — the web client's default — and an unmute is the request
        // with no `until` at all, the wire's own word for "lift it".
        let until = mute.then(|| {
            migo_core::Timestamp::from_unix_ms(
                migo_core::Timestamp::now().as_unix_ms() + 60 * 60 * 1000,
            )
        });
        context.issue(Command::MuteGroupMember {
            conversation_id,
            target_id,
            until,
        });
    }
    if let Some(target_id) = kick_send {
        context.issue(Command::KickGroupMember {
            conversation_id,
            target_id,
        });
    }
    if let Some(target_id) = vote_send {
        context.issue(Command::VoteKickMember {
            conversation_id,
            target_id,
        });
    }
    if leave {
        context.issue(Command::LeaveGroup { conversation_id });
    }
}

/// The compact header over the open conversation: title, encryption state, and the counts that
/// are true of the whole thread rather than of any one message in it.
///
/// Mutable state, unlike most headers, for two buttons: the floppy folds the transcript save
/// row in and out, and the magnifier folds the thread search row in and out — both toggles
/// are the conversation's own (kept the way drafts are), so the header writes them.
fn thread_header(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatState,
    conversation_id: Id,
) {
    let colors = palette(context.theme);
    let Some(conversation) = state
        .conversations
        .iter()
        .find(|c| c.conversation_id == conversation_id)
    else {
        return;
    };
    let title = context
        .account
        .map(|a| conversation.display_title(a.account_id, &state.names))
        .unwrap_or_else(|| model::short_id(conversation_id));
    // Deferred, not written at the click: `conversation` above borrows `state`, and the toggle
    // writes `state.log_panels` — one frame's worth of patience keeps the two borrows apart.
    let mut want_log_panel = false;
    // The search's toggle, deferred for the same reason — the same patience, twice.
    let mut want_search_panel = false;
    // The roster's toggle, deferred for the same reason — the same patience, three times.
    let mut want_roster_panel = false;
    // The peer verdicts, deferred for the same reason — the same patience, four times: a
    // personal mute on the peer of a direct chat, or the block that ends it. Both act on
    // someone outside the conversation row's own state, so they are only issued after the
    // header's borrows close.
    let mut peer_mute_toggle: Option<(Id, bool)> = None;
    let mut peer_block: Option<Id> = None;

    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::LG);
        widgets::avatar(ui, context.theme, &title, 34.0);
        ui.add_space(space::SM);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(widelide(&title))
                    .font(egui::FontId::proportional(font::SUBTITLE))
                    .color(colors.text)
                    .strong(),
            );
            // The lock travels with the words: a padlock floating alone is decoration, and the
            // pair together is the one thing about a conversation a reader must be able to
            // find without hunting for it.
            let detail = if conversation.encrypted {
                "\u{1F512} End-to-end encrypted"
            } else {
                // Said plainly rather than left blank. A user who cannot tell an encrypted
                // conversation from an unencrypted one has no way to act on the difference, and
                // the honest name for what remains is the transport's own encryption.
                "Transport encryption only"
            };
            ui.label(
                RichText::new(detail)
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(if conversation.encrypted {
                        colors.positive
                    } else {
                        colors.warning
                    }),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::LG);
            // The unread count comes from the server's read watermark, which can lag the thread
            // open on screen, so the badge appears here whenever it does.
            widgets::unread_badge(ui, context.theme, conversation.unread);
            ui.add_space(space::XS);
            // The call button, on an encrypted two-member conversation only — the same gate the
            // web and Android headers use. A call is sealed with the conversation's own E2EE
            // group layer, so an unencrypted conversation has no key to seal with, and a group
            // call is a different protocol this build does not speak. Busy is the worker's word:
            // a second call while one runs is refused there with a toast, not hidden here,
            // because the button's target (the one other member) does not change with call
            // state and re-deriving that gate in the UI would be two opinions about one rule.
            if conversation.encrypted && conversation.members.len() == 2 {
                if let Some(me) = context.account.map(|account| account.account_id) {
                    if let Some(peer) = conversation.members.iter().find(|id| **id != me) {
                        let colors = palette(context.theme);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("\u{1F4DE}")
                                        .font(egui::FontId::proportional(font::BODY))
                                        .color(colors.text),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .on_hover_text("Voice call")
                            .clicked()
                        {
                            context.issue(Command::StartCall {
                                conversation_id,
                                callee_id: *peer,
                            });
                        }
                        ui.add_space(space::XS);
                    }
                }
            }
            // The peer's own row of verdicts, on a direct chat only: the personal mute the
            // web profile modal offers, and the block that ends the conversation. The mute
            // needs the muted set to say which way its switch points; the block is set-only
            // on the wire, so it states what it does and never offers an undo.
            if !conversation.is_group() && conversation.room_id.is_none() {
                if let Some(me) = context.account.map(|account| account.account_id) {
                    if let Some(peer) = conversation.members.iter().find(|id| **id != me) {
                        let muted = state.muted.contains(peer);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(if muted { "Unmute" } else { "Mute for me" })
                                        .font(egui::FontId::proportional(font::TINY))
                                        .color(colors.text_muted),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .on_hover_text(
                                "Hides this person's room messages for you. Direct messages are never muted.",
                            )
                            .clicked()
                        {
                            peer_mute_toggle = Some((*peer, !muted));
                        }
                        ui.add_space(space::XS);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Block")
                                        .font(egui::FontId::proportional(font::TINY))
                                        .color(colors.text_muted),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE),
                            )
                            .on_hover_text(
                                "End the friendship and stop contact. There is no unblock.",
                            )
                            .clicked()
                        {
                            peer_block = Some(*peer);
                        }
                        ui.add_space(space::XS);
                    }
                }
            }
            if conversation.members.len() > 2 {
                widgets::pill(
                    ui,
                    &format!("{} members", conversation.members.len()),
                    colors.text_muted,
                    colors.surface_raised,
                );
            }
            // The floppy: this conversation's words as a file, on demand. The automatic copy
            // (Settings, "save on close") is a preference; this button is the one-off that
            // needs no preference — a person closing a contract negotiation wants the record
            // whether or not they ever turned anything on.
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("\u{1F4BE}")
                            .font(egui::FontId::proportional(font::BODY))
                            .color(colors.text),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE),
                )
                .on_hover_text("Save transcript")
                .clicked()
            {
                want_log_panel = true;
            }
            // The magnifier, beside the floppy: this conversation's thread, searched live over
            // what this session already holds. The honesty rule is the web field's own — a
            // filter of the loaded messages, not a query the server answers — and the row
            // under the header says so in the field's own hint.
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("\u{1F50D}")
                            .font(egui::FontId::proportional(font::BODY))
                            .color(colors.text),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE),
                )
                .on_hover_text("Search this conversation")
                .clicked()
            {
                want_search_panel = true;
            }
            // The people button, on a group only: the roster, the roles, and every lever a
            // member or a founder holds — invite, rename, mute, kick, the vote, and leaving.
            // Gated on the server's own kind and not the member count, because a group of
            // two (one member just left) is still a group with a roster and a rename.
            if conversation.is_group()
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new("\u{1F465}")
                                .font(egui::FontId::proportional(font::BODY))
                                .color(colors.text),
                        )
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE),
                    )
                    .on_hover_text("Group members")
                    .clicked()
            {
                want_roster_panel = true;
            }
        });
    });
    if want_log_panel {
        let panel = state.log_panels.entry(conversation_id).or_default();
        panel.open = !panel.open;
    }
    if let Some((peer, on)) = peer_mute_toggle {
        context.issue(Command::MuteUser { user_id: peer, on });
    }
    if let Some(peer) = peer_block {
        context.issue(Command::BlockUser { user_id: peer });
    }
    if want_search_panel {
        let panel = state.searches.entry(conversation_id).or_default();
        panel.open = !panel.open;
        // Opening claims the field's focus, the frame the row first draws: a magnifier click
        // that left the person to find the field themselves would be the question
        // half-asked.
        if panel.open {
            panel.claim_focus = true;
        }
    }
    if want_roster_panel {
        let open = state.roster_open.entry(conversation_id).or_insert(false);
        *open = !*open;
        // Opening is the ask: the roster the panel draws is a wire fact, and the panel's
        // first frame is the moment it starts waiting for one.
        if *open {
            context.issue(Command::GroupRoster { conversation_id });
        }
    }
    ui.add_space(space::MD);
}

/// A date across the thread, so a long history is navigable.
fn day_separator(ui: &mut Ui, context: &Context<'_>, day: &str) {
    let colors = palette(context.theme);
    ui.vertical_centered(|ui| {
        ui.add_space(space::SM);
        widgets::pill(ui, day, colors.text_muted, colors.surface_raised);
        ui.add_space(space::SM);
    });
}

/// One message, as a bubble with its delivery state.
///
/// Incoming messages carry a small avatar beside the bubble — the peer's monogram in a direct
/// chat, the sender's in a group — because with avatars the eye tracks who said what by colour
/// instead of by reading a name, and a thread that can be followed peripherally reads faster.
///
/// Text renders as the plain bubble; an attachment or a voice note renders as its own kind of
/// row (an image, a save affordance, a play button), because what a message *is* decides what
/// touching it does. The media and reactions state is passed in mutable: the row that draws
/// an unfetched image is the row that asks for it, and the row that can be reacted to is the
/// row that carries the picker.
// Every fact the row draws is a fact it needs, and in immediate mode they arrive as
// parameters, not as a struct the caller would build only to hand it here.
#[allow(clippy::too_many_arguments)]
fn message_row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    message: &Message,
    sender: Option<&str>,
    avatar_seed: Option<&str>,
    read: bool,
    media: &mut MediaState,
    reactions: &mut HashMap<Id, Vec<(Id, String)>>,
    edits: &mut HashMap<Id, EditDraft>,
) {
    // The disappearing mark rides the meta line as a glyph, so the promise is visible on the
    // row itself rather than learned only when the row vanishes — the web's ExpiryMark, the
    // same clock the composer's arm shows, on every row the arm produced. The deadline it
    // names is the receiving clock's, by the same design the web states: a client must not
    // wait for the server to tell it a message is gone.
    let meta = format!(
        "{} {}{}{}{}",
        model::clock(message.sent_at),
        tick(message.delivery),
        if read { " \u{2713}\u{2713}" } else { "" },
        // The correction's mark, the same quiet word the web client's bubble carries. It
        // rides the meta line rather than the bubble because it is a fact about the row,
        // not part of what was said.
        if message.edited { " · edited" } else { "" },
        if message.expires_at.is_some() {
            " \u{1F552}"
        } else {
            ""
        }
    );
    if let Some(sender) = sender {
        let colors = palette(context.theme);
        ui.horizontal(|ui| {
            // Indented to the bubble's own left edge, so the name reads as a label on the bubble
            // rather than as a separate row of its own.
            ui.add_space(space::LG + space::SM);
            ui.label(
                RichText::new(sender)
                    .text_style(crate::theme::named(crate::theme::text_style::CAPTION))
                    .color(colors.text_muted),
            );
        });
    }
    ui.horizontal(|ui| {
        ui.add_space(space::LG);
        if let Some(seed) = avatar_seed {
            widgets::avatar(ui, context.theme, seed, 24.0);
            ui.add_space(space::SM);
        }
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width() - space::LG, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                // One of this account's own text messages, open in the inline editor: the
                // bubble becomes the field and the row's actions wait below it, the same
                // trade the web client's MessageEditor makes. A finished editor (saved,
                // cancelled, or emptied) closes itself here, so the row is a bubble again
                // on this same paint.
                let mut edit_done = false;
                if let Some(draft) = edits.get_mut(&message.message_id) {
                    edit_done = edit_in_place(ui, context, message, draft);
                    if !edit_done {
                        reaction_chips(ui, context, message, reactions);
                        return;
                    }
                }
                if edit_done {
                    edits.remove(&message.message_id);
                }
                match &message.body {
                    Body::Text(text) => {
                        widgets::bubble(ui, context.theme, text, &meta, message.outgoing, BubbleTone::Normal);
                    }
                    Body::Media {
                        media_id,
                        mime_type,
                        size_bytes,
                        width,
                        height,
                        caption,
                    } => {
                        attachment_bubble(
                            ui,
                            context,
                            message.outgoing,
                            *media_id,
                            mime_type,
                            *size_bytes,
                            *width,
                            *height,
                            caption.as_deref(),
                            &meta,
                            media,
                        );
                    }
                    Body::VoiceNote {
                        media_id,
                        duration_ms,
                    } => {
                        voice_bubble(ui, context, message.outgoing, *media_id, *duration_ms, &meta, media);
                    }
                    // A reaction never reaches a row: absorb files it as a chip on its
                    // target. The arm stays because the match must be exhaustive, and a
                    // body that somehow arrives here is drawn as nothing rather than as a
                    // message that buries its own target.
                    Body::Reaction { .. } => {}
                    // A withdrawn message keeps its row — the sequence numbering has no
                    // hole — but says only the fact. The web client's tombstone text, the
                    // same words on every surface so a transcript read on two devices
                    // tells one story.
                    Body::Tombstone => {
                        widgets::bubble(
                            ui,
                            context.theme,
                            "Message deleted",
                            &meta,
                            message.outgoing,
                            BubbleTone::Problem,
                        );
                    }
                    Body::Unsupported { content_type } => {
                        widgets::bubble(
                            ui,
                            context.theme,
                            &format!("Unsupported message (type {content_type}). Update Migo to read it."),
                            &meta,
                            message.outgoing,
                            BubbleTone::Problem,
                        );
                    }
                }
                reaction_chips(ui, context, message, reactions);
                own_message_actions(ui, context, message, edits);
            },
        );
    });
}

/// One open edit: the replacement text so far, and whether the field has had its focus
/// claimed yet. A draft lives in the chat state keyed by message id, so it survives
/// repaints the way a composer draft does; opening a second edit closes the first by
/// simply not being that id anymore.
#[derive(Debug, Default)]
pub struct EditDraft {
    pub text: String,
    pub claim_focus: bool,
}

/// The inline editor for one of this account's own text messages: the bubble becomes a
/// field, Enter commits, Escape closes, and an empty field closes rather than sending a
/// blank replacement the server would only refuse. Returns `true` when the editor is
/// finished — saved, cancelled, or emptied — and the row should go back to being a bubble.
fn edit_in_place(
    ui: &mut Ui,
    context: &mut Context<'_>,
    message: &Message,
    draft: &mut EditDraft,
) -> bool {
    let field = egui::TextEdit::multiline(&mut draft.text)
        .hint_text("the corrected message")
        .desired_width((ui.available_width() - space::LG * 2.0).max(120.0));
    let response = ui.add(field);
    if draft.claim_focus {
        response.request_focus();
        draft.claim_focus = false;
    }
    let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut done = false;
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
        if widgets::primary_button(ui, context.theme, "Save", !draft.text.trim().is_empty())
            .clicked()
            || submitted
        {
            let text = draft.text.trim().to_owned();
            if !text.is_empty() {
                context.issue(Command::EditMessage {
                    conversation_id: message.conversation_id,
                    message_id: message.message_id,
                    text,
                });
            }
            done = true;
        }
        if ui.button("Cancel").clicked() || cancelled {
            done = true;
        }
    });
    // An emptied field is a closed editor too: there is nothing left to save, and a blank
    // replacement is a message the server would only refuse.
    done || draft.text.trim().is_empty()
}

/// The hover actions on this account's own messages: Edit on a text bubble, Delete on any.
/// Drawn as quiet text buttons below the bubble, on the sender's own side, so the actions
/// are reachable but never compete with the reading.
fn own_message_actions(
    ui: &mut Ui,
    context: &mut Context<'_>,
    message: &Message,
    edits: &mut HashMap<Id, EditDraft>,
) {
    if !message.outgoing || message.deleted || message.delivery == Delivery::Sending {
        return;
    }
    let colors = palette(context.theme);
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
        // Edit is offered on the one body this client composes: a text message. An
        // attachment cannot be re-typed, and the web client offers nothing there either.
        if matches!(message.body, Body::Text(ref text) if !text.is_empty())
            && ui
                .add(
                    egui::Button::new(
                        RichText::new("Edit")
                            .font(egui::FontId::proportional(font::TINY))
                            .color(colors.text_muted),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE),
                )
                .on_hover_text("Fix this message for everyone")
                .clicked()
        {
            let draft = edits.entry(message.message_id).or_default();
            if draft.text.is_empty() {
                if let Body::Text(text) = &message.body {
                    draft.text = text.clone();
                }
            }
            draft.claim_focus = true;
        }
        if ui
            .add(
                egui::Button::new(
                    RichText::new("Delete")
                        .font(egui::FontId::proportional(font::TINY))
                        .color(colors.text_muted),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE),
            )
            .on_hover_text("Withdraw this message for everyone")
            .clicked()
        {
            context.issue(Command::DeleteMessage {
                conversation_id: message.conversation_id,
                message_id: message.message_id,
            });
        }
    });
}

/// One image or document attachment, as its own kind of bubble.
///
/// An image is shown as the image — fitted to the column but never beyond its own pixels —
/// once it has been fetched, and as a quiet placeholder naming its dimensions until then,
/// because a bubble that reserves no room would resize the whole thread when the picture
/// lands. A document is a row: what it claims to be, how big it is, and where to save it.
/// egui offers no save dialog, so the destination is typed — the same trade the attach
/// panel and the avatar picker make.
// The arm count is the message's own shape: what a media body carries is what the row
// draws, and splitting it into structs would split one bubble across two types.
#[allow(clippy::too_many_arguments)]
fn attachment_bubble(
    ui: &mut Ui,
    context: &mut Context<'_>,
    outgoing: bool,
    media_id: Id,
    mime_type: &str,
    size_bytes: u64,
    width: Option<u32>,
    height: Option<u32>,
    caption: Option<&str>,
    meta: &str,
    media: &mut MediaState,
) {
    if mime_type.starts_with("image/") {
        image_bubble(
            ui, context, outgoing, media_id, width, height, caption, meta, media,
        );
        return;
    }
    let colors = palette(context.theme);
    widgets::bubble(
        ui,
        context.theme,
        &format!(
            "\u{1F4C4} Document \u{00B7} {mime_type} \u{00B7} {}",
            model::human_bytes(size_bytes)
        ),
        meta,
        outgoing,
        BubbleTone::Normal,
    );
    if let Some(reason) = media.failures.get(&media_id) {
        ui.label(
            RichText::new(reason)
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.danger),
        );
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
        // The destination, kept between frames so a failed save retries without retyping.
        let path = media.save_paths.entry(media_id).or_default();
        let field = egui::TextEdit::singleline(path)
            .hint_text("/path/to/save")
            .desired_width(200.0);
        let response = ui.add(field);
        if ui.button("Save").clicked() {
            let typed = path.trim().to_owned();
            if typed.is_empty() {
                response.request_focus();
            } else {
                context.issue(Command::SaveMedia {
                    media_id,
                    path: PathBuf::from(typed),
                });
            }
        }
    });
}

/// One image attachment: the picture when it has arrived, a named placeholder until then.
// As above: each parameter is one fact of the image, drawn.
#[allow(clippy::too_many_arguments)]
fn image_bubble(
    ui: &mut Ui,
    context: &mut Context<'_>,
    outgoing: bool,
    media_id: Id,
    width: Option<u32>,
    height: Option<u32>,
    caption: Option<&str>,
    meta: &str,
    media: &mut MediaState,
) {
    let colors = palette(context.theme);
    if let Some(blob) = media.images.get(&media_id) {
        // One upload per image: the texture is built the first frame the blob is drawn, and
        // dropped with the blob when a re-fetch replaces it (the app layer removes the old
        // texture beside the new blob, so stale pixels never outlive their bytes).
        let texture = media.textures.entry(media_id).or_insert_with(|| {
            ui.ctx().load_texture(
                format!("media-{media_id}"),
                egui::ColorImage::from_rgba_unmultiplied(
                    [blob.width as usize, blob.height as usize],
                    &blob.rgba,
                ),
                egui::TextureOptions::default(),
            )
        });
        // Fitted to the column, capped at a reasonable height, never beyond the image's own
        // pixels: a thumbnail smaller than the column is shown at its own size, the way the
        // web client shows one.
        let available = (ui.available_width() - space::LG).min(320.0);
        let cap = 320.0;
        let (w, h) = (f64::from(blob.width.max(1)), f64::from(blob.height.max(1)));
        let scale = (available / w as f32).min(cap / h as f32).min(1.0);
        let size = egui::vec2(w as f32 * scale, h as f32 * scale);
        ui.image((texture.id(), size));
        if let Some(caption) = caption.filter(|caption| !caption.is_empty()) {
            ui.label(
                RichText::new(caption)
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text),
            );
        }
        ui.label(
            RichText::new(meta)
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
        return;
    }
    if let Some(reason) = media.failures.get(&media_id) {
        // The fetch refused: say so in the bubble it belongs to, and offer the one honest
        // action again — a retry, for whatever changed since.
        widgets::bubble(
            ui,
            context.theme,
            reason,
            meta,
            outgoing,
            BubbleTone::Problem,
        );
        if ui.button("Try again").clicked() {
            media.requested.remove(&media_id);
            media.failures.remove(&media_id);
        }
        return;
    }
    // Not fetched yet. The first frame that draws this bubble is the frame that asks; every
    // later frame reads `requested` and draws the placeholder without another command.
    if media.requested.insert(media_id) {
        context.issue(Command::FetchMedia { media_id });
    }
    let dims = match (width, height) {
        (Some(width), Some(height)) => format!(" \u{00B7} {width}\u{00D7}{height}"),
        _ => String::new(),
    };
    widgets::bubble(
        ui,
        context.theme,
        &format!("Loading image{dims}\u{2026}"),
        meta,
        outgoing,
        BubbleTone::Normal,
    );
}

/// One voice note: a play/stop button, the note's length, and the delivery state.
///
/// The button is the bubble — the whole point of a voice note is that it is pressed, and a
/// row whose only control sits outside its shape is a row that has to explain itself.
fn voice_bubble(
    ui: &mut Ui,
    context: &mut Context<'_>,
    outgoing: bool,
    media_id: Id,
    duration_ms: u32,
    meta: &str,
    media: &mut MediaState,
) {
    let colors = palette(context.theme);
    let playing = media.playing == Some(media_id);
    if let Some(reason) = media.failures.get(&media_id) {
        widgets::bubble(
            ui,
            context.theme,
            reason,
            meta,
            outgoing,
            BubbleTone::Problem,
        );
        if ui.button("Try again").clicked() {
            media.requested.remove(&media_id);
            media.failures.remove(&media_id);
        }
        return;
    }
    ui.horizontal(|ui| {
        let glyph = if playing { "\u{23F9}" } else { "\u{25B6}" };
        if ui
            .add(
                egui::Button::new(
                    RichText::new(glyph)
                        .font(egui::FontId::proportional(font::BODY))
                        .color(colors.text_on_accent),
                )
                .fill(colors.accent)
                .min_size(egui::vec2(30.0, 30.0)),
            )
            .on_hover_text(if playing { "Stop" } else { "Play" })
            .clicked()
        {
            // One button, both meanings: the note that is playing is the note the press
            // stops, and any other note is the note the press starts (the worker stops the
            // old one itself).
            if playing {
                context.issue(Command::StopVoiceNote);
            } else {
                context.issue(Command::PlayVoiceNote { media_id });
            }
        }
        ui.add_space(space::XS);
        widgets::bubble(
            ui,
            context.theme,
            &format!(
                "\u{1F3A4} Voice note \u{00B7} {}",
                human_duration(duration_ms)
            ),
            meta,
            outgoing,
            BubbleTone::Normal,
        );
    });
}

/// The reaction chips under one message, and the picker that adds to them.
///
/// Grouped by emoji with a count, the same grouping every client shows; the account's own
/// chip is the accented one, so "did mine land?" is answered at a glance. The picker is the
/// same three emoji the web and Android clients offer, and picking files an optimistic chip
/// immediately — this device's own echo is suppressed in the worker, so the click is the
/// only moment the own chip is ever drawn from.
fn reaction_chips(
    ui: &mut Ui,
    context: &mut Context<'_>,
    message: &Message,
    reactions: &mut HashMap<Id, Vec<(Id, String)>>,
) {
    let colors = palette(context.theme);
    let me = context.account.map(|account| account.account_id);
    ui.menu_button(
        RichText::new("\u{1F642}")
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.text_muted),
        |ui| {
            for emoji in REACTIONS {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(emoji).font(egui::FontId::proportional(font::TITLE)),
                        )
                        .fill(egui::Color32::TRANSPARENT),
                    )
                    .clicked()
                {
                    context.issue(Command::SendReaction {
                        conversation_id: message.conversation_id,
                        target_message_id: message.message_id,
                        emoji: emoji.to_owned(),
                    });
                    if let Some(me) = me {
                        let chips = reactions.entry(message.message_id).or_default();
                        let pair = (me, emoji.to_owned());
                        if !chips.contains(&pair) {
                            chips.push(pair);
                        }
                    }
                }
            }
        },
    );
    // Drawn after the picker so the closure's mutable borrow of the reactions map ends
    // before this read — the chips are whatever they are by the time the row paints.
    let Some(chips) = reactions.get(&message.message_id) else {
        return;
    };
    if chips.is_empty() {
        return;
    }
    // Grouped by emoji, first-seen order, with each group marked when it holds this
    // account's own chip.
    let mut groups: Vec<(String, usize, bool)> = Vec::new();
    for (sender, emoji) in chips {
        let own = me == Some(*sender);
        match groups.iter_mut().find(|(held, _, _)| held == emoji) {
            Some((_, count, own_seen)) => {
                *count += 1;
                *own_seen |= own;
            }
            None => groups.push((emoji.clone(), 1, own)),
        }
    }
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
        for (emoji, count, own) in groups {
            let text = if count > 1 {
                format!("{emoji} {count}")
            } else {
                emoji.clone()
            };
            let (foreground, background) = if own {
                (colors.text_on_accent, colors.accent)
            } else {
                (colors.text, colors.surface_raised)
            };
            widgets::pill(ui, &text, foreground, background);
            ui.add_space(space::XS);
        }
    });
}

/// The delivery mark shown after the timestamp.
///
/// Only outgoing messages carry one, because a tick on something received tells the reader nothing
/// they do not already know by seeing it. A read message upgrades one tick to two — the same
/// pair the web client draws — but the upgrade is decided by the caller (the read watermark),
/// not here: the marker means *someone else* read it, and this function has no way to know.
fn tick(state: Delivery) -> &'static str {
    match state {
        Delivery::Sending => "\u{00B7}\u{00B7}\u{00B7}",
        Delivery::Sent => "\u{2713}",
        Delivery::Failed => "\u{26A0}",
        Delivery::Received => "",
    }
}

/// "Someone is typing", below the thread.
fn typing_line(ui: &mut Ui, context: &Context<'_>, state: &ChatState, conversation_id: Id) {
    let colors = palette(context.theme);
    let Some(who) = state.typing.get(&conversation_id) else {
        return;
    };
    if who.is_empty() {
        return;
    }
    let names: Vec<String> = who
        .iter()
        .map(|id| {
            state
                .names
                .get(id)
                .cloned()
                .unwrap_or_else(|| model::short_id(*id))
        })
        .collect();
    let text = if names.len() == 1 {
        format!("{} is typing\u{2026}", names[0])
    } else {
        format!("{} people are typing\u{2026}", names.len())
    };
    ui.horizontal(|ui| {
        ui.add_space(space::LG);
        ui.label(
            RichText::new(text)
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
    });
}

/// The one disappearing lifetime the desktop offers, in milliseconds — the web's
/// `DISAPPEARING_MS`, the same number, so a thread armed on either client makes the same
/// promise.
pub const DISAPPEARING_MS: u32 = 8 * 60 * 60 * 1_000;

/// The lifetime's own words, for the toggle's hover — the web's `DISAPPEARING_LABEL`.
const DISAPPEARING_LABEL: &str = "8 hours";

/// The composer.
///
/// Enter sends, Shift+Enter inserts a newline. That is the convention every chat client uses, and
/// reversing it means every third message is sent half-finished.
///
/// A message can be three things — text, a voice note, a file — and all three start here: the
/// field types the first, the microphone records the second, the paperclip folds out a panel for
/// the third. While a note is being recorded the composer is replaced by the recording bar,
/// because a field that still accepts typing invites a message that arrives after — and
/// interrupts — the note it would replace.
fn composer(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatState,
    conversation_id: Id,
    is_room: bool,
) {
    let colors = palette(context.theme);
    let online = context.connection.is_online();

    // The recording bar: this conversation's live note, a ticking length, and the only two
    // honest actions — keep it or throw it away.
    if let Some((recording_conversation, started)) = state.recording {
        if recording_conversation == conversation_id {
            egui::Frame::new()
                .fill(colors.surface)
                .inner_margin(egui::Margin::symmetric(space::LG as i8, space::MD as i8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("\u{25CF} Recording")
                                .font(egui::FontId::proportional(font::BODY))
                                .color(colors.danger),
                        );
                        let elapsed = started.elapsed().as_millis() as u32;
                        ui.label(
                            RichText::new(human_duration(elapsed))
                                .font(egui::FontId::proportional(font::BODY))
                                .color(colors.text_muted),
                        );
                        // The length is a clock: ask for frames on a cadence so it ticks
                        // instead of freezing between interactions.
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(500));
                        if ui.button("Cancel").clicked() {
                            context.issue(Command::StopRecording { send: false });
                        }
                        if ui.button("Send").clicked() {
                            context.issue(Command::StopRecording { send: true });
                        }
                    });
                });
            return;
        }
    }

    egui::Frame::new()
        .fill(colors.surface)
        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Room for everything that shares the field's row: the send button and the
                // two attachment affordances beside it.
                let send_width = 56.0 + 2.0 * (32.0 + space::SM);
                // This conversation's own draft, born empty the first time it is typed into and
                // left exactly as it stands when the window closes.
                let draft = state.drafts.entry(conversation_id).or_default();
                let response = ui.add_enabled(
                    online,
                    egui::TextEdit::multiline(draft)
                        .hint_text(if online {
                            "Write a message"
                        } else {
                            "Offline. Reconnecting\u{2026}"
                        })
                        .desired_rows(1)
                        .desired_width(ui.available_width() - send_width - space::SM)
                        .margin(egui::Margin::symmetric(space::LG as i8, space::MD as i8)),
                );
                // The pill: the input's own frame is rounded to the composer's capsule shape.
                let pill =
                    egui::Rect::from_min_size(response.rect.shrink(0.0).min, response.rect.size());
                let _ = pill;
                ui.painter().rect_stroke(
                    response.rect,
                    20.0,
                    egui::Stroke::new(1.0, colors.border),
                    egui::StrokeKind::Inside,
                );

                let enter = ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);
                let send_by_key = response.has_focus() && enter;
                let send_by_click = widgets::send_button(ui, context.theme, online).clicked();

                // Every read of the draft from here on takes its own short borrow: the send and
                // the typing report both write it, and one long borrow would have them fight.
                let can_send = online
                    && state
                        .drafts
                        .get(&conversation_id)
                        .is_some_and(|draft| !draft.trim().is_empty());
                // The lifetime an armed composer stamps on this send, read before the send's
                // own borrows so the arm is one fact read once.
                let expires_in_ms = state
                    .disappearing
                    .contains(&conversation_id)
                    .then_some(DISAPPEARING_MS);
                if (send_by_key || send_by_click) && can_send {
                    let text = state
                        .drafts
                        .get_mut(&conversation_id)
                        .map(|draft| {
                            let text = draft.trim().to_owned();
                            draft.clear();
                            text
                        })
                        .unwrap_or_default();
                    context.issue(Command::SendText {
                        conversation_id,
                        text,
                        expires_in_ms,
                    });
                    if state
                        .typing_sent
                        .get(&conversation_id)
                        .copied()
                        .unwrap_or(false)
                    {
                        context.issue(Command::Typing {
                            conversation_id,
                            typing: false,
                        });
                        state.typing_sent.insert(conversation_id, false);
                    }
                    // Enter leaves a newline in the buffer on some platforms; clearing after the
                    // command is queued keeps the field empty either way.
                    if let Some(draft) = state.drafts.get_mut(&conversation_id) {
                        draft.clear();
                    }
                    response.request_focus();
                }

                // Typing is reported on the transition, not per keystroke. A frame-rate stream of
                // typing frames is bandwidth spent to say the same thing sixty times a second, and the
                // server would rightly rate-limit it.
                let has_text = state
                    .drafts
                    .get(&conversation_id)
                    .is_some_and(|draft| !draft.trim().is_empty());
                let sent = state
                    .typing_sent
                    .get(&conversation_id)
                    .copied()
                    .unwrap_or(false);
                if has_text != sent && online {
                    context.issue(Command::Typing {
                        conversation_id,
                        typing: has_text,
                    });
                    state.typing_sent.insert(conversation_id, has_text);
                }

                // The disappearing arm: a clock the person can set on the thread's future.
                // Excluded in rooms exactly the way the web excludes it — a room is
                // server-readable, so a transcript the server keeps is not a promise a sender
                // can make. The armed state colours the glyph so the promise is visible on the
                // composer itself, not learned only when a row later vanishes.
                if !is_room {
                    let armed = state.disappearing.contains(&conversation_id);
                    let clock =
                        RichText::new("\u{1F552}").font(egui::FontId::proportional(font::BODY));
                    let clock = if armed {
                        clock.color(colors.accent)
                    } else {
                        clock.color(colors.text_muted)
                    };
                    if ui
                        .add(egui::Button::new(clock))
                        .on_hover_text(if armed {
                            format!(
                                "Disappearing on — new messages vanish after {DISAPPEARING_LABEL}"
                            )
                        } else {
                            format!("New messages vanish after {DISAPPEARING_LABEL}")
                        })
                        .clicked()
                    {
                        if armed {
                            state.disappearing.remove(&conversation_id);
                        } else {
                            state.disappearing.insert(conversation_id);
                        }
                    }
                }

                // The microphone and the paperclip: the two things a message can be besides
                // text, one press away from the field that types the third.
                if ui
                    .button(RichText::new("\u{1F3A4}").font(egui::FontId::proportional(font::BODY)))
                    .on_hover_text("Record a voice note")
                    .clicked()
                {
                    context.issue(Command::StartRecording {
                        conversation_id,
                        expires_in_ms: state
                            .disappearing
                            .contains(&conversation_id)
                            .then_some(DISAPPEARING_MS),
                    });
                }
                if ui
                    .button(RichText::new("\u{1F4CE}").font(egui::FontId::proportional(font::BODY)))
                    .on_hover_text("Attach a file")
                    .clicked()
                {
                    let panel = state.attach.entry(conversation_id).or_default();
                    panel.open = !panel.open;
                }
            });

            // The paperclip's fold-out. egui offers no file dialog, so the source of an
            // attachment is typed — the same trade the avatar picker makes, and the same
            // "/path" hint the document save row gives.
            let panel = state.attach.entry(conversation_id).or_default();
            if panel.open {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut panel.path)
                            .hint_text("/path/to/file")
                            .desired_width(240.0),
                    );
                    if ui.button("Attach").clicked() {
                        let typed = panel.path.trim().to_owned();
                        if !typed.is_empty() {
                            context.issue(Command::SendAttachment {
                                conversation_id,
                                path: PathBuf::from(typed),
                                expires_in_ms: state
                                    .disappearing
                                    .contains(&conversation_id)
                                    .then_some(DISAPPEARING_MS),
                            });
                            panel.open = false;
                            panel.path.clear();
                        }
                    }
                });
            }
        });
}

/// A wider elision for the header, which has more room than a list row.
fn widelide(text: &str) -> String {
    widgets::elide(text, 42)
}

/// A day label for the thread separators.
///
/// Derived from the millisecond timestamp arithmetically rather than through a calendar library,
/// because the only requirement is that consecutive messages on the same day share a label. A full
/// locale-aware date is a larger dependency than the feature justifies.
fn day_label(at: migo_core::Timestamp) -> String {
    let days = at.as_millis() / 86_400_000;
    let now = migo_core::Timestamp::now().as_millis() / 86_400_000;
    match now.saturating_sub(days) {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        n if n < 7 => format!("{n} days ago"),
        _ => model::date(at),
    }
}

/// A duration as `m:ss`.
fn human_duration(ms: u32) -> String {
    let total = ms / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verification block's own filing rules in one test: a peer's devices file by device,
    /// the rows stay ordered by device id so the block never reorders itself while someone is
    /// reading it, a re-observation updates the number, and the key-change warning is sticky —
    /// the worker reports a change once, and a window that reopened mid-change must not drop
    /// the sentence that says verify.
    #[test]
    fn peer_identities_file_by_device_and_keep_their_warnings_sticky() {
        let mut state = ChatState::default();
        let peer = Id::from_bytes([7; 16]);
        let phone = Id::from_bytes([1; 16]);
        let laptop = Id::from_bytes([2; 16]);

        state.note_peer_identity(peer, laptop, "55555".to_owned(), false);
        state.note_peer_identity(peer, phone, "44444".to_owned(), false);
        let devices = state.peers.get(&peer).expect("the peer is filed");
        assert_eq!(
            devices.iter().map(|d| d.device_id).collect::<Vec<_>>(),
            vec![phone, laptop]
        );

        state.note_peer_identity(peer, phone, "99999".to_owned(), true);
        // The same new number again, reported unchanged — the warning stays anyway.
        state.note_peer_identity(peer, phone, "99999".to_owned(), false);
        let phone_row = state
            .peers
            .get(&peer)
            .expect("the peer is filed")
            .iter()
            .find(|d| d.device_id == phone)
            .expect("the device is filed");
        assert_eq!(phone_row.safety_number, "99999");
        assert!(phone_row.changed);
    }

    /// A message to react to, and a reaction to it, in the shapes the worker delivers.
    fn reaction_fixture() -> (Message, Message) {
        let conversation = Id::from_bytes([3; 16]);
        let target = Id::from_bytes([4; 16]);
        let target_message = Message {
            message_id: target,
            conversation_id: conversation,
            seq: 1,
            sender_id: Id::from_bytes([9; 16]),
            outgoing: false,
            body: Body::Text("a message to react to".to_owned()),
            sent_at: migo_core::Timestamp::from_unix_ms(1_000),
            delivery: Delivery::Received,
            deleted: false,
            edited: false,
            expires_at: None,
        };
        let reaction = Message {
            message_id: Id::from_bytes([5; 16]),
            conversation_id: conversation,
            seq: 2,
            sender_id: Id::from_bytes([9; 16]),
            outgoing: false,
            body: Body::Reaction {
                emoji: "\u{1F44D}".to_owned(),
                target,
            },
            sent_at: migo_core::Timestamp::from_unix_ms(2_000),
            delivery: Delivery::Received,
            deleted: false,
            edited: false,
            expires_at: None,
        };
        (target_message, reaction)
    }

    /// The reaction filing rules in one test: a reaction lands as a chip on its target,
    /// never as a row; the same sender's same emoji twice is one chip (an echo racing a
    /// re-fetch), while a second emoji from the same sender is a second chip; and a
    /// reaction to a message that has not arrived yet still files, so the chip is there
    /// when the target lands.
    #[test]
    fn reactions_file_as_chips_never_rows() {
        let (target_message, reaction) = reaction_fixture();
        let mut state = ChatState::default();
        let conversation = target_message.conversation_id;
        let target = target_message.message_id;
        let sender = reaction.sender_id;

        // The reaction arrives before its target — history out of order, or the target
        // still behind a page boundary. The chip files anyway.
        state.absorb(reaction.clone());
        assert_eq!(
            state.reactions.get(&target),
            Some(&vec![(sender, "\u{1F44D}".to_owned())])
        );
        assert!(!state.messages.contains_key(&conversation));

        // The same reaction again — an echo, or a re-fetch. One chip, not two.
        state.absorb(reaction.clone());
        assert_eq!(
            state.reactions.get(&target).map_or(0, |chips| chips.len()),
            1
        );

        // A second emoji from the same sender is a second chip.
        let mut second = reaction;
        second.body = Body::Reaction {
            emoji: "\u{2764}\u{FE0F}".to_owned(),
            target,
        };
        state.absorb(second);
        assert_eq!(
            state.reactions.get(&target).map_or(0, |chips| chips.len()),
            2
        );

        // The target itself arrives afterwards and lands as the one row it is.
        state.absorb(target_message);
        assert_eq!(state.messages.get(&conversation).map(Vec::len), Some(1));
    }

    /// A message in the shape the store holds, for the search tests: only the parts the
    /// filter reads vary; the rest is the worker's own delivery.
    fn search_message(seq: u64, body: Body) -> Message {
        Message {
            message_id: Id::from_bytes([seq as u8; 16]),
            conversation_id: Id::from_bytes([3; 16]),
            seq,
            sender_id: Id::from_bytes([9; 16]),
            outgoing: false,
            body,
            sent_at: migo_core::Timestamp::from_unix_ms(1_000),
            delivery: Delivery::Received,
            deleted: false,
            edited: false,
            expires_at: None,
        }
    }

    /// The thread search's own vocabulary, pinned: the needle is the query trimmed and
    /// lowercased — or absent, when nothing was really asked — and only a text body ever
    /// answers. A media caption is a label on an object, not words someone wrote, so it does
    /// not match even when it says the very thing being searched for; the web client's filter
    /// makes the same cut, so the same thread searches the same on every screen it is read on.
    #[test]
    fn the_needle_trims_lowercases_and_only_text_answers() {
        // Nothing really asked: empty, or only whitespace.
        assert_eq!(search_needle(""), None);
        assert_eq!(search_needle("   "), None);
        assert_eq!(search_needle(" \t "), None);
        // A real question: trimmed of its edges, lowered for the case-insensitive compare.
        assert_eq!(search_needle("  Hello  "), Some("hello".to_owned()));

        let text = Body::Text("The Quarterly Report".to_owned());
        assert!(body_matches("quarterly", &text), "case does not matter");
        assert!(body_matches("the", &text));
        assert!(
            !body_matches("weekly", &text),
            "a substring that is not there"
        );

        // Words someone did not write: a caption on an attachment, a voice note, a reaction,
        // a body a newer peer sent. None of them answer, whatever they carry.
        let media = Body::Media {
            media_id: Id::from_bytes([1; 16]),
            mime_type: "image/png".to_owned(),
            size_bytes: 1,
            width: None,
            height: None,
            caption: Some("the quarterly report".to_owned()),
        };
        assert!(
            !body_matches("quarterly", &media),
            "a caption is not a match"
        );
        let voice = Body::VoiceNote {
            media_id: Id::from_bytes([2; 16]),
            duration_ms: 1_500,
        };
        assert!(!body_matches("1", &voice));
        let reaction = Body::Reaction {
            emoji: "\u{1F44D}".to_owned(),
            target: Id::from_bytes([4; 16]),
        };
        assert!(!body_matches("\u{1F44D}", &reaction));
        assert!(!body_matches(
            "anything",
            &Body::Unsupported { content_type: 99 }
        ));
    }

    /// The filter as the thread composes it: rows the needle does not answer are skipped in
    /// place, never re-gathered — the store is already sequence-sorted, and a search must be
    /// a narrower view of that order, not a second opinion about it. An absent needle is the
    /// loop's own `None` arm: the whole thread, untouched.
    #[test]
    fn the_filter_skips_in_place_and_keeps_the_thread_order() {
        // An array, not a `vec!`: four fixed rows, and the filter borrows them, never grows.
        let thread = [
            search_message(1, Body::Text("alpha".to_owned())),
            search_message(2, Body::Text("Beta report".to_owned())),
            search_message(
                3,
                Body::Media {
                    media_id: Id::from_bytes([1; 16]),
                    mime_type: "image/png".to_owned(),
                    size_bytes: 1,
                    width: None,
                    height: None,
                    caption: None,
                },
            ),
            search_message(4, Body::Text("the report, again".to_owned())),
        ];

        let needle = search_needle("  REPORT ").expect("a real question");
        let survivors: Vec<u64> = thread
            .iter()
            .filter(|message| body_matches(&needle, &message.body))
            .map(|message| message.seq)
            .collect();
        // The two text rows that answer, in the store's own order — and only those.
        assert_eq!(survivors, vec![2, 4]);

        // No question asked: nothing is filtered, whatever the panel's field holds.
        assert!(search_needle("   ").is_none());
    }
}
