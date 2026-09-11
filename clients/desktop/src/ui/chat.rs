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

use crate::model::{self, Body, Conversation, Delivery, Message};
use crate::net::Command;
use crate::theme::{font, palette, space};
use crate::ui::widgets::{self, BubbleTone};
use crate::ui::Context;

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
            return;
        }
        thread.push(message);
        thread.sort_by_key(|m| (m.seq, m.sent_at.as_millis()));
        self.scroll_to_end = true;
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

    // Read before the scroll area borrows `state`, and by member count rather than by conversation
    // kind: a group of two reads like a direct chat and should look like one.
    let conversation = state
        .conversations
        .iter()
        .find(|c| c.conversation_id == conversation_id);
    let group = conversation.is_some_and(|c| c.members.len() > 2);
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
            composer(ui, context, state, conversation_id);
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
                for message in thread {
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
                    );
                    ui.add_space(space::SM);
                }
            }
            // The room's own life, as the scroll's final lines: who came, who went, who dropped.
            // A live tail, not history — the notices arrived while the room was open, in arrival
            // order, and a reader who wants the durable roster opens the rooms pane.
            room_notices(ui, context, state, conversation_id);
            ui.add_space(space::SM);
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

/// The compact header over the open conversation: title, encryption state, and the counts that
/// are true of the whole thread rather than of any one message in it.
fn thread_header(ui: &mut Ui, context: &mut Context<'_>, state: &ChatState, conversation_id: Id) {
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
            if conversation.members.len() > 2 {
                widgets::pill(
                    ui,
                    &format!("{} members", conversation.members.len()),
                    colors.text_muted,
                    colors.surface_raised,
                );
            }
        });
    });
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
fn message_row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    message: &Message,
    sender: Option<&str>,
    avatar_seed: Option<&str>,
    read: bool,
    media: &mut MediaState,
    reactions: &mut HashMap<Id, Vec<(Id, String)>>,
) {
    let meta = format!(
        "{} {}{}",
        model::clock(message.sent_at),
        tick(message.delivery),
        if read { " \u{2713}\u{2713}" } else { "" }
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
            },
        );
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
            human_bytes(size_bytes)
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
fn composer(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
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

                // The microphone and the paperclip: the two things a message can be besides
                // text, one press away from the field that types the third.
                if ui
                    .button(RichText::new("\u{1F3A4}").font(egui::FontId::proportional(font::BODY)))
                    .on_hover_text("Record a voice note")
                    .clicked()
                {
                    context.issue(Command::StartRecording { conversation_id });
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

/// A byte count in the largest unit that keeps it under four digits.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
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
            sent_at: migo_core::Timestamp::from_millis(1_000),
            delivery: Delivery::Received,
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
            sent_at: migo_core::Timestamp::from_millis(2_000),
            delivery: Delivery::Received,
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
        assert!(state.messages.get(&conversation).is_none());

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
}
