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

use egui::{Align, Align2, Key, Layout, RichText, Ui};
use migo_core::Id;
use migo_protocol::ConversationRole;

use crate::model::{self, Body, Conversation, Delivery, Message};
use crate::net::Command;
use crate::theme::{font, palette, radius, space, Palette};
use crate::ui::packs;
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
    /// Group calls this device is seated in, per conversation: the participant count, one seat
    /// per account. The header's button reads this — Join when the conversation has no seat,
    /// Leave with the count when it has — and the net worker's own seat events keep it true.
    pub group_calls: HashMap<Id, u32>,
    /// When each typing entry expires, keyed by `(conversation, typer)`.
    ///
    /// The local timeout brief section 15 demands: a `Start` that is never
    /// followed by a `Stop` — a typer whose app died mid-word — must still
    /// clear itself, because the server's own backstop can lag the deadline by
    /// a tick and a client that waited for a frame would wait forever for a
    /// client that cannot send one. The web client arms the same four seconds.
    pub typing_expires: HashMap<(Id, Id), std::time::Instant>,
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
    /// The live voice-note recording, `None` when none runs. The conversation id rides
    /// along so a bar left behind by a window switch clears when its own conversation's
    /// recording ends, not whichever one is open; the elapsed and bars are the worker's own
    /// tick, so the clock stands still through a pause exactly as the capture does.
    pub recording: Option<RecordingView>,
    /// A finished note waiting on the composer's word — the preview, an undo window's
    /// restore, or a draft recovered after an app death. One at a time, like the recording.
    pub note_preview: Option<NotePreview>,
    /// The conversation whose discarded note the undo window still holds, if one stands.
    /// The window's deadline is the worker's — it owns the bytes — so the chip is withdrawn
    /// by the event that says the window closed, not by a clock the UI keeps.
    pub note_discard_undo: Option<Id>,
    /// Whether the microphone button is under a press — the hold mode's own state, kept here
    /// because the gesture belongs to the surface that started it. The press started the
    /// recording already; what the release decides is the hold's whole vocabulary.
    pub mic_held: bool,
    /// Where the held press began, in screen points. The slides are measured against the
    /// press's own origin rather than the button's frame, so a layout shift mid-press — the
    /// composer trading its field for the held row — cannot move the goal.
    pub mic_press_origin: Option<egui::Pos2>,
    /// When the held press began, for the quick-tap judgement the release makes.
    pub mic_hold_started: Option<std::time::Instant>,
    /// Whether the held press has slid into its cancel zone: the release then cancels
    /// rather than sends, and the hint says so in the release's own colour.
    pub mic_cancel_slide: bool,
    /// The attach menu's own state, per conversation: whether the file control's menu is
    /// open, whether one of its pick doors left the path row standing, and the path typed
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
    /// The load-earlier row's own state, per conversation — the row a budget-stopped walk
    /// arms at the top of the scroll, the same row the web client's `hasEarlier` draws. Kept
    /// per conversation because the row belongs to the thread's history, not to whichever
    /// window is showing it.
    pub earlier: HashMap<Id, EarlierState>,
    /// The member whose options menu is open, per conversation: clicking a roster row opens
    /// the options that row hides — view profile, gift, the vote, and the founder's levers —
    /// rather than laying them out beside every name. One menu per conversation at a time,
    /// because two open menus would be two members' levers on screen at once and a click
    /// aimed at one could land in the other's.
    pub member_menus: HashMap<Id, Id>,
    /// The gift catalogue as the member menu's picker reads it — the same shelves the wallet's
    /// shop shows, filed by the same event, so a gift sent from a group costs what the wallet
    /// said it would. Empty until a read lands; the picker's first open is the ask.
    pub gifts: Vec<crate::model::GiftRow>,
    /// The gift picker that stands, when one stands: who the gift is for, and the one
    /// idempotency key the pick minted. Opened by a roster row's member or by the header's gift
    /// control, and one at a time on the whole screen like the new-group form — two pickers
    /// would be two gifts half-chosen.
    pub gifting: Option<GiftPick>,
    /// The member profile a roster menu opened, when the card has answered: the card itself,
    /// the conversation whose window asked for it — the window that draws it, so a view
    /// never outlives the group it was opened from — and the standing facts that arrive on
    /// their own schedule beside it.
    pub member_profile: Option<MemberProfileView>,
    /// The account's owned catalogue codes, as the composer's emoticon picker reads them.
    /// `None` is "not asked yet" — the picker waits rather than showing a free-only set that
    /// would read as "you own nothing"; the first open of a picker is the ask, and a pack
    /// bought elsewhere lands on the next open.
    pub owned_packs: Option<HashSet<String>>,
    /// The emoticon/sticker picker's open state, per conversation: the composer's smile folds
    /// the picker out above the row, the way the attach menu folds under it, and the tab it
    /// was last left on stays with the conversation's own picker.
    pub emoticon_pickers: HashMap<Id, EmoticonPanel>,
}

/// The member profile view's own state: the card the wire answered, the conversation whose
/// window asked for it, and the standing facts that answer beside it — each on its own
/// schedule, each degrading to absence, because a profile without its level lines is still a
/// profile and a card must never break waiting on a fact.
#[derive(Clone)]
pub struct MemberProfileView {
    /// The conversation whose roster menu opened the view. The window draws the card, so the
    /// card files against the conversation — a profile view that outlived its window would be
    /// a card with nowhere to belong.
    pub conversation_id: Id,
    /// The card itself, as the profile fetch answered it.
    pub card: crate::model::MemberCard,
    /// The person's XP standing, when the economy answered: level, totals, and the bar's two
    /// ends. Absent draws no level lines at all — no guess, no zero, no silence pretending
    /// to be a number.
    pub progression: Option<crate::model::Progression>,
    /// The badges the person holds, with the days they were earned. An empty row renders
    /// nothing, the same absence the web card draws.
    pub badges: Vec<crate::model::BadgeRow>,
    /// The person's position on the XP board's first page. `None` is both "not answered yet"
    /// and "off the board" — the view draws no rank line for either, which is the honest
    /// sentence in both cases.
    pub rank: Option<u32>,
    /// The viewer's edge to this person, as the graph walk answered it. `None` is no edge the
    /// graph names — drawn as the "Add friend" line the web card offers, never as a guess
    /// about a stranger.
    pub relationship: Option<crate::model::RelationshipKind>,
}

/// The emoticon/sticker picker's per-conversation state: whether it stands open above the
/// composer, and which of its two tabs was last drawn.
#[derive(Default)]
pub struct EmoticonPanel {
    /// Whether the picker is showing above the composer row.
    pub open: bool,
    /// Whether the Stickers tab is the one drawn — the Emoticons tab is the picker's default,
    /// the way the web picker's is.
    pub stickers_tab: bool,
}

/// The gift picker's own state, as the header's gift control or a member menu opens it.
#[derive(Clone)]
pub struct GiftPick {
    /// The conversation whose window opened the picker — the window that draws it, the same
    /// ownership rule the profile view keeps.
    pub conversation_id: Id,
    /// The member the gift is for, when the opener already named one: a direct chat's peer or
    /// a roster row's member, pre-chosen so the picker is just the cards. `None` is the
    /// header's ask in a conversation with more than one candidate — the picker offers the
    /// members as a choice first, because a gift sent from a thread names someone in it.
    pub member: Option<Id>,
    /// The pick's idempotency key, minted when the picker opened: the same key on every send
    /// this pick attempts, so a lost reply retried is the first send again, not a second
    /// charge. The picker closes after a send — one pick is one gift — so the key is one
    /// intent's whole life.
    pub key: String,
}

/// The load-earlier row's state for one conversation.
#[derive(Default)]
pub struct EarlierState {
    /// The cursor the next downward ask starts from: the last page's `from_seq`. Zero until a
    /// page has arrived, which is the first ask's own word for "from the newest" — a
    /// budget-stopped walk is short at the tip, so the tip is where the row begins.
    pub cursor: u64,
    /// Whether a downward ask is in flight: the row draws its quiet self and a second click
    /// sends nothing, because the page the first click asked for is the row's whole answer.
    pub loading: bool,
    /// Whether older history may remain. Armed by a budget-stopped walk, withdrawn only by
    /// the row's own downward walk reaching the bottom — never by a later forward walk, which
    /// cannot know what the row already paged in below it.
    pub more: bool,
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

/// The attach menu's state for one conversation.
#[derive(Default)]
pub struct AttachPanel {
    /// Whether the file control's menu is showing under the composer.
    pub open: bool,
    /// Whether the path row a menu pick opened is standing: the menu's doors all lead to the
    /// one row, because on this client the platform's picker *is* a typed path and the worker
    /// judges the bytes the way the server will anyway.
    pub picking: bool,
    /// The path typed into the row, kept between frames so closing it on a mistake does
    /// not cost the whole path.
    pub path: String,
}

/// The live recording as the composer draws it: which conversation, how far it has run,
/// whether the speaker paused it, and the waveform's sampled bars so far. Every fact is the
/// worker's own tick restated here — the UI never measures a recording itself, so a clock
/// that stands still through a pause on the capture's side stands still on the screen too,
/// and the timer can never disagree with the cap that ends the note.
pub struct RecordingView {
    /// The conversation the microphone is speaking into.
    pub conversation_id: Id,
    /// How long the recording has run, pauses included as nothing — the worker's own count.
    pub elapsed_ms: u64,
    /// Whether the speaker paused the capture.
    pub paused: bool,
    /// The amplitude bars sampled so far, 0–255, one per tenth of a second of speech.
    pub amplitudes: Vec<u8>,
}

/// A finished note held for the composer's word: the two-step mode's Stop, an undo window's
/// restore, or a draft recovered after an app death — one face for all three, because all
/// three are the same question: send it or throw it away. The duration and waveform are the
/// sender's own measurements restated, so the bar lays out before a single audio byte is
/// read back.
pub struct NotePreview {
    /// The conversation the note was recorded into.
    pub conversation_id: Id,
    /// The note's playing time as the recorder counted it.
    pub duration_ms: u32,
    /// The fixed-width waveform the message will carry, already folded.
    pub waveform: Vec<u8>,
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
    /// How long a typing entry survives its last `Start` without a `Stop`.
    ///
    /// Four seconds, matching the web client's timer: long enough that a
    /// continuous typer's refreshes (which the protocol sends every few
    /// seconds) keep the line alive, short enough that a dead typer's ghost
    /// is gone before anybody wonders.
    pub const TYPING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

