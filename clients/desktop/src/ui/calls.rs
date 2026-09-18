//! The Calls place: the calls this account was party to that have already ended.
//!
//! The one call surface in this client that reads the past. Every other call surface is about a
//! call that is happening, and the server's listing is exactly that — a filter on calls that are
//! still alive — so an ended call falls out of it the moment it ends. This pane holds the other
//! read's answer, and the shape is a paging one: rows accumulate behind the newest, because that
//! is the order the server answers in and the order the pane draws.
//!
//! Everything drawn here is the server's own row rendered. Whether a call was answered is the
//! answer the node recorded when the callee picked up, never a claim either party made
//! afterwards, so a call this account missed reads as missed no matter what was said on the wire.

use egui::{Align, FontId, Layout, RichText, Ui};

use crate::net::{Command, CALL_HISTORY_PAGE};
use crate::theme::{font, palette, space, Theme};
use crate::ui::chat::ChatState;
use crate::ui::{widgets, Context};

/// The wire's own numbering for the three vocabularies a row is read through.
///
/// Kept as the bare numbers the schema spells, in the same shape the call plane's own constants
/// are kept in this client: the row arrives as integers, and a name for each is what the match
/// below reads against — a number this build does not know falls to the last arm rather than
/// being guessed at.
const KIND_GROUP: u32 = 1;
const DIRECTION_OUTGOING: u32 = 0;
const OUTCOME_ANSWERED: u32 = 0;
const OUTCOME_MISSED: u32 = 1;
const OUTCOME_DECLINED: u32 = 2;
const OUTCOME_BUSY: u32 = 3;
const OUTCOME_CANCELLED: u32 = 4;
const OUTCOME_FAILED: u32 = 5;

/// A day in milliseconds, for deciding whether a row's time reads as a clock or a date.
const DAY_MS: i64 = 86_400_000;

/// The place's state.
#[derive(Debug, Default)]
pub struct CallsState {
    /// The rows read so far, newest first.
    pub rows: Vec<migo_protocol::CallHistoryEntry>,
    /// True once a page has landed.
    ///
    /// Held apart from a non-empty row list, because "no calls yet" and "nothing read yet" are
    /// different sentences and a pane that showed the first while it meant the second would be
    /// telling the user something it does not know.
    pub loaded: bool,
    /// True once a page came back shorter than asked for: the end of the history.
    pub complete: bool,
    /// True while a page is in flight, so a second press cannot race the first.
    pub busy: bool,
    /// True when the page in flight is the one behind the rows held rather than a fresh first.
    appending: bool,
}

impl CallsState {
    /// Marks the next page as a fresh first one, and the pane busy.
    pub fn begin_first(&mut self) {
        self.appending = false;
        self.busy = true;
    }

    /// Marks the next page as the one behind the rows held, and the pane busy.
    fn begin_older(&mut self) {
        self.appending = true;
        self.busy = true;
    }

    /// The cursor the next page is asked with: the end time of the oldest row held.
    ///
    /// Exclusive on the server, so the row this names never comes back and pages cannot overlap
    /// however slowly they are walked.
    #[must_use]
    pub fn cursor(&self) -> Option<migo_core::Timestamp> {
        self.rows.last().map(|row| row.ended_at)
    }

    /// A page came back.
    ///
    /// The end of the history is a page shorter than the one asked for, which is the only signal
    /// there is: the server answers with rows and not with a total, so there is nothing to count
    /// against and a full page is the only reason the button that asks for more is offered.
    pub fn file_page(&mut self, rows: Vec<migo_protocol::CallHistoryEntry>) {
        let complete = rows.len() < CALL_HISTORY_PAGE as usize;
        if self.appending {
            self.rows.extend(rows);
        } else {
            self.rows = rows;
        }
        self.appending = false;
        self.busy = false;
        self.loaded = true;
        self.complete = complete;
    }
}

/// Draws the Calls place.
pub fn show(ui: &mut Ui, context: &mut Context<'_>, state: &mut CallsState, chat: &ChatState) {
    let me = context.account.map(|account| account.account_id);
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        widgets::header(ui, context.theme, "Calls", Some("What your calls came to"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::MD);
            if ui
                .add_enabled(!state.busy, egui::Button::new("Refresh"))
                .clicked()
            {
                state.begin_first();
                context.issue(Command::CallHistory { before: None });
            }
        });
    });
    ui.add_space(space::SM);
    widgets::divider(ui, context.theme);

    if state.rows.is_empty() {
        widgets::empty_state(
            ui,
            context.theme,
            if state.loaded {
                "No calls yet"
            } else {
                "Loading…"
            },
            if state.loaded {
                "A call you place, answer, or miss will land here."
            } else {
                ""
            },
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(space::XS);
            for row in &state.rows {
                call_row(ui, context.theme, row, chat, me);
            }
            if !state.complete {
                ui.add_space(space::SM);
                let cursor = state.cursor();
                if ui
                    .add_enabled(!state.busy, egui::Button::new("Load older calls"))
                    .clicked()
                {
                    if let Some(before) = cursor {
                        state.begin_older();
                        context.issue(Command::CallHistory {
                            before: Some(before),
                        });
                    }
                }
            }
            ui.add_space(space::SM);
            ui.label(
                RichText::new(
                    "This is a record of who called whom and how it ended. No call is recorded.",
                )
                .font(FontId::proportional(font::TINY))
                .color(palette(context.theme).text_muted),
            );
            ui.add_space(space::XL);
        });
}

