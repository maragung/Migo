//! The chat screen: a conversation list, a thread, and a composer.
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
    /// The composer's contents for the open conversation.
    pub draft: String,
    /// The account id typed into the new-conversation field.
    pub new_peer: String,
    /// Whether the new-conversation row is showing.
    pub composing_new: bool,
    /// Whether the account panel is open.
    pub account_open: bool,
    /// The last typing state reported, so a keystroke does not send one frame per character.
    pub typing_sent: bool,
    /// True until the first message that must be scrolled into view has been.
    pub scroll_to_end: bool,
}

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

    /// The highest sequence number held for a conversation, for the next sync request.
    pub fn have_seq(&self, conversation_id: Id) -> u64 {
        self.messages
            .get(&conversation_id)
            .and_then(|thread| thread.iter().map(|m| m.seq).max())
            .unwrap_or(0)
    }
}

/// Orders delivery states so a later one never overwrites an earlier one.
/// One conversation list row, owned.
///
/// The list is drawn from data resolved out of [`ChatState`] before the loop starts, because opening a
/// conversation writes back into that same state. Owning the strings is what makes the two safe to do
/// in one pass, and naming the shape keeps the six values from becoming an unreadable tuple.
struct Row {
    conversation_id: Id,
    title: String,
    preview: Option<String>,
    time: Option<String>,
    unread: u32,
    encrypted: bool,
}

fn delivery_rank(state: Delivery) -> u8 {
    match state {
        Delivery::Failed => 0,
        Delivery::Sending => 1,
        Delivery::Sent => 2,
        Delivery::Received => 3,
    }
}

/// Draws the Chats tab: the conversation list as a pane of its own, centred, with the account
/// footer beneath it.
///
/// In the v3 shell a conversation is no longer opened *beside* this list — opening one lands a
/// closable tab on the strip and the thread takes the whole pane (see [`thread`]). The list is
/// therefore the Chats tab's whole content, and it is centred the way the reference centres it,
/// because a conversation list stretched across a maximised window is mostly empty margin.
pub fn list(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState) {
    let colors = palette(context.theme);
    let width = 560.0_f32.min(ui.available_width());
    let margin = (ui.available_width() - width) / 2.0;
    ui.horizontal(|ui| {
        ui.add_space(margin);
        ui.allocate_ui_with_layout(
            egui::vec2(width, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| list_pane(ui, context, state),
        );
    });
    // Hairlines framing the centred column, so it reads as a pane rather than as a float.
    let height = ui.min_rect().height();
    let top = ui.min_rect().top();
    for at in [margin - 1.0, margin + width] {
        if at >= 0.0 {
            let rect = egui::Rect::from_min_size(egui::pos2(at, top), egui::vec2(1.0, height));
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::ZERO, colors.border);
        }
    }
}

/// The open conversation as its own tab: header, messages, composer, with nothing beside them.
pub fn thread(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState) {
    thread_pane(ui, context, state);
}

/// The conversation list, its header, and the new-conversation row.
fn list_pane(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState) {
    let colors = palette(context.theme);
    let me = context.account.map(|a| a.account_id);

    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        ui.label(
            RichText::new("Conversations")
                .font(egui::FontId::proportional(font::SUBTITLE))
                .color(colors.text)
                .strong(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui
                .button(RichText::new("+").font(egui::FontId::proportional(font::TITLE)))
                .on_hover_text("Start a conversation")
                .clicked()
            {
                state.composing_new = !state.composing_new;
            }
        });
    });
    ui.add_space(space::SM);

    if state.composing_new {
        ui.horizontal(|ui| {
            ui.add_space(space::MD);
            let response = ui.add(
                egui::TextEdit::singleline(&mut state.new_peer)
                    .hint_text("account id")
                    .desired_width(ui.available_width() - space::XXL - space::MD),
            );
            let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (ui.button("Go").clicked() || submitted) && !state.new_peer.trim().is_empty() {
                context.issue(Command::StartDirect {
                    username: state.new_peer.trim().to_owned(),
                });
                state.new_peer.clear();
                state.composing_new = false;
            }
        });
        ui.add_space(space::SM);
    }

    widgets::divider(ui, context.theme);

    let footer = 56.0;
    let list_height = (ui.available_height() - footer).max(0.0);
    egui::ScrollArea::vertical()
        .id_salt("conversations")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.conversations.is_empty() {
                widgets::empty_state(
                    ui,
                    context.theme,
                    "No conversations yet",
                    "Use + and paste someone's account id to start one.",
                );
                return;
            }
            // Resolved first because the click handler mutates `state.selected`, and the borrow
            // checker is right to object to mutating what the loop is reading.
            let rows: Vec<Row> = state
                .conversations
                .iter()
                .map(|conversation| Row {
                    conversation_id: conversation.conversation_id,
                    title: me
                        .map(|me| conversation.display_title(me, &state.names))
                        .unwrap_or_else(|| model::short_id(conversation.conversation_id)),
                    preview: conversation.preview.clone(),
                    time: conversation.updated_at.map(model::clock),
                    unread: conversation.unread,
                    encrypted: conversation.encrypted,
                })
                .collect();

            for row in rows {
                let response = widgets::conversation_row(
                    ui,
                    context.theme,
                    widgets::RowContent {
                        title: &row.title,
                        preview: row.preview.as_deref(),
                        time: row.time.as_deref(),
                        unread: row.unread,
                        selected: state.selected == Some(row.conversation_id),
                        encrypted: row.encrypted,
                    },
                );
                if response.clicked() {
                    open(context, state, row.conversation_id);
                }
            }
        });

    widgets::divider(ui, context.theme);
    account_footer(ui, context, state);
}