    /// Drops the typing entries whose local timeout passed, returning how long
    /// until the next one expires (so the caller can schedule exactly the
    /// repaint that will clear it).
    ///
    /// The deadline half of the typing indicator: the entry half is
    /// [`ChatState::note_typing`], and the pair is brief section 15's
    /// "penerima menerapkan timeout lokal" — the receiver ends an indicator on
    /// its own, because the typer that would have ended it may not exist
    /// anymore.
    pub fn expire_typing(&mut self, now: std::time::Instant) -> Option<std::time::Duration> {
        let expired: Vec<(Id, Id)> = self
            .typing_expires
            .iter()
            .filter(|(_, deadline)| now >= **deadline)
            .map(|(key, _)| *key)
            .collect();
        for (conversation, typer) in expired {
            self.typing_expires.remove(&(conversation, typer));
            if let Some(who) = self.typing.get_mut(&conversation) {
                who.retain(|id| *id != typer);
                if who.is_empty() {
                    self.typing.remove(&conversation);
                }
            }
        }
        self.typing_expires
            .values()
            .min()
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// Records one typing event, arming (or disarming) the local timeout that
    /// ends it.
    pub fn note_typing(&mut self, conversation: Id, typer: Id, typing: bool) {
        if typing {
            // Retain-then-push, not push alone: a repeated `Start` is a
            // refresh (the protocol's own rule), and a refresh that duplicated
            // its row would name the typer twice on the line.
            let who = self.typing.entry(conversation).or_default();
            who.retain(|id| *id != typer);
            who.push(typer);
            self.typing_expires.insert(
                (conversation, typer),
                std::time::Instant::now() + Self::TYPING_TIMEOUT,
            );
        } else {
            self.typing_expires.remove(&(conversation, typer));
            if let Some(who) = self.typing.get_mut(&conversation) {
                who.retain(|id| *id != typer);
                if who.is_empty() {
                    self.typing.remove(&conversation);
                }
            }
        }
    }

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

    /// Takes a forward walk's verdict for the thread it paged.
    ///
    /// `more` — the walk stopped at its page budget with the server still holding history
    /// above — arms the load-earlier row. On a thread that already held rows the arm is
    /// one-way, exactly the web client's rule: the pages the row itself paged in below may
    /// have already established that older history exists, and a forward walk cannot un-know
    /// that. A fresh replay's verdict is taken whole, because the walk that armed the row is
    /// the walk that can withdraw it — and a fresh replay's row starts from the newest, which
    /// is the cursor's own default.
    pub fn note_history(&mut self, conversation_id: Id, more: bool) {
        let held = self
            .messages
            .get(&conversation_id)
            .is_some_and(|thread| !thread.is_empty());
        let earlier = self.earlier.entry(conversation_id).or_default();
        // a forward page also settles the row — an earlier ask whose reply was lost to a
        // refused reconnect would otherwise sit on "loading" forever.
        earlier.loading = false;
        if held {
            earlier.more |= more;
        } else {
            earlier.more = more;
        }
    }

    /// Merges a load-earlier page and takes the page's own verdict for the row that asked.
    ///
    /// The "everything known" test runs *before* the merge, because the merge is what makes
    /// every id held: a page that changes nothing is the downward walk meeting history the
    /// thread already has — the natural end of the walk even when the server's `more` still
    /// says otherwise (a full page may be the last one, and the server cannot know what this
    /// thread holds).
    pub fn absorb_earlier(
        &mut self,
        conversation_id: Id,
        from_seq: u64,
        more: bool,
        page: Vec<Message>,
    ) {
        let all_known = !page.is_empty()
            && self.messages.get(&conversation_id).is_some_and(|thread| {
                page.iter().all(|message| {
                    thread
                        .iter()
                        .any(|held| held.message_id == message.message_id)
                })
            });
        self.absorb_history(conversation_id, page);
        let earlier = self.earlier.entry(conversation_id).or_default();
        earlier.cursor = from_seq;
        earlier.loading = false;
        earlier.more = more && !all_known;
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
    // The thread's own word for whether it holds anything decides the replay's floor — the
    // web client's `messagesRef` test, on the desktop's own terms: an empty thread replays
    // from the beginning, a held one continues from the worker's contiguous watermark. The
    // max-seq this open used to send is exactly the cursor §152 forbids: it stands above any
    // hole below it, and a sync asked from it would leave the hole unfilled for good.
    let held = state
        .messages
        .get(&conversation_id)
        .is_some_and(|thread| !thread.is_empty());
    context.issue(Command::History {
        conversation_id,
        held,
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

    // A voice-note draft the last session left behind — §179's app-death rule — is offered on
    // every open, the same door the worker checks: recovered, it becomes the preview the
    // composer would show a note it had just stopped, and the worker ignores the ask when a
    // note is already live or held. Asked unconditionally because the worker is the only one
    // who knows whether the last session died mid-recording.
    context.issue(Command::RecoverVoiceDraft { conversation_id });
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

    // The roster's fold-out: the group's people, their roles, and — behind each row's click —
    // the options a member and a founder hold. Drawn only for a group and only while the
    // header's people button left it open; the panel's own first draw is the roster read's ask.
    let roster_open = state
        .roster_open
        .get(&conversation_id)
        .copied()
        .unwrap_or(false);
    if roster_open {
        group_roster_panel(ui, context, state, conversation_id);
    }

    // The member menu's two overlays, drawn after the panes they were opened from: the gift
    // picker over everything, and the profile card the wire answered, over the window whose
    // roster asked for it. Both are this window's only while their conversation matches — a
    // picker asked for in one group is not another group's picker, and neither is a card.
    gift_picker(ui, context, state, conversation_id);
    member_profile_window(ui, context, state, conversation_id);

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
            load_earlier_row(ui, context, state, conversation_id);
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

/// The load-earlier row, at the top of the scroll: the thread's own admission that history
/// exists above what it holds, and the click that pages the next chunk of it down.
///
/// Armed by a budget-stopped catch-up walk ([`ChatState::note_history`]); withdrawn only by
/// the row's own downward walk — reaching the bottom, or paging a page the thread already
/// held whole ([`ChatState::absorb_earlier`]). While a page is in flight the row draws its
/// quiet self and the click sends nothing, because the page it is waiting for is the row's
/// whole answer.
fn load_earlier_row(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatState,
    conversation_id: Id,
) {
    let Some(earlier) = state.earlier.get_mut(&conversation_id) else {
        return;
    };
    if !earlier.more && !earlier.loading {
        return;
    }
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        if earlier.loading {
            let colors = palette(context.theme);
            ui.label(
                RichText::new("Loading earlier messages…")
                    .text_style(crate::theme::named(crate::theme::text_style::CAPTION))
                    .color(colors.text_muted),
            );
        } else {
            let cursor = earlier.cursor;
            if widgets::ghost_button(ui, context.theme, "Load earlier").clicked() {
                earlier.loading = true;
                context.issue(Command::HistoryEarlier {
                    conversation_id,
                    before_seq: cursor,
                });
            }
        }
    });
    ui.add_space(space::SM);
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

/// The group's roster panel: every member with role and mute, and — folded behind each row
/// until the row is clicked — the options a member and a founder hold.
///
/// The panel's facts are the roster the wire answered, not the conversation row's member
/// preview: roles and mutes live only on the roster, and the founder gates read them, so the
/// panel opens with an ask (the shell issues it the moment the toggle opens) and draws what
/// the answer filed. Until the answer lands the panel says so, because a roster that guessed
/// would be a list of names with wrong authority beside them.
///
/// A member row is the entry to that member's options: view profile, gift, the vote every
/// member holds, and the founder's mute terms and kick — the same menu the web client's room
/// and group rosters open on a row click, so the levers live behind one click instead of
/// beside every name. The gates are the server's own, mirrored: the founder's levers never
/// reach this account's own row, never a fellow founder's, and never the departed; the vote
/// is every member's but never aims at self or a founder.
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

    // Deferred intents, past the borrows above: a click is intent, and intent is applied
    // after the panel has finished drawing — the same patience every lever in this file is
    // given, extended to the menu's own state (the roster borrow the loop draws from would
    // otherwise fight the write).
    let mut invite_send: Option<Vec<Id>> = None;
    let mut rename_send: Option<String> = None;
    let mut mute_send: Option<(Id, Option<u64>)> = None;
    let mut kick_send: Option<Id> = None;
    let mut vote_send: Option<Id> = None;
    let mut leave = false;
    // The member menu's own intents: a row clicked (open or close its options), a profile
    // asked for, and a gift picker asked for.
    let mut menu_toggle: Option<Id> = None;
    let mut profile_ask: Option<Id> = None;
    let mut gift_ask: Option<Id> = None;

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
                // The gates, stated once and read by everything below: the server's own
                // immunity rules, mirrored so the menu says what the wire would allow. A
                // founder's levers reach only the active members below the founder — never
                // this account's own row (the server refuses a self-mute and a self-kick),
                // never a fellow founder's. The vote is every member's, with the same two
                // exclusions the server holds. The menu itself is offered on any row but
                // this account's own and the departed: a departed member is past every
                // lever, and the person themselves needs no menu to view their own card.
                let not_self = me != Some(member.account_id);
                let below_founder = member.role != ConversationRole::Founder;
                let votable = not_self && below_founder && !departed;
                let targetable = i_am_founder && votable;
                let has_menu = not_self && !departed;
                let menu_open = state
                    .member_menus
                    .get(&conversation_id)
                    .is_some_and(|open| *open == member.account_id);

                // The row is the menu's entry, so the row itself is the click: an
                // allocated strip with a hover fill, the way a room row in the directory
                // is, rather than a label that happens to sit where a button should be.
                let height = 30.0;
                let width = ui.available_width();
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
                let background = if menu_open {
                    colors.surface_selected
                } else if response.hovered() && has_menu {
                    colors.surface_hover
                } else {
                    egui::Color32::TRANSPARENT
                };
                if background != egui::Color32::TRANSPARENT {
                    ui.painter().rect_filled(
                        rect,
                        egui::CornerRadius::same(crate::theme::radius::MD),
                        background,
                    );
                }
                let mut inner = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect.shrink2(egui::vec2(space::XS, 0.0)))
                        .layout(Layout::left_to_right(Align::Center)),
                );
                widgets::avatar(&mut inner, context.theme, &name, 22.0);
                inner.add_space(space::XS);
                inner.label(
                    RichText::new(name)
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(if departed {
                            colors.text_muted
                        } else {
                            colors.text
                        }),
                );
                if member.role == ConversationRole::Founder {
                    widgets::pill(&mut inner, "founder", colors.text_muted, colors.surface);
                }
                if member.muted_until.is_some() && !departed {
                    widgets::pill(&mut inner, "muted", colors.warning, colors.surface);
                }
                if departed {
                    widgets::pill(&mut inner, "left", colors.text_muted, colors.surface);
                }
                if has_menu {
                    inner.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // The cue: a closed corner-bracket that says the row opens. The
                        // glyph is the row's own promise, so it travels with the row and
                        // not with the menu it opens.
                        ui.label(
                            RichText::new(if menu_open { "\u{25BE}" } else { "\u{25B8}" })
                                .font(egui::FontId::proportional(font::TINY))
                                .color(colors.text_muted),
                        );
                    });
                }
                if response.clicked() && has_menu {
                    menu_toggle = Some(member.account_id);
                }

