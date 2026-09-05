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

use std::collections::HashMap;

use egui::{Align, Key, Layout, RichText, Ui};
use migo_core::Id;

use crate::model::{self, Body, Conversation, Delivery, Message};
use crate::net::Command;
use crate::theme::{font, palette, space};
use crate::ui::widgets::{self, BubbleTone};
use crate::ui::Context;

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
}

/// The open conversation: header, messages, composer.
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

    let composer_height = 64.0;
    let typing_height = if state
        .typing
        .get(&conversation_id)
        .is_some_and(|w| !w.is_empty())
    {
        18.0
    } else {
        0.0
    };
    let list_height = (ui.available_height() - composer_height - typing_height).max(0.0);

    egui::ScrollArea::vertical()
        .id_salt(("thread", conversation_id.to_string()))
        .max_height(list_height)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.add_space(space::MD);
            let empty = Vec::new();
            let thread = state.messages.get(&conversation_id).unwrap_or(&empty);
            if thread.is_empty() {
                widgets::empty_state(
                    ui,
                    context.theme,
                    "No messages yet",
                    "Anything you send here is encrypted on this device before it leaves.",
                );
                return;
            }
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
                message_row(ui, context, message, sender.as_deref(), avatar_seed, read);
                ui.add_space(space::SM);
            }
            ui.add_space(space::SM);
        });

    // The room's own life, after the messages: who came, who went, who dropped. A live tail, not
    // history — the notices arrived while the room was open, in arrival order, and a reader who
    // wants the durable roster opens the rooms pane.
    room_notices(ui, context, state, conversation_id);

    if typing_height > 0.0 {
        typing_line(ui, context, state, conversation_id);
    }

    composer(ui, context, state, conversation_id);
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
fn thread_header(ui: &mut Ui, context: &Context<'_>, state: &ChatState, conversation_id: Id) {
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
fn message_row(
    ui: &mut Ui,
    context: &Context<'_>,
    message: &Message,
    sender: Option<&str>,
    avatar_seed: Option<&str>,
    read: bool,
) {
    let (text, tone) = match &message.body {
        Body::Text(text) => (text.clone(), BubbleTone::Normal),
        Body::Media {
            mime_type,
            size_bytes,
        } => (
            format!("Attachment ({mime_type}, {})", human_bytes(*size_bytes)),
            BubbleTone::Normal,
        ),
        Body::VoiceNote { duration_ms } => (
            format!("Voice note ({})", human_duration(*duration_ms)),
            BubbleTone::Normal,
        ),
        // Named by target rather than drawn on the message it reacts to: attaching it would mean
        // finding that message, which may not have arrived or may be older than the loaded history.
        // A reaction whose target is off screen still says something; one silently dropped does not.
        Body::Reaction { emoji, target } => (
            format!("Reacted {emoji} to message {}", model::short_id(*target)),
            BubbleTone::Normal,
        ),
        Body::Unsupported { content_type } => (
            format!("Unsupported message (type {content_type}). Update Migo to read it."),
            BubbleTone::Problem,
        ),
    };
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
            |ui| widgets::bubble(ui, context.theme, &text, &meta, message.outgoing, tone),
        );
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
fn composer(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
    let colors = palette(context.theme);
    let online = context.connection.is_online();

    egui::Frame::new()
        .fill(colors.surface)
        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let send_width = 56.0;
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
            });
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
