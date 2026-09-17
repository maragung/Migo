//! The Chat List Mode's list: every conversation, one box, one selection.
//!
//! # What the list is
//!
//! Navigation Mode (see [`crate::ui::NavigationMode`]) decides what the main window's ground
//! is, and in Chat List Mode the ground is this list — the phone's home, translated to a
//! desktop: the list is the main window's Main tab (see [`crate::ui::MainTab`]), drawn under
//! the account bar and the window's four-tab strip, beside Friends, Rooms and Feed — not a
//! pane docked beside a chat and not a floating window. The list is presentation only — the
//! rows are the conversation summaries the session already holds, the unread counts are the
//! counts the rest of the client reads, and a click opens a conversation the one way there is,
//! [`crate::ui::chat::open`], so there is no second chat system to keep true. The shell mints
//! the opened conversation its own floating, closable window (the same window tabbed
//! navigation mints), and closing that window is the mode's "back": the list never went away,
//! so returning to it is not a navigation the list owes anyone.
//!
//! The search field is the list's own and filters locally, by title, because every title it
//! needs is already on the device — a round trip for a prefix the eye can match faster than
//! the wire can answer would be theatre, not search.

use egui::{RichText, Ui};
use migo_core::{Id, Timestamp};

use crate::theme::{font, palette, space};
use crate::ui::chat::{self, ChatState};
use crate::ui::{widgets, Context};

/// The list's own state.
#[derive(Debug, Default)]
pub struct ChatListState {
    /// The search box's text, kept between frames so a query survives a mode switch and a
    /// conversation change — the words were typed against the whole list, not against one
    /// thread, and neither event unasks the question they asked.
    pub query: String,
}

/// One row's worth of conversation, copied out of the chat state before the rows draw.
///
/// Copied rather than borrowed for the same reason the search screen's local half copies: a
/// click opens a conversation, which takes the chat state mutably, and an iterator's borrow
/// would otherwise outlive the click it exists to serve.
struct Row {
    conversation_id: Id,
    title: String,
    preview: Option<String>,
    time: Option<String>,
    unread: u32,
    encrypted: bool,
    /// The bot the row's title names, where it names exactly one account.
    bot: Option<Id>,
}

/// Draws the chat list: the box, then every conversation it lets through.
pub fn show(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatListState,
    chat: &mut ChatState,
) {
    let colors = palette(context.theme);
    ui.add_space(space::MD);
    widgets::header(ui, context.theme, "Chats", None);
    ui.add_space(space::SM);

    // The box, the panel's full width inside its own margins: one field, filtering as it is
    // typed, because the whole point of a list kept beside the chat is that finding a name in
    // it is instant.
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        ui.add(
            egui::TextEdit::singleline(&mut state.query)
                .hint_text("search your chats")
                .desired_width(ui.available_width() - space::MD),
        );
        ui.add_space(space::MD);
    });
    ui.add_space(space::SM);
    widgets::divider(ui, context.theme);

    // The rows, copied out before the scroll area borrows anything: a click inside the loop
    // opens a conversation, and the open wants the chat state mutably.
    let me = context
        .account
        .map(|account| account.account_id)
        .unwrap_or_default();
    let query = state.query.trim().to_lowercase();
    let rows: Vec<Row> = chat
        .conversations
        .iter()
        .filter(|conversation| {
            query.is_empty()
                || conversation
                    .display_title(me, &chat.names)
                    .to_lowercase()
                    .contains(&query)
        })
        .map(|conversation| Row {
            conversation_id: conversation.conversation_id,
            title: conversation.display_title(me, &chat.names),
            preview: conversation.preview.clone(),
            time: conversation.updated_at.map(row_time),
            unread: conversation.unread,
            encrypted: conversation.encrypted,
            // Read from the same bots map the thread's own marks come from, through the helper
            // that keeps the mark and the title in step: a row can only wear the mark when the
            // title it wears is the name of that one bot.
            bot: conversation.display_bot(me, &chat.bots),
        })
        .collect();

    let selected = chat.selected;
    // The count, quiet, under the box and above the rows (the scroll takes everything below
    // it): the one fact a person scanning for "did they all get read?" wants, stated once
    // instead of counted by hand.
    let unread: u32 = rows.iter().map(|row| row.unread).sum();
    if unread > 0 {
        ui.label(
            RichText::new(format!(
                "{unread} unread in {} chat{}",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" }
            ))
            .font(egui::FontId::proportional(font::TINY))
            .color(colors.text_muted),
        );
    }
    let mut opened: Option<Id> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if rows.is_empty() {
                if query.is_empty() {
                    widgets::empty_state(
                        ui,
                        context.theme,
                        "No chats yet",
                        "Open one from Friends, Rooms, or Search.",
                    );
                } else {
                    widgets::empty_state(
                        ui,
                        context.theme,
                        "No chat matches",
                        &format!("Nothing in your chats matches \u{201C}{query}\u{201D}."),
                    );
                }
                return;
            }
            for row in &rows {
                if widgets::conversation_row(
                    ui,
                    context.theme,
                    widgets::RowContent {
                        title: &row.title,
                        preview: row.preview.as_deref(),
                        time: row.time.as_deref(),
                        unread: row.unread,
                        selected: selected == Some(row.conversation_id),
                        encrypted: row.encrypted,
                        bot: row.bot,
                    },
                )
                .clicked()
                {
                    opened = Some(row.conversation_id);
                }
            }
        });

    // The open, after the scroll area has let go of its borrows: the same door every other
    // list of conversations uses, so the thread the shell mints a window for is opened the one
    // way there is — history asked, read watermark reported, unread cleared.
    if let Some(conversation_id) = opened {
        chat::open(context, chat, conversation_id);
    }
}

/// The row's stamp: the clock when the conversation last moved today, the date when it is
/// older. A list that stamped dates on everything would waste its one quiet column on today's
/// chats, and one that stamped times on last week's would lie about when.
fn row_time(at: Timestamp) -> String {
    let day = |ts: Timestamp| ts.as_unix_ms().div_euclid(86_400_000);
    if day(at) == day(Timestamp::now()) {
        crate::model::clock(at)
    } else {
        crate::model::date(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stamp switches at midnight, not at some "recent enough" blur: today's conversations
    /// name the time, older ones name the day. Pinned at two days back rather than one so the
    /// test cannot straddle a midnight either side of its own two `now` reads.
    #[test]
    fn the_row_stamp_names_the_time_today_and_the_date_before() {
        let now = Timestamp::now();
        assert_eq!(row_time(now), crate::model::clock(now));
        let ago = Timestamp::from_unix_ms(now.as_unix_ms() - 2 * 86_400_000);
        assert_eq!(row_time(ago), crate::model::date(ago));
    }
}