                // The options themselves, folded out under the row while it is open: one
                // click on the row opens them, and the row's own facts — the role, the
                // mute — stay on the row where they were read.
                if menu_open && has_menu {
                    egui::Frame::new()
                        .fill(colors.surface)
                        .corner_radius(egui::CornerRadius::same(crate::theme::radius::MD))
                        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::XS as i8))
                        .show(ui, |ui| {
                            if quiet_action(ui, "View profile", colors.text).clicked() {
                                profile_ask = Some(member.account_id);
                            }
                            if quiet_action(ui, "Gift", colors.text)
                                .on_hover_text("Send this person a gift from the shop.")
                                .clicked()
                            {
                                gift_ask = Some(member.account_id);
                            }
                            // The vote, every member's lever, never aimed at this account
                            // and never at a founder: the same gates the server holds,
                            // mirrored so the option says what the wire would allow.
                            if votable
                                && quiet_action(ui, "Vote kick", colors.text)
                                    .on_hover_text(
                                        "Call a vote to remove this person. When half the \
                                         group agrees, they are kicked. Free — the vote costs \
                                         nothing.",
                                    )
                                    .clicked()
                            {
                                vote_send = Some(member.account_id);
                            }
                            // The founder's levers, on the rows the founder may act on: the
                            // mute as its three terms rather than one fixed hour — the same
                            // vocabulary the web panel offers — and the kick, which costs a
                            // Kick Point and says so before it is spent.
                            if targetable {
                                if member.muted_until.is_some() {
                                    if quiet_action(ui, "Unmute", colors.text)
                                        .on_hover_text("Lift this group mute now.")
                                        .clicked()
                                    {
                                        mute_send = Some((member.account_id, None));
                                    }
                                } else {
                                    for (label, term_ms) in GROUP_MUTE_TERMS_MS {
                                        if quiet_action(ui, &format!("Mute {label}"), colors.text)
                                            .on_hover_text(format!(
                                                "Silence this person for the whole group for \
                                             {label}. They keep every other right, including \
                                             the vote."
                                            ))
                                            .clicked()
                                        {
                                            mute_send = Some((member.account_id, Some(term_ms)));
                                        }
                                    }
                                }
                                if quiet_action(ui, "Remove", colors.danger)
                                    .on_hover_text(
                                        "Costs 1 Kick Point, or 1 $MIG when none are held",
                                    )
                                    .clicked()
                                {
                                    kick_send = Some(member.account_id);
                                }
                            }
                        });
                }
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

    // The menu's own state, applied now that the roster borrow has closed: a click opens
    // the row's options, or closes them when the row was the one already open — one menu
    // per conversation, so opening one member's options closes another's.
    if let Some(member) = menu_toggle {
        let already = state
            .member_menus
            .get(&conversation_id)
            .is_some_and(|open| *open == member);
        if already {
            state.member_menus.remove(&conversation_id);
        } else {
            state.member_menus.insert(conversation_id, member);
        }
    }
    // The gift picker opens with a fresh idempotency key and a fresh read of the shelves:
    // a price is a fact worth re-asking before a charge, and the picker that sends the gift
    // is the picker that quoted it. The roster's row names the member, so the picker is just
    // the cards.
    if let Some(member) = gift_ask {
        state.gifting = Some(GiftPick {
            conversation_id,
            member: Some(member),
            key: gift_intent_key(),
        });
        context.issue(Command::GiftCatalogue);
    }
    // The profile view opens with its card ask and its standing asks together: the card is
    // the face, and the progression, badges, board rank, and social edge are the facts that
    // answer beside it — each on its own schedule, each degrading to absence if its answer
    // never lands.
    if let Some(user_id) = profile_ask {
        context.issue(Command::MemberProfile {
            conversation_id,
            user_id,
        });
        context.issue(Command::MemberStanding {
            conversation_id,
            user_id,
        });
        context.issue(Command::MemberRank {
            conversation_id,
            user_id,
        });
        context.issue(Command::MemberEdge {
            conversation_id,
            user_id,
        });
    }
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
    if let Some((target_id, term_ms)) = mute_send {
        // A mute runs the term the menu named — the three terms the web panel offers too —
        // and an unmute is the request with no `until` at all, the wire's own word for
        // "lift it".
        let until = term_ms.map(|ms| {
            migo_core::Timestamp::from_unix_ms(migo_core::Timestamp::now().as_unix_ms() + ms as i64)
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

/// The mute terms a founder may set, as the member menu offers them — the web panel's own
/// three, so a group muted from either client keeps the same vocabulary and the same clock.
const GROUP_MUTE_TERMS_MS: [(&str, u64); 3] = [
    ("1 hour", 60 * 60 * 1000),
    ("1 day", 24 * 60 * 60 * 1000),
    ("7 days", 7 * 24 * 60 * 60 * 1000),
];

/// One quiet option inside a member's menu: words, not a boxed button, because a menu is a
/// list of things that can happen and not a row of controls competing with the row that
/// opened it. The one exception is stated at the call site, where the destructive option
/// takes the danger ink.
fn quiet_action(ui: &mut Ui, label: &str, ink: egui::Color32) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .font(egui::FontId::proportional(font::SMALL))
                .color(ink),
        )
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::NONE),
    )
}

/// A fresh idempotency key for one gift-picker intent, minted the moment the picker opens.
///
/// The wallet's own key's rule, kept: wall-clock nanoseconds are unique per pick on one
/// device, which is all the key needs to be — it separates a retry of one pick from a
/// second, separate gift.
fn gift_intent_key() -> String {
    format!(
        "gift-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

/// The gift picker: the shop's shelves, opened from the header's gift control or a member
/// menu's Gift.
///
/// The wallet's own picker is the pattern — a centred window, one gift per row, a Send
/// beside each — but this one answers the thread's doors: a gift sent from a conversation
/// names someone in it, so a direct chat's picker arrives with its one recipient pre-chosen
/// and a group's offers the members as a choice first. One pick is one gift, so the window
/// closes the moment it sends, and the key minted at open is the whole intent the wire
/// de-duplicates on.
fn gift_picker(ui: &mut Ui, context: &mut Context<'_>, state: &mut ChatState, conversation_id: Id) {
    // This window's pick only: another window's picker is that window's to draw.
    let Some(pick) = state.gifting.clone() else {
        return;
    };
    if pick.conversation_id != conversation_id {
        return;
    }
    let colors = palette(context.theme);
    // The candidate recipients, for the opener that named none: the conversation's other
    // members, named the way the thread itself names them. A conversation whose membership
    // the summary never disclosed offers nobody, and the picker says so rather than guessing
    // a recipient for a spend.
    let recipients: Vec<(Id, String)> = context
        .account
        .zip(
            state
                .conversations
                .iter()
                .find(|c| c.conversation_id == conversation_id),
        )
        .map(|(account, conversation)| {
            conversation
                .members
                .iter()
                .filter(|id| **id != account.account_id)
                .map(|id| {
                    (
                        *id,
                        state
                            .names
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| model::short_id(*id)),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let title = match pick.member {
        Some(member) => format!(
            "Send a gift to {}",
            state
                .names
                .get(&member)
                .cloned()
                .unwrap_or_else(|| model::short_id(member))
        ),
        None => "Send a gift".to_owned(),
    };
    // Deferred: the send closes the picker as it issues (one pick is one gift), and a
    // recipient chosen from the rows becomes the pick's own member for the next frame.
    let mut sent: Option<(Id, String)> = None;
    let mut chosen: Option<Id> = None;
    let mut open = true;
    egui::Window::new(title)
        .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .resizable(false)
        .collapsible(false)
        .open(&mut open)
        .min_width(300.0)
        .show(ui.ctx(), |ui| {
            match pick.member {
                // The opener named nobody: the members are the picker's first question, and
                // the shelves wait behind the answer — a gift without a recipient is a
                // transfer with the wrong name on it.
                None => {
                    if recipients.is_empty() {
                        ui.label(
                            RichText::new(
                                "No one to gift here yet — a gift names someone in the \
                                 conversation.",
                            )
                            .font(egui::FontId::proportional(font::SMALL))
                            .color(colors.text_muted),
                        );
                        return;
                    }
                    for (member, name) in &recipients {
                        let height = 30.0;
                        let width = ui.available_width();
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(crate::theme::radius::MD),
                                colors.surface_hover,
                            );
                        }
                        let mut inner = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(rect.shrink2(egui::vec2(space::XS, 0.0)))
                                .layout(Layout::left_to_right(Align::Center)),
                        );
                        widgets::avatar(&mut inner, context.theme, name, 22.0);
                        inner.add_space(space::XS);
                        inner.label(
                            RichText::new(name.as_str())
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(colors.text),
                        );
                        if response.clicked() {
                            chosen = Some(*member);
                        }
                    }
                }
                Some(member) => {
                    if state.gifts.is_empty() {
                        ui.label(
                            RichText::new("Reading the gift catalogue…")
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(colors.text_muted),
                        );
                        return;
                    }
                    for gift in &state.gifts {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&gift.name)
                                    .font(egui::FontId::proportional(font::BODY))
                                    .color(colors.text),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.button("Send").clicked() {
                                    sent = Some((member, gift.sku.clone()));
                                }
                                ui.label(
                                    RichText::new(format!("{} $MIG", gift.price))
                                        .font(egui::FontId::proportional(font::SMALL))
                                        .color(colors.text_muted),
                                );
                            });
                        });
                    }
                }
            }
        });
    if let Some(member) = chosen {
        if let Some(pick) = state.gifting.as_mut() {
            pick.member = Some(member);
        }
    }
    if let Some((recipient, sku)) = sent {
        context.issue(Command::SendGift {
            sku,
            recipient,
            client_key: Some(pick.key),
        });
        state.gifting = None;
    } else if !open {
        state.gifting = None;
    }
}