/// One history row: the direction, who, how it ended, and — when it connected — how long.
fn call_row(
    ui: &mut Ui,
    theme: Theme,
    row: &migo_protocol::CallHistoryEntry,
    chat: &ChatState,
    me: Option<migo_core::Id>,
) {
    let colors = palette(theme);
    let outgoing = row.direction == DIRECTION_OUTGOING;
    let group = row.kind == KIND_GROUP;
    let answered = row.outcome == OUTCOME_ANSWERED;

    // The conversation's own title when the pane knows the conversation, and what the row can say
    // on its own otherwise. A group row is never a bare id: it is a call with a roster, whether
    // or not this session ever saw the conversation it happened in.
    let name = chat
        .conversations
        .iter()
        .find(|conversation| conversation.conversation_id == row.conversation_id)
        .and_then(|conversation| me.map(|me| conversation.display_title(me, &chat.names)))
        .or_else(|| chat.names.get(&row.peer_id).cloned())
        .unwrap_or_else(|| {
            if group {
                "Group call".to_owned()
            } else {
                crate::model::short_id(row.peer_id)
            }
        });

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 44.0), egui::Sense::hover());
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(space::MD, space::XS * 0.5)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    inner.label(
        RichText::new(if outgoing { "\u{2197}" } else { "\u{2199}" })
            .font(FontId::proportional(font::BODY))
            .color(colors.text_muted),
    );
    inner.add_space(space::SM);
    inner.vertical(|ui| {
        ui.label(
            RichText::new(widgets::elide(&name, 48))
                .font(FontId::proportional(font::BODY))
                .color(colors.text),
        );
        ui.label(
            RichText::new(sentence(row, outgoing, group, answered))
                .font(FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
    });
    inner.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(
            RichText::new(when(row.ended_at))
                .font(FontId::proportional(font::TINY))
                .color(colors.text_muted),
        );
    });
}

/// What the row reads as, from facts the row carries.
///
/// Direction is what makes it honest: an outgoing call that rang out was not missed by this
/// account, it went unanswered by the other party, and one word for both would be telling the
/// user they missed a call they placed.
fn sentence(
    row: &migo_protocol::CallHistoryEntry,
    outgoing: bool,
    group: bool,
    answered: bool,
) -> String {
    let how = match row.outcome {
        OUTCOME_ANSWERED => {
            if outgoing {
                "Outgoing"
            } else {
                "Incoming"
            }
        }
        OUTCOME_MISSED => {
            if outgoing {
                "No answer"
            } else {
                "Missed"
            }
        }
        OUTCOME_DECLINED => "Declined",
        OUTCOME_BUSY => {
            if outgoing {
                "Busy"
            } else {
                "Missed on another call"
            }
        }
        OUTCOME_CANCELLED => "Cancelled",
        OUTCOME_FAILED => "Failed",
        // A number this build does not know is a server ahead of it; the row is still a call, so
        // it is drawn as one rather than dropped.
        _ => "Call",
    };
    let mut parts = vec![how.to_owned()];
    // A duration only when the call connected: an unanswered ring has no length to report, and a
    // zero would read as a call that lasted no time rather than one that never happened.
    if answered {
        if let Some(answered_at) = row.answered_at {
            parts.push(duration(
                row.ended_at.as_unix_ms() - answered_at.as_unix_ms(),
            ));
        }
    }
    // A group row counts the seats the roster held over the call's life rather than its size at
    // any instant, which is the same distinction the wire's own page makes.
    if group {
        if let Some(count) = row.participant_count {
            parts.push(format!("{count} people"));
        }
    }
    parts.join(" \u{00B7} ")
}

/// A call's length as a person reads it: seconds under a minute, then minutes, then hours.
fn duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{}m {}s", minutes, seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// When a call ended, as a person reads it: a clock time for today, a date before that.
///
/// The two formatters are the model's own, shared with the thread's day separators and the
/// settings screen's session rows, so a time never reads differently in the places that name it.
fn when(ended: migo_core::Timestamp) -> String {
    let age = migo_core::Timestamp::now().as_unix_ms() - ended.as_unix_ms();
    if (0..DAY_MS).contains(&age) {
        crate::model::clock(ended)
    } else {
        crate::model::date(ended)
    }
}