/// Opens a conversation and asks for anything missing from its history.
///
/// Public because the shell's other places are doors into threads too: a Home digest row, a
/// joined room, a search hit. All of them open a conversation the one way there is.
pub fn open(context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
    state.selected = Some(conversation_id);
    state.draft.clear();
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

/// The signed-in account, its safety number, and sign-out.
fn account_footer(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState) {
    let colors = palette(context.theme);
    let Some(account) = context.account else {
        return;
    };

    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::avatar(ui, context.theme, &account.username, 28.0);
        ui.add_space(space::SM);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(widgets::elide(&account.username, 18))
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text),
            );
            let (color, label) = connection_indicator(context, &colors);
            ui.horizontal(|ui| widgets::status_dot(ui, context.theme, color, label));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui.button("\u{2699}").on_hover_text("Account").clicked() {
                state.account_open = !state.account_open;
            }
        });
    });
    ui.add_space(space::SM);

    if state.account_open {
        egui::Frame::new()
            .fill(colors.surface_raised)
            .inner_margin(egui::Margin::same(space::MD as i8))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Safety number")
                        .text_style(crate::theme::named(crate::theme::text_style::OVERLINE))
                        .color(colors.text_muted),
                );
                ui.add_space(space::XS);
                // Shown as a monospace block so a pair of people can read it to each other aloud and
                // compare. That comparison is the only thing that detects a substituted identity key,
                // and it is worth the four lines it costs.
                ui.label(
                    RichText::new(&account.safety_number)
                        .font(egui::FontId::monospace(font::SMALL))
                        .color(colors.text),
                );
                ui.add_space(space::XS);
                ui.label(
                    RichText::new(
                        "Compare this with the other person, in a call or in person. If it differs, \
                         stop and do not trust the conversation.",
                    )
                    .font(egui::FontId::proportional(font::TINY))
                    .color(colors.text_muted),
                );
                ui.add_space(space::MD);
                ui.label(
                    RichText::new(format!("Account {}", model::short_id(account.account_id)))
                        .font(egui::FontId::proportional(font::TINY))
                        .color(colors.text_muted),
                );
                ui.label(
                    RichText::new(format!("Device {}", model::short_id(account.device_id)))
                        .font(egui::FontId::proportional(font::TINY))
                        .color(colors.text_muted),
                );
                ui.add_space(space::MD);
                if widgets::ghost_button(ui, context.theme, "Sign out").clicked() {
                    state.account_open = false;
                    context.issue(Command::SignOut);
                }
            });
    }
}

/// The colour and word for the current connection state.
fn connection_indicator<'a>(
    context: &'a Context<'_>,
    colors: &crate::theme::Palette,
) -> (egui::Color32, &'a str) {
    match context.connection {
        model::Connection::Online => (colors.positive, "Connected"),
        model::Connection::Connecting => (colors.warning, "Connecting"),
        model::Connection::Offline => (colors.text_muted, "Offline"),
        model::Connection::Failed(_) => (colors.danger, "Disconnected"),
    }
}

/// The open conversation: header, messages, composer.
fn thread_pane(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState) {
    let Some(conversation_id) = state.selected else {
        widgets::empty_state(
            ui,
            context.theme,
            "Nothing open",
            "Pick a conversation on the left, or start a new one.",
        );
        return;
    };

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
                message_row(ui, context, message, sender.as_deref(), avatar_seed);
                ui.add_space(space::SM);
            }
            ui.add_space(space::SM);
        });

    if typing_height > 0.0 {
        typing_line(ui, context, state, conversation_id);
    }

    composer(ui, context, state, conversation_id);
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
        // Shown, not hidden. A message that cannot be decrypted still happened, and the gap it would
        // otherwise leave in the sequence numbers has no other explanation on screen. The reason is
        // safe to render: it is a short classification produced by this client, never key material and
        // never part of a plaintext.
        Body::Undecryptable(reason) => (
            format!("This message could not be decrypted on this device: {reason}."),
            BubbleTone::Problem,
        ),
    };
    let meta = format!(
        "{} {}",
        model::clock(message.sent_at),
        tick(message.delivery)
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
/// they do not already know by seeing it.
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
                let response = ui.add_enabled(
                    online,
                    egui::TextEdit::multiline(&mut state.draft)
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

                if (send_by_key || send_by_click) && !state.draft.trim().is_empty() && online {
                    let text = state.draft.trim().to_owned();
                    state.draft.clear();
                    context.issue(Command::SendText {
                        conversation_id,
                        text,
                    });
                    if state.typing_sent {
                        context.issue(Command::Typing {
                            conversation_id,
                            typing: false,
                        });
                        state.typing_sent = false;
                    }
                    // Enter leaves a newline in the buffer on some platforms; clearing after the
                    // command is queued keeps the field empty either way.
                    state.draft.clear();
                    response.request_focus();
                }

                // Typing is reported on the transition, not per keystroke. A frame-rate stream of
                // typing frames is bandwidth spent to say the same thing sixty times a second, and the
                // server would rightly rate-limit it.
                let has_text = !state.draft.trim().is_empty();
                if has_text != state.typing_sent && online {
                    context.issue(Command::Typing {
                        conversation_id,
                        typing: has_text,
                    });
                    state.typing_sent = has_text;
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