/// The member menu's profile view: the card the wire answered, drawn over the window whose
/// roster asked for it.
///
/// The pane's own card is the pattern — who the person is, not what they can be changed
/// into — but this one only reads: a group is where you find out who somebody is, not where
/// you change who you are. The one thing the pane's card carries that this one never does is
/// the birth year, the owner's own disclosure on their own pane.
///
/// Around the card stand the facts that answer on their own schedule — the economy's level,
/// XP, and badge row, the board's rank, the graph's edge — and every one of them degrades to
/// absence: a profile without its standing lines is still a profile, the same rule the web
/// card draws by. The social line is the view's one lever: the friend acts it offers issue
/// their command and re-read the edge, so the line always states what the wire says.
fn member_profile_window(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatState,
    conversation_id: Id,
) {
    // This window's card only: the view files against the conversation whose window asked,
    // so it never outlives the group it was opened from.
    let Some(view) = state.member_profile.clone() else {
        return;
    };
    if view.conversation_id != conversation_id {
        return;
    }
    let colors = palette(context.theme);
    let card = &view.card;
    let progression = view.progression;
    // Deferred: the friend acts the social line offers, applied after the window's borrows
    // close. Each act is a command plus the re-read that makes the next line honest.
    let mut friend_request = false;
    let mut friend_respond: Option<bool> = None;
    let mut open = true;
    egui::Window::new("Profile")
        .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .resizable(false)
        .collapsible(false)
        .open(&mut open)
        .min_width(320.0)
        .show(ui.ctx(), |ui| {
            // The head: the face, and under it the name with the server's own ✔ beside it,
            // the @handle, the presence word, the level, and the status in the person's own
            // words — the same stack the web card draws, each line absent when the fact is.
            ui.horizontal(|ui| {
                widgets::avatar(
                    ui,
                    context.theme,
                    if card.display_name.is_empty() {
                        &card.username
                    } else {
                        &card.display_name
                    },
                    48.0,
                );
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if card.display_name.is_empty() {
                                card.username.clone()
                            } else {
                                card.display_name.clone()
                            })
                            .font(egui::FontId::proportional(font::SUBTITLE))
                            .color(colors.text),
                        );
                        // The verified mark: the server's own word, not a judgement this
                        // client makes, so it draws as the plain ✔ the web card draws and
                        // says whose word it is on the hover.
                        if card.verified.unwrap_or(false) {
                            ui.label(
                                RichText::new("\u{2714}")
                                    .font(egui::FontId::proportional(font::SMALL))
                                    .color(colors.accent),
                            )
                            .on_hover_text("Verified account");
                        }
                    });
                    ui.label(
                        RichText::new(format!("@{}", card.username))
                            .font(egui::FontId::proportional(font::SMALL))
                            .color(colors.text_muted),
                    );
                    let presence = card.presence.label();
                    if !presence.is_empty() {
                        ui.label(
                            RichText::new(presence)
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(colors.positive),
                        );
                    }
                    if let Some(progression) = progression {
                        ui.label(
                            RichText::new(format!("Level {}", progression.level))
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(colors.text_muted),
                        );
                    }
                    if let Some(status) = card
                        .custom_status
                        .as_deref()
                        .filter(|status| !status.trim().is_empty())
                    {
                        ui.label(
                            RichText::new(format!("\u{201C}{status}\u{201D}"))
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(colors.text),
                        );
                    }
                });
            });
            ui.add_space(space::SM);
            if let Some(bio) = card.bio.as_deref().filter(|bio| !bio.trim().is_empty()) {
                ui.label(
                    RichText::new(bio)
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text),
                );
                ui.add_space(space::XS);
            }

            // The facts, one line each and each absent when the fact is: where the person
            // says they are, the language they say they speak, the XP total the economy
            // vouches for, and the board position only the community's first page can state.
            if let Some(country) = card.country.as_deref().filter(|c| !c.is_empty()) {
                ui.label(fact_line(colors, format!("\u{1F30D} {country}")));
            }
            if let Some(language) = card.language.as_deref().filter(|l| !l.is_empty()) {
                ui.label(fact_line(colors, format!("\u{1F5E3} {language}")));
            }
            if let Some(progression) = progression {
                ui.label(fact_line(colors, format!("\u{2B50} {} XP", progression.xp)));
            }
            if let Some(rank) = view.rank {
                ui.label(fact_line(
                    colors,
                    format!("\u{1F3C6} #{rank} on the XP board"),
                ))
                .on_hover_text("Their position on the XP board");
            }

            // The shareable id, drawn for copying rather than for parsing: the click puts it
            // on the clipboard, the same trade the pane's own card makes.
            let response = ui.add(
                egui::Button::new(
                    RichText::new(format!("\u{1FAA4} {}", card.public_id))
                        .font(egui::FontId::proportional(font::TINY))
                        .color(colors.text_muted),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE),
            );
            if response.on_hover_text("Click to copy").clicked() {
                ui.ctx().copy_text(card.public_id.clone());
            }

            // The level bar: the run towards the next level, drawn only when the economy
            // stated both ends of it — a span of zero is no promise, so no bar.
            if let Some(progression) = progression {
                if progression.xp_for_next_level > 0 {
                    ui.add_space(space::XS);
                    let width = ui.available_width();
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, egui::CornerRadius::same(3), colors.surface);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2(width * progression.fraction(), rect.height()),
                        ),
                        egui::CornerRadius::same(3),
                        colors.accent,
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} / {} XP to level {}",
                            progression.xp_into_level,
                            progression.xp_for_next_level,
                            progression.level + 1
                        ))
                        .font(egui::FontId::proportional(font::TINY))
                        .color(colors.text_muted),
                    );
                }
            }

            // The badges, each a chip that says on its hover the day it was earned.
            if !view.badges.is_empty() {
                ui.add_space(space::XS);
                ui.horizontal_wrapped(|ui| {
                    for badge in &view.badges {
                        widgets::pill(
                            ui,
                            &format!("\u{1F3C5} {}", badge.code),
                            colors.text_muted,
                            colors.surface,
                        )
                        .on_hover_text(format!("Earned {}", day_label(badge.awarded_at)));
                    }
                });
            }

            // The social line: what the viewer is to this person, and the one act that state
            // admits. A friend is stated, an outgoing request is stated, an incoming one is
            // answered with Accept or Decline, and any other known edge offers the request —
            // only a block (whose verdict the header's own controls state) and a graph that
            // names no edge draw nothing at all.
            if view.relationship != Some(model::RelationshipKind::Block) {
                ui.add_space(space::XS);
                match view.relationship {
                    Some(model::RelationshipKind::Friend) => {
                        ui.label(
                            RichText::new("\u{2713} Friends")
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(colors.positive),
                        )
                        .on_hover_text("You are friends");
                    }
                    Some(model::RelationshipKind::PendingOutgoing) => {
                        ui.label(
                            RichText::new("Request sent")
                                .font(egui::FontId::proportional(font::SMALL))
                                .color(colors.text_muted),
                        )
                        .on_hover_text("Waiting on their answer");
                    }
                    Some(model::RelationshipKind::PendingIncoming) => {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("wants to be your friend")
                                    .font(egui::FontId::proportional(font::SMALL))
                                    .color(colors.text),
                            );
                            if ui.button("Accept").clicked() {
                                friend_respond = Some(true);
                            }
                            if ui.button("Decline").clicked() {
                                friend_respond = Some(false);
                            }
                        });
                    }
                    // The button rides the arm's guard so the arm stays one clause: egui paints
                    // it once per frame either way, and an unclicked press simply falls through
                    // to the quiet arm below.
                    Some(_) if ui.button("Add friend").clicked() => {
                        friend_request = true;
                    }
                    _ => {}
                }
            }
        });
    // The friend acts, applied now that the window's clone is spent: each issues its command
    // and re-reads the edge, so the line next says what the wire says — never what the
    // button's click wished for.
    if friend_request {
        context.issue(Command::AddFriend {
            user_id: card.account_id.to_string(),
        });
        context.issue(Command::MemberEdge {
            conversation_id,
            user_id: card.account_id,
        });
    }
    if let Some(accept) = friend_respond {
        context.issue(Command::RespondFriend {
            user_id: card.account_id,
            accept,
        });
        context.issue(Command::MemberEdge {
            conversation_id,
            user_id: card.account_id,
        });
    }
    if !open {
        state.member_profile = None;
    }
}

/// One fact line for the member profile view: quiet, small, and muted — the facts are the
/// card's supporting cast, not its headlines.
fn fact_line(colors: Palette, text: String) -> RichText {
    RichText::new(text)
        .font(egui::FontId::proportional(font::SMALL))
        .color(colors.text_muted)
}

/// The compact header over the open conversation: the avatar, and the thread's controls.
///
/// The header no longer restates what the window's own title bar already names — the title,
/// the encryption state, and the member count are all said where they live, and a second copy
/// here was a second thing to keep true (and a third place a reader had to check). What stays
/// is the one fact no title bar carries — the face the conversation goes by, as its avatar —
/// and the controls the thread needs beside it, every one of them drawn at the one control
/// size the composer's send button also wears, so the window's two edges agree about the
/// weight of a touch.
///
/// Mutable state, unlike most headers, for three toggles: the floppy folds the transcript
/// save row in and out, the magnifier folds the thread search row in and out, and the people
/// button folds the roster panel in and out — all three the conversation's own (kept the way
/// drafts are), so the header writes them.
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
    // The header's gift ask, deferred for the same reason — the patience, five times: opening
    // the picker writes `state.gifting` and `state.emoticon_pickers` while `conversation` above
    // still borrows `state`, so the write waits for the frame's borrows to close.
    let mut want_gift = false;

    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.add_space(space::LG);
        widgets::avatar(ui, context.theme, &title, 34.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::LG);
            // The unread count comes from the server's read watermark, which can lag the thread
            // open on screen, so the badge appears here whenever it does.
            widgets::unread_badge(ui, context.theme, conversation.unread);
            ui.add_space(space::XS);
            // The call button, on an encrypted two-member conversation only — the same gate the
            // web and Android headers use. A call is sealed with the conversation's own E2EE
            // group layer, so an unencrypted conversation has no key to seal with. Busy is the
            // worker's word: a second call while one runs is refused there with a toast, not
            // hidden here, because the button's target (the one other member) does not change
            // with call state and re-deriving that gate in the UI would be two opinions about
            // one rule. A group conversation's call is the seat button below, a different
            // protocol from this 1:1 invite.
            if conversation.encrypted && conversation.members.len() == 2 {
                if let Some(me) = context.account.map(|account| account.account_id) {
                    if let Some(peer) = conversation.members.iter().find(|id| **id != me) {
                        if header_control(ui, context.theme, "\u{1F4DE}")
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
            // The group-call seat, on an encrypted group only — the SFU call of section 163,
            // where joining means taking a seat the roster counts and leaving means the one
            // end frame the group service answers for its own ids. The same E2EE gate as the
            // 1:1 button (the join's offer is sealed with the conversation's own call seal),
            // and the button reads the seat map rather than deriving anything about the call:
            // seated or not is the worker's single opinion, delivered as seat events, and the
            // count beside Leave is the roster's, not a member list re-counted here.
            if conversation.encrypted && conversation.is_group() {
                match state.group_calls.get(&conversation_id) {
                    Some(count) => {
                        if header_control(ui, context.theme, "\u{1F3A4}")
                            .on_hover_text(format!("Leave group call ({count} in call)"))
                            .clicked()
                        {
                            context.issue(Command::LeaveGroupCall { conversation_id });
                        }
                    }
                    None => {
                        if header_control(ui, context.theme, "\u{1F3A4}")
                            .on_hover_text("Join group call")
                            .clicked()
                        {
                            context.issue(Command::JoinGroupCall { conversation_id });
                        }
                    }
                }
                ui.add_space(space::XS);
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
            // The floppy: this conversation's words as a file, on demand. The automatic copy
            // (Settings, "save on close") is a preference; this button is the one-off that
            // needs no preference — a person closing a contract negotiation wants the record
            // whether or not they ever turned anything on.
            if header_control(ui, context.theme, "\u{1F4BE}")
                .on_hover_text("Save transcript")
                .clicked()
            {
                want_log_panel = true;
            }
            // The magnifier, beside the floppy: this conversation's thread, searched live over
            // what this session already holds. The honesty rule is the web field's own — a
            // filter of the loaded messages, not a query the server answers — and the row
            // under the header says so in the field's own hint.
            if header_control(ui, context.theme, "\u{1F50D}")
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
                && header_control(ui, context.theme, "\u{1F465}")
                    .on_hover_text("Group members")
                    .clicked()
            {
                want_roster_panel = true;
            }
            // The gift, last of the glyph controls before the avatar: gifting is a thread-level
            // act — it picks a person and spends balance — so its control lives with the
            // thread's other actions up here, not in the row the composer keeps exclusively
            // for chat. The picker it opens is the same one a roster row opens, aimed at the
            // one other member when the thread names exactly one.
            if header_control(ui, context.theme, "\u{1F381}")
                .on_hover_text("Send a gift")
                .clicked()
            {
                want_gift = true;
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
    if want_gift {
        // The picker is one per screen, so the header's click is a toggle of its own picker: a
        // second click while it stands closes it, the same courtesy the web header's gift pays.
        if state
            .gifting
            .as_ref()
            .is_some_and(|pick| pick.conversation_id == conversation_id)
        {
            state.gifting = None;
        } else {
            // A thread of exactly two names its own recipient — the one other member — so the
            // picker is just the cards. Every other thread offers its members as the choice,
            // because a gift sent from a thread names someone in it.
            let member = if conversation.members.len() == 2 {
                context
                    .account
                    .map(|account| account.account_id)
                    .and_then(|me| conversation.members.iter().find(|id| **id != me).copied())
            } else {
                None
            };
            // The picker opens with a fresh idempotency key and a fresh read of the shelves: a
            // price is a fact worth re-asking before a charge, and the picker that sends the
            // gift is the picker that quoted it.
            state.gifting = Some(GiftPick {
                conversation_id,
                member,
                key: gift_intent_key(),
            });
            context.issue(Command::GiftCatalogue);
        }
        // The gift and the smile's fold-out share one composer between them: opening the gift
        // closes the emoticon picker, so the row is never asked to carry two pickers at once.
        if let Some(panel) = state.emoticon_pickers.get_mut(&conversation_id) {
            panel.open = false;
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
                        waveform,
                    } => {
                        voice_bubble(
                            ui,
                            context,
                            message.outgoing,
                            &VoiceNoteView {
                                media_id: *media_id,
                                duration_ms: *duration_ms,
                                waveform: waveform.clone(),
                            },
                            &meta,
                            media,
                        );
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

/// One voice note's own facts, as a row receives them from the message body: the media id
/// the fetch flow needs, the playing time the sender measured, and the sender's sampled
/// waveform. A struct because the row was an eight-argument call by the time the waveform
/// landed, and eight arguments is a call site nobody can check.
pub struct VoiceNoteView {
    /// The media the note's bytes live behind.
    pub media_id: Id,
    /// The playing time the sender measured.
    pub duration_ms: u32,
    /// The folded waveform the sender sampled, when one came on the wire.
    pub waveform: Option<Vec<u8>>,
}

/// One voice note: a play/stop button, the note's own shape — the bars the sender's
/// microphone sampled, folded to the fixed width the message carries — and the delivery
/// state.
///
/// The button is the bubble's neighbour rather than the bubble itself: the bubble carries the
/// waveform, whose whole point is to be *seen* — a row that shows the shape of what was said
/// answers "is this the part I missed?" before it is pressed.
fn voice_bubble(
    ui: &mut Ui,
    context: &mut Context<'_>,
    outgoing: bool,
    note: &VoiceNoteView,
    meta: &str,
    media: &mut MediaState,
) {
    let colors = palette(context.theme);
    let playing = media.playing == Some(note.media_id);
    if let Some(reason) = media.failures.get(&note.media_id) {
        widgets::bubble(
            ui,
            context.theme,
            reason,
            meta,
            outgoing,
            BubbleTone::Problem,
        );
        if ui.button("Try again").clicked() {
            media.requested.remove(&note.media_id);
            media.failures.remove(&note.media_id);
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
                context.issue(Command::PlayVoiceNote {
                    media_id: note.media_id,
                });
            }
        }
        ui.add_space(space::XS);
        // The bubble itself, in the same two tones a text bubble takes — and carrying the
        // note's own shape when the message brought one. A note with no waveform (an older
        // client's send, or a wire that never described it) still says what it is and how
        // long, the same words the row has always used.
        let (fill, foreground, stroke) = if outgoing {
            (colors.accent, colors.text_on_accent, egui::Stroke::NONE)
        } else {
            (
                colors.surface_raised,
                colors.text,
                egui::Stroke::new(1.0, colors.border),
            )
        };
        egui::Frame::new()
            .fill(fill)
            .stroke(stroke)
            .corner_radius(egui::CornerRadius::same(radius::MD))
            .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.68).max(140.0));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let bars = note.waveform.as_deref().unwrap_or(&[]);
                        if !bars.is_empty() {
                            waveform_bars(ui, bars, foreground, 160.0);
                            ui.add_space(space::SM);
                        }
                        ui.label(
                            RichText::new(human_duration(note.duration_ms))
                                .font(egui::FontId::proportional(font::BODY))
                                .color(foreground),
                        );
                    });
                    ui.add_space(space::XS * 0.5);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                        // A dimmer foreground, derived rather than taken from the palette
                        // because the fill differs by direction — the same derivation a text
                        // bubble's timestamp makes.
                        ui.label(
                            RichText::new(meta)
                                .font(egui::FontId::proportional(font::TINY))
                                .color(egui::Color32::from_rgba_unmultiplied(
                                    foreground.r(),
                                    foreground.g(),
                                    foreground.b(),
                                    170,
                                )),
                        );
                    });
                });
            });
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

/// The phone's own hold vocabulary, in the phone's own numbers (§179): a press shorter than
/// this many milliseconds is a tap and becomes the two-step mode's recording, a slide left
/// past this many points has the release throw the note out, and a slide up past this many
/// points locks the recording into the two-step bar without waiting for the finger. The same
/// thresholds the Android client's mic button carries, so the gesture feels the same in the
/// hand wherever it was learned.
const MIC_QUICK_TAP_MS: u64 = 400;
const MIC_CANCEL_SLIDE: f32 = 96.0;
const MIC_LOCK_SLIDE: f32 = 72.0;

/// The waveform's own geometry: bars three points wide with two between, on an
/// eighteen-point strip — wide enough to read at a glance, narrow enough that a full note's
/// fifty bars fit in a composer's row.
const WAVE_BAR_WIDTH: f32 = 3.0;
const WAVE_BAR_GAP: f32 = 2.0;
const WAVE_STRIP_HEIGHT: f32 = 18.0;

/// The composer.
///
/// Enter sends, Shift+Enter inserts a newline. That is the convention every chat client uses, and
/// reversing it means every third message is sent half-finished.
///
/// The row is chat and only chat — the smile, the file control, the field, the microphone, and
/// the send, in that order — because everything else a composer could carry (a gift, a
/// disappearing clock) is a thread-level act that lives in the header or folds into the file
/// menu below the row. A message can be three things — text, a voice note, a file — and all
/// three start here: the field types the first, the microphone records the second, and the file
/// control's menu folds out the doors to the third. While a note is being recorded the composer
/// is replaced by the recording's own face — the hold's row under a finger, the two-step bar
/// without one — because a field that still accepts typing invites a message that arrives
/// after — and interrupts — the note it would replace. A finished note waits in the preview's
/// face, and a discarded one leaves its undo chip standing above whichever face comes next,
/// §179's rule that an accidental cancel is a recoverable mistake.
fn composer(
    ui: &mut Ui,
    context: &mut Context<'_>,
    state: &mut ChatState,
    conversation_id: Id,
    is_room: bool,
) {
    let colors = palette(context.theme);
    let online = context.connection.is_online();

    // The held press's own frame, before any face is chosen: the release and the slides are
    // the pointer's facts, not the composer's, so a hold begun in one conversation window
    // must end honestly in whichever window the pointer lets go over. The slide zones are
    // recomputed every frame — a drift out of the cancel zone and back is the truth on
    // screen, not a latch the release would then contradict.
    if state.mic_held {
        let (released, position) =
            ui.input(|i| (i.pointer.primary_released(), i.pointer.latest_pos()));
        if let Some(position) = position {
            if let Some(origin) = state.mic_press_origin {
                let slide = position - origin;
                state.mic_cancel_slide = slide.x < -MIC_CANCEL_SLIDE;
                // A slide up past the lock threshold is the lock itself: the hold ends and
                // the recording runs on into the two-step bar, whose Stop carries the rest
                // of the vocabulary the finger no longer needs to hold.
                if slide.y < -MIC_LOCK_SLIDE {
                    mic_hold_end(state);
                }
            }
        }
        if state.mic_held && released {
            let held_ms = state
                .mic_hold_started
                .map(|started| started.elapsed().as_millis())
                .unwrap_or(0);
            match hold_release(held_ms, state.mic_cancel_slide) {
                ReleaseDecision::Send => context.issue(Command::SendVoiceNote),
                ReleaseDecision::Cancel => context.issue(Command::CancelVoiceNote),
                // A quick tap is the two-step mode's own start: the recording already runs,
                // so the release hands it to the bar below rather than ending it.
                ReleaseDecision::TwoStep => {}
            }
            mic_hold_end(state);
        }
    }

    // The undo chip, above whichever face the composer wears: a discard's window is a fact
    // about the note rather than about the face that discarded it, so the chip stands even
    // while the next recording is already running — the seconds of mistake the window buys
    // are not lost to someone changing their mind about a second take.
    if state.note_discard_undo == Some(conversation_id) {
        egui::Frame::new()
            .fill(colors.surface)
            .inner_margin(egui::Margin::symmetric(space::LG as i8, space::SM as i8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Recording discarded")
                            .font(egui::FontId::proportional(font::SMALL))
                            .color(colors.text_muted),
                    );
                    if ui.button("Undo").clicked() {
                        context.issue(Command::UndoVoiceNoteDiscard);
                    }
                });
            });
    }

    // Face one: the hold's own row. The press started the recording already, and while the
    // finger stays down the row is the whole interface — the release sends, a quick tap
    // hands the note to the two-step bar, and the slides are told by the hint rather than by
    // buttons, because a button under a held press is a second gesture fighting the first.
    if state.mic_held
        && state
            .recording
            .as_ref()
            .is_none_or(|view| view.conversation_id == conversation_id)
    {
        // The worker's own tick, restated for the row — and on the frame the press was made
        // on, before the worker has said the recording began, the hold's own zero: a timer
        // at 0:00 and a silent strip, for the frame it takes the event to arrive.
        let (elapsed_ms, amplitudes) = match &state.recording {
            Some(view) => (view.elapsed_ms, view.amplitudes.clone()),
            None => (0, Vec::new()),
        };
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
                    ui.label(
                        RichText::new(human_duration(elapsed_ms as u32))
                            .font(egui::FontId::proportional(font::BODY))
                            .color(colors.text_muted),
                    );
                    let width = (ui.available_width() - space::LG).max(60.0);
                    waveform_bars(ui, &amplitudes, colors.accent, width);
                    // The timer is a clock: ask for frames on a cadence so it ticks instead
                    // of freezing between interactions.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                });
                ui.add_space(space::XS * 0.5);
                ui.label(
                    RichText::new(if state.mic_cancel_slide {
                        "Release to cancel"
                    } else {
                        "Release to send \u{00B7} slide left to cancel \u{00B7} slide up to keep recording"
                    })
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(if state.mic_cancel_slide {
                        colors.danger
                    } else {
                        colors.text_muted
                    }),
                );
            });
        return;
    }

    // Face two: the two-step mode's bar. The recording runs without a finger on it — started
    // by a quick tap, a lock, an interruption's pause, or a recovery — so its vocabulary is
    // buttons: the speaker's own pause, a stop into the preview, a cancel under the undo
    // window.
    if let Some(view) = state.recording.as_ref() {
        if view.conversation_id == conversation_id {
            egui::Frame::new()
                .fill(colors.surface)
                .inner_margin(egui::Margin::symmetric(space::LG as i8, space::MD as i8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (word, ink) = if view.paused {
                            ("Paused", colors.text_muted)
                        } else {
                            ("Recording", colors.danger)
                        };
                        ui.label(
                            RichText::new(format!("\u{25CF} {word}"))
                                .font(egui::FontId::proportional(font::BODY))
                                .color(ink),
                        );
                        ui.label(
                            RichText::new(human_duration(view.elapsed_ms as u32))
                                .font(egui::FontId::proportional(font::BODY))
                                .color(colors.text_muted),
                        );
                        let width = (ui.available_width() - 240.0).max(60.0);
                        waveform_bars(ui, &view.amplitudes, colors.accent, width);
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(250));
                        let pause = if view.paused { "Resume" } else { "Pause" };
                        if ui.button(pause).clicked() {
                            // The pause and the resume are the same button's two words: the
                            // bar's state is the worker's own, restated here, so the word
                            // shown is always the one the press performs.
                            context.issue(if view.paused {
                                Command::ResumeRecording
                            } else {
                                Command::PauseRecording
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            context.issue(Command::CancelVoiceNote);
                        }
                        if ui.button("Stop").clicked() {
                            context.issue(Command::StopRecording);
                        }
                    });
                });
            return;
        }
    }

    // Face three: the preview. The note is finished and held; the composer's word is the only
    // thing it waits on. Delete hands it to the undo window rather than destroying it
    // outright, and Send reads the bytes back from the store and seals them into the
    // conversation — the same door every attachment leaves through.
    if let Some(preview) = state.note_preview.as_ref() {
        if preview.conversation_id == conversation_id {
            // The buttons only speak; the composer decides after the frame is drawn, so the
            // bar's own borrow of the preview ends before its word is acted on.
            let mut delete = false;
            let mut send = false;
            egui::Frame::new()
                .fill(colors.surface)
                .inner_margin(egui::Margin::symmetric(space::LG as i8, space::MD as i8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("\u{1F3A4} Voice note")
                                .font(egui::FontId::proportional(font::BODY))
                                .color(colors.text),
                        );
                        ui.label(
                            RichText::new(human_duration(preview.duration_ms))
                                .font(egui::FontId::proportional(font::BODY))
                                .color(colors.text_muted),
                        );
                        let width = (ui.available_width() - 200.0).max(60.0);
                        waveform_bars(ui, &preview.waveform, colors.accent, width);
                        if ui.button("Delete").clicked() {
                            delete = true;
                        }
                        if ui.button("Send").clicked() {
                            send = true;
                        }
                    });
                });
            if delete {
                context.issue(Command::CancelVoiceNote);
            }
            if send {
                // The optimistic hand-off, the same trade a text send makes: the bar steps
                // aside on the word, and a failure on the way brings it back — the worker
                // restores the draft's preview on every failure path, so the note is only
                // really gone when the send says it went.
                state.note_preview = None;
                context.issue(Command::SendVoiceNote);
            }
            return;
        }
    }

    egui::Frame::new()
        .fill(colors.surface)
        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::SM as i8))
        .show(ui, |ui| {
            // The smile's fold-out, above the row it folds from: the two tabs and the glyphs
            // they hold, inserting into the draft the row below is holding. Drawn before the
            // row so the picker stands over it, the way the file menu stands under it.
            let inserted = emoticon_picker(ui, context.theme, state, conversation_id);

            ui.horizontal(|ui| {
                // Room for the row's right edge only: the microphone and the send button the
                // field leaves standing beside it. The smile and the file control sit on the
                // left, drawn before the field, so the field measures what remains after them
                // itself.
                let send_width = 56.0 + (32.0 + space::SM);
                // The smile: the composer's one door to every glyph the account can send — the
                // free emoticons every account has and the packs the wallet sold. The
                // control's own ink says whether its fold-out stands above the row, so the
                // open picker is visible on the control that opened it and not learned only
                // by looking up.
                {
                    let open = state
                        .emoticon_pickers
                        .get(&conversation_id)
                        .is_some_and(|panel| panel.open);
                    let smile =
                        RichText::new("\u{1F60A}").font(egui::FontId::proportional(font::BODY));
                    let smile = if open {
                        smile.color(colors.accent)
                    } else {
                        smile.color(colors.text_muted)
                    };
                    if ui
                        .add(egui::Button::new(smile))
                        .on_hover_text(if open {
                            "Close the emoticon picker"
                        } else {
                            "Emoticons and stickers"
                        })
                        .clicked()
                    {
                        let panel = state.emoticon_pickers.entry(conversation_id).or_default();
                        panel.open = !panel.open;
                        if panel.open {
                            // Opening is the ask, once: the owned-pack read is what the
                            // Stickers tab is made of, and `None` is "not asked yet" rather
                            // than "you own nothing", so the ask is never sent twice.
                            if state.owned_packs.is_none() {
                                context.issue(Command::Entitlements);
                            }
                            // One fold-out at a time: the picker above and the file menu
                            // below share the row's attention.
                            let attach = state.attach.entry(conversation_id).or_default();
                            attach.open = false;
                            attach.picking = false;
                        }
                    }
                }
                // The file control, the smile's row-neighbour and the row's other left-hand
                // glyph: a message's everything-else, folded into a menu rather than laid out
                // on the row, because the row is chat and only chat — two glyphs, a field, a
                // microphone, and the send.
                {
                    let open = state
                        .attach
                        .get(&conversation_id)
                        .is_some_and(|panel| panel.open);
                    let clip =
                        RichText::new("\u{1F4CE}").font(egui::FontId::proportional(font::BODY));
                    let clip = if open {
                        clip.color(colors.accent)
                    } else {
                        clip.color(colors.text_muted)
                    };
                    if ui
                        .add(egui::Button::new(clip))
                        .on_hover_text("Attach a file")
                        .clicked()
                    {
                        let panel = state.attach.entry(conversation_id).or_default();
                        panel.open = !panel.open;
                        // Reopening the menu starts at the menu, never at a path row an
                        // earlier pick left standing.
                        if panel.open {
                            panel.picking = false;
                            // The same one-fold-out courtesy, paid back the other way.
                            if let Some(smile) = state.emoticon_pickers.get_mut(&conversation_id) {
                                smile.open = false;
                            }
                        }
                    }
                }
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

                // The microphone: the one thing a message can be besides text and a file, one
                // press away from the field that types the third. The microphone begins on
                // the press itself, not the click — the hold is the mode, §179's first
                // interaction: the press starts the capture and the release decides among
                // send, cancel, and the two-step handover, so a click that only began on
                // release would have already thrown away the hold's own meaning.
                let mic = ui
                    .add_enabled(
                        online,
                        egui::Button::new(
                            RichText::new("\u{1F3A4}").font(egui::FontId::proportional(font::BODY)),
                        ),
                    )
                    .on_hover_text("Hold to record a voice note");
                if mic.is_pointer_button_down_on() && !state.mic_held && state.recording.is_none() {
                    state.mic_held = true;
                    // The slides are measured from the press's own origin rather than the
                    // button's frame, so the composer trading its field for the hold's row
                    // mid-press — a layout shift under the very finger making it — cannot
                    // move the goal.
                    state.mic_press_origin = ui.input(|i| i.pointer.latest_pos());
                    state.mic_hold_started = Some(std::time::Instant::now());
                    state.mic_cancel_slide = false;
                    context.issue(Command::StartRecording {
                        conversation_id,
                        expires_in_ms: state
                            .disappearing
                            .contains(&conversation_id)
                            .then_some(DISAPPEARING_MS),
                    });
                }

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
            });

            // The glyph the picker handed down, applied now rather than at the click: the
            // fold-out's closures read `state`'s shelves while the draft the glyph lands in is
            // `state` too, and one frame's patience keeps the two apart. The picker stays
            // open after a glyph — a wall of stickers is chosen from one pick at a time.
            if let Some(glyph) = inserted {
                state
                    .drafts
                    .entry(conversation_id)
                    .or_default()
                    .push_str(glyph);
            }

            // The file control's fold-out: the menu of what a file can be, then the path row
            // one of its doors leaves standing. egui offers no file dialog, so every door
            // leads to the one typed path — the same trade the avatar picker makes, and the
            // worker judges the bytes the way the server will anyway, so a door's word
            // ("photo", "video") is vocabulary, not a filter.
            let panel = state.attach.entry(conversation_id).or_default();
            if panel.open {
                // The doors, in the reference's own order and words: any file at all, then
                // the camera's two, then the gallery.
                for door in [
                    "Pick a file",
                    "Take a photo",
                    "Pick a video",
                    "Pick an image",
                ] {
                    if quiet_action(ui, door, colors.text).clicked() {
                        // A door closes the menu and leaves the path row standing: the choice
                        // is made, the path is what remains to be named.
                        panel.open = false;
                        panel.picking = true;
                    }
                }
                // The disappearing arm folds under the doors, below a separator: both are
                // private-and-group-only vocabulary, so they pair naturally, and the row keeps
                // its chat-only shape by carrying neither. Rooms exclude the arm exactly the
                // way the web excludes it — a room is server-readable, so a transcript the
                // server keeps is not a promise a sender can make. The armed state says its
                // own name in the menu, so the promise is visible where it was set and not
                // learned only when a row later vanishes.
                if !is_room {
                    ui.separator();
                    let armed = state.disappearing.contains(&conversation_id);
                    let label = if armed {
                        format!("Disappearing on — new messages vanish after {DISAPPEARING_LABEL}")
                    } else {
                        format!("New messages vanish after {DISAPPEARING_LABEL}")
                    };
                    let ink = if armed {
                        colors.accent
                    } else {
                        colors.text_muted
                    };
                    if quiet_action(ui, &label, ink).clicked() {
                        if armed {
                            state.disappearing.remove(&conversation_id);
                        } else {
                            state.disappearing.insert(conversation_id);
                        }
                    }
                }
            }
            if panel.picking {
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
                            panel.picking = false;
                            panel.path.clear();
                        }
                    }
                });
            }
        });
}

/// The smile's fold-out: the composer's emoticon and sticker picker, two tabs over the
/// account's own shelves.
///
/// The Emoticons tab is the free set every account has, plus the emoticon items of purchased
/// packs; the Stickers tab is the sticker packs the account owns, grouped by pack with a
/// header — a sticker is chosen from its set, not from a merged wall, because the pack is what
/// was bought and the pack is what the eye scans. Everything in either tab inserts as text:
/// the glyphs are Unicode, the conversation is end-to-end encrypted, and a sticker rides out
/// as ordinary message text the way an emoticon does — the size it renders at downstream is
/// the receiver's presentation choice. A pack owned but not carried in this client's table
/// ([`crate::ui::packs`]) does not appear at all; the wallet's shop is where owned and
/// renderable would disagree.
///
/// Returns the glyph chosen this frame, if one was: the composer applies it to the draft after
/// the fold-out's borrows close, because the shelves the picker reads and the draft it writes
/// are one state. The picker stays open after a glyph — a wall of stickers is picked from one
/// cell at a time.
fn emoticon_picker(
    ui: &mut Ui,
    theme: crate::theme::Theme,
    state: &mut ChatState,
    conversation_id: Id,
) -> Option<&'static str> {
    // Read the panel's flags into copies first: the fold-out below reads `state`'s shelves, and
    // a live borrow of the panel would have the two fighting over one state.
    let (open, stickers_tab) = {
        let panel = state.emoticon_pickers.entry(conversation_id).or_default();
        (panel.open, panel.stickers_tab)
    };
    if !open {
        return None;
    }
    let colors = palette(theme);
    // The tab clicks, deferred with the glyph they choose: the writes wait for the fold-out's
    // borrows to close, the same patience every click in this file is given.
    let mut chose_emoticons = false;
    let mut chose_stickers = false;
    let mut chosen: Option<&'static str> = None;
    egui::Frame::new()
        .fill(colors.surface)
        .stroke(egui::Stroke::new(1.0, colors.border))
        .corner_radius(egui::CornerRadius::same(radius::MD))
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if picker_tab(ui, colors, "Emoticons", !stickers_tab).clicked() {
                    chose_emoticons = true;
                }
                if picker_tab(ui, colors, "Stickers", stickers_tab).clicked() {
                    chose_stickers = true;
                }
            });
            // The owned-pack read is what both tabs are made of, and `None` is "not asked
            // yet" — the picker waits rather than showing a free-only set that would read
            // as "you own nothing".
            let Some(owned) = state.owned_packs.as_ref() else {
                ui.label(
                    RichText::new("Reading your packs\u{2026}")
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
                return;
            };
            if stickers_tab {
                let packs = packs::owned_sticker_packs(owned);
                if packs.is_empty() {
                    ui.label(
                        RichText::new("You do not own any sticker packs yet.")
                            .font(egui::FontId::proportional(font::SMALL))
                            .color(colors.text_muted),
                    );
                    ui.label(
                        RichText::new("The wallet's shop sells them.")
                            .font(egui::FontId::proportional(font::TINY))
                            .color(colors.text_muted),
                    );
                } else {
                    for pack in packs {
                        ui.label(
                            RichText::new(pack.name)
                                .font(egui::FontId::proportional(font::TINY))
                                .color(colors.text_muted),
                        );
                        ui.horizontal_wrapped(|ui| {
                            for &glyph in pack.items {
                                if picker_cell(ui, glyph, font::DISPLAY, 44.0).clicked() {
                                    chosen = Some(glyph);
                                }
                            }
                        });
                    }
                }
            } else {
                ui.horizontal_wrapped(|ui| {
                    for &glyph in packs::FREE_EMOTICONS {
                        if picker_cell(ui, glyph, font::TITLE, 32.0).clicked() {
                            chosen = Some(glyph);
                        }
                    }
                    for glyph in packs::owned_emoticons(owned) {
                        if picker_cell(ui, glyph, font::TITLE, 32.0).clicked() {
                            chosen = Some(glyph);
                        }
                    }
                });
            }
        });
    if chose_emoticons || chose_stickers {
        let panel = state.emoticon_pickers.entry(conversation_id).or_default();
        panel.stickers_tab = chose_stickers;
    }
    chosen
}

/// One of the picker's two tabs: a quiet chip whose selected state is its fill, so the tab the
/// picker is on is said by the row itself and not by position alone.
fn picker_tab(ui: &mut Ui, colors: Palette, label: &str, selected: bool) -> egui::Response {
    let text = RichText::new(label)
        .font(egui::FontId::proportional(font::SMALL))
        .color(if selected {
            colors.text
        } else {
            colors.text_muted
        });
    ui.add(
        egui::Button::new(text)
            .fill(if selected {
                colors.surface_selected
            } else {
                egui::Color32::TRANSPARENT
            })
            .stroke(egui::Stroke::NONE),
    )
}

/// One glyph cell in the picker's grid: a button whose whole face is the glyph itself, sized by
/// the shelf it sits on — an emoticon is read at a glance, a sticker at its own larger scale.
fn picker_cell(ui: &mut Ui, glyph: &str, size: f32, side: f32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(glyph).font(egui::FontId::proportional(size)))
            .min_size(egui::Vec2::splat(side)),
    )
}

/// One header control: a glyph button at the one size every control in the window shares.
///
/// The search, the transcript, the members, and the call are a row of peers, and the composer's
/// send is the action the whole window exists to reach — so all of them stand at
/// [`widgets::CONTROL_SIDE`], and a row of same-sized squares is what a row of peers looks
/// like. Quiet by design: the glyph is the label, the hover is the sentence, and the fill stays
/// transparent so the thread's name (the window's own title bar) remains the loudest thing at
/// this edge.
fn header_control(ui: &mut Ui, theme: crate::theme::Theme, glyph: &str) -> egui::Response {
    let colors = palette(theme);
    ui.add(
        egui::Button::new(
            RichText::new(glyph)
                .font(egui::FontId::proportional(font::BODY))
                .color(colors.text),
        )
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::NONE)
        .min_size(egui::Vec2::splat(widgets::CONTROL_SIDE)),
    )
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

/// The three ways a held microphone press can end. Drawn nowhere; it is the vocabulary
/// [`hold_release`] speaks, so the release and the hint under it can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseDecision {
    /// The finger's own send: the note is finalised and sealed on the spot.
    Send,
    /// The slide away: the note is handed to the undo window rather than destroyed.
    Cancel,
    /// The quick tap: the recording runs on into the two-step bar.
    TwoStep,
}

/// What a held microphone's release does, from the hold's own two facts: how long the press
/// was held, and whether it had slid into its cancel zone. The phone's whole hold vocabulary
/// in one place — slide away to throw the note out, a quick tap to hand the note to the
/// two-step bar, anything longer to send — with the same thresholds the Android client's mic
/// button carries, so the same gesture means the same thing wherever it was learned.
fn hold_release(held_ms: u128, cancelled: bool) -> ReleaseDecision {
    if cancelled {
        ReleaseDecision::Cancel
    } else if held_ms < u128::from(MIC_QUICK_TAP_MS) {
        ReleaseDecision::TwoStep
    } else {
        ReleaseDecision::Send
    }
}

/// Ends the hold, wherever it ended: the press's own facts come off the state together, since
/// a hold that is half-remembered — a timer without an origin, a cancel zone without a
/// press — is a gesture the next press would inherit rather than start clean.
fn mic_hold_end(state: &mut ChatState) {
    state.mic_held = false;
    state.mic_press_origin = None;
    state.mic_hold_started = None;
    state.mic_cancel_slide = false;
}

/// The waveform's own drawing: a strip of bars, one per sampled amplitude, mirrored around
/// the strip's midline the way every client draws them, newest at the right — the recording's
/// own direction, so what is being said now is always at the right edge, and a strip with
/// more bars than it has room for drops its oldest, never its newest. Each bar has a small
/// floor so a held silence still shows a pulse to read.
fn waveform_bars(ui: &mut Ui, bars: &[u8], color: egui::Color32, width: f32) {
    let pitch = WAVE_BAR_WIDTH + WAVE_BAR_GAP;
    let capacity = ((width / pitch).floor() as usize).max(1);
    let shown = if bars.len() > capacity {
        &bars[bars.len() - capacity..]
    } else {
        bars
    };
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(width, WAVE_STRIP_HEIGHT), egui::Sense::hover());
    let mid = rect.center().y;
    let right = rect.right() - WAVE_BAR_GAP;
    // Newest first from the right edge, so the strip right-aligns what is being said.
    for (index, bar) in shown.iter().rev().enumerate() {
        let magnitude = (*bar as f32 / 255.0).max(0.15);
        let height = (magnitude * WAVE_STRIP_HEIGHT).min(WAVE_STRIP_HEIGHT);
        let bar_rect = egui::Rect::from_center_size(
            egui::pos2(right - index as f32 * pitch - WAVE_BAR_WIDTH / 2.0, mid),
            egui::vec2(WAVE_BAR_WIDTH, height),
        );
        ui.painter()
            .rect_filled(bar_rect, egui::CornerRadius::same(1), color);
    }
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
            waveform: None,
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

    /// The hold's whole vocabulary in one place, the same thresholds the phone carries: a
    /// slide into the cancel zone throws the note out whatever the clock says, a quick tap
    /// hands the recording to the two-step bar, and anything longer sends. The boundary is
    /// honest in the tap's favour — held exactly the quick-tap window, a press is already a
    /// hold — because a tap that almost made it is a note the sender meant to keep talking to.
    #[test]
    fn a_held_release_speaks_the_phones_vocabulary() {
        assert_eq!(
            hold_release(150, false),
            ReleaseDecision::TwoStep,
            "a quick tap keeps recording"
        );
        assert_eq!(
            hold_release(u128::from(MIC_QUICK_TAP_MS), false),
            ReleaseDecision::Send,
            "held exactly the quick-tap window is already a hold"
        );
        assert_eq!(
            hold_release(2_000, false),
            ReleaseDecision::Send,
            "a long hold sends"
        );
        assert_eq!(
            hold_release(150, true),
            ReleaseDecision::Cancel,
            "the cancel zone wins over the clock"
        );
        assert_eq!(
            hold_release(2_000, true),
            ReleaseDecision::Cancel,
            "the cancel zone wins over a long hold"
        );
    }

    /// The typing line's own clock (brief section 15): a `Start` with no
    /// `Stop` behind it must clear itself on the receiver's local timeout,
    /// because the typer that would have sent the `Stop` may not exist
    /// anymore — the app-death case the wire cannot answer.
    #[test]
    fn a_typing_entry_expires_on_its_own_without_a_stop() {
        let mut state = ChatState::default();
        let conversation = Id::from_bytes([0x11; 16]);
        let typer = Id::from_bytes([0x22; 16]);
        state.note_typing(conversation, typer, true);
        assert_eq!(
            state.typing.get(&conversation),
            Some(&vec![typer]),
            "the Start shows on the line"
        );
        let deadline = state.typing_expires[&(conversation, typer)];
        // A moment before the timeout: the entry survives.
        assert!(
            state
                .expire_typing(deadline - std::time::Duration::from_millis(1))
                .is_some(),
            "the next deadline is still owed"
        );
        assert_eq!(
            state.typing.get(&conversation),
            Some(&vec![typer]),
            "an entry inside its timeout is not swept"
        );
        // At the timeout, with no Stop ever arriving: the entry goes.
        assert!(
            state.expire_typing(deadline).is_none(),
            "nothing is left to expire"
        );
        assert!(
            !state.typing.contains_key(&conversation),
            "a Start that was never followed by a Stop still ends"
        );
    }

    /// A repeated `Start` is a refresh, not a new row: the protocol sends one
    /// every few seconds while a user keeps typing, and the line must neither
    /// duplicate the typer nor expire under them.
    #[test]
    fn a_refreshed_typing_entry_survives_its_first_deadline() {
        let mut state = ChatState::default();
        let conversation = Id::from_bytes([0x11; 16]);
        let typer = Id::from_bytes([0x22; 16]);
        state.note_typing(conversation, typer, true);
        let first = state.typing_expires[&(conversation, typer)];
        state.note_typing(conversation, typer, true);
        let second = state.typing_expires[&(conversation, typer)];
        assert!(second > first, "the refresh moved the deadline");
        assert_eq!(
            state.typing.get(&conversation),
            Some(&vec![typer]),
            "a refresh does not name the typer twice"
        );
        // Past the first deadline but inside the refreshed one: still typing.
        state.expire_typing(first);
        assert_eq!(
            state.typing.get(&conversation),
            Some(&vec![typer]),
            "a continuous typer's indicator does not blink off between refreshes"
        );
    }

    /// A `Stop` clears the entry and its deadline at once, so the line ends
    /// before the timeout when the typer said so themselves.
    #[test]
    fn a_stop_clears_the_entry_and_its_deadline() {
        let mut state = ChatState::default();
        let conversation = Id::from_bytes([0x11; 16]);
        let typer = Id::from_bytes([0x22; 16]);
        state.note_typing(conversation, typer, true);
        state.note_typing(conversation, typer, false);
        assert!(!state.typing.contains_key(&conversation));
        assert!(
            state.typing_expires.is_empty(),
            "the Stop disarms the timeout too"
        );
    }
}
