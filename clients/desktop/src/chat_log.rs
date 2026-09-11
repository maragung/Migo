//! Chat logs: the decrypted transcript as a file this device keeps.
//!
//! Migo is end-to-end encrypted, which means the server holds only ciphertext — the one place a
//! readable transcript exists is this device's memory. This module is the honest bridge between
//! that fact and the wish to keep a record: it renders the transcript the app already decrypted
//! into a plain file on this device's disk, and nothing here ever sends that file anywhere. A
//! log is plaintext by design, stored only here, and the Settings group that offers these
//! controls says so in one line.
//!
//! The line shape is plain text rather than JSON, for the reason the Android client states: a
//! log's reader is a person in a text editor, not a program, and a format only a program could
//! read would be a backup, which is a different feature with different obligations (the `.migo`
//! container is the backup, and it is sealed).
//!
//! # The split
//!
//! The formatting, the file-name rule, and the eviction plan are pure and unit-tested, exactly
//! like the web client's `chat-logs.js` and the phone's `ChatLog.kt` — the same two bugs the
//! phone's tests caught (a space is an underscore; the eviction keeps the *newest*) are pinned
//! here too. The disk halves live in the same module because a Rust crate keeps a feature's
//! logic and its file writes together (see [`crate::settings`], the pattern this follows), and
//! every write is atomic: a rename, never an in-place write, so a crash cannot leave a
//! half-written transcript that reads as a record but records half a conversation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use migo_core::Timestamp;

use crate::model;

/// How many conversations' snapshots the logs directory keeps. The same cap the Android client
/// holds: enough that a month of conversations survives, few enough that a directory of
/// plaintext never grows unbounded on a disk nobody watches.
pub const KEEP_CONVERSATIONS: usize = 20;

/// One transcript line, as a log renders it — the thread's message row reduced to its words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLogLine {
    /// The message's own time as the server stamped it.
    pub at: Timestamp,
    /// The sender's display name; the caller decides what "You" means.
    pub author: String,
    /// The line's text — a chat row's label, so a media message logs its kind.
    pub text: String,
}

/// The one-line rendering a message's body contributes to a transcript.
///
/// Text logs in full, not the preview's first line: a transcript that truncated a paragraph at
/// its first newline would quietly misreport everything anyone wrote with a line break in it.
/// Every other kind logs the same vocabulary the conversation list's preview uses, so a reader
/// of the file and a reader of the app meet the same words.
#[must_use]
pub fn body_text(body: &crate::model::Body) -> String {
    match body {
        crate::model::Body::Text(text) => text.clone(),
        other => other.preview(),
    }
}

/// A timestamp as `2026-09-11 03:52`, in UTC.
///
/// UTC rather than the device's zone, for the reason the web client states: a saved file is an
/// archival artifact, read years later and possibly far away, and the one thing an archival
/// timestamp must carry that a clock on a bubble may omit is a zone that never depended on
/// where or when it is opened again. Built from the model's own civil-day and clock halves, so
/// the log's dates and the thread's day separators can never disagree about what a day is. A
/// stamp that cannot be built (a non-positive instant) renders as `—` rather than panicking
/// halfway through writing a file the person asked for.
#[must_use]
pub fn log_stamp(at: Timestamp) -> String {
    if at.as_unix_ms() <= 0 {
        return "\u{2014}".to_owned();
    }
    format!("{} {}", model::date(at), model::clock(at))
}

/// Formats one conversation's transcript.
///
/// The header names the conversation and the moment of the export, because a log that does not
/// say when it was taken silently answers "is this current?" with a guess. Each line is
/// `date  time  author  text` — two spaces between fields, so a fixed-width glance parses it
/// and a paste into anything keeps the columns. The body keeps the caller's order — the thread
/// is already sequence-sorted, and a transcript is not a place to invent a second sort.
#[must_use]
pub fn format_chat_log(title: &str, exported_at: Timestamp, lines: &[ChatLogLine]) -> String {
    let mut out = String::new();
    out.push_str("Migo chat log \u{2014} ");
    out.push_str(title);
    out.push('\n');
    out.push_str("Exported ");
    out.push_str(&log_stamp(exported_at));
    out.push('\n');
    out.push('\n');
    for line in lines {
        out.push_str(&log_stamp(line.at));
        out.push_str("  ");
        out.push_str(&line.author);
        out.push_str("  ");
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

/// Formats every held conversation into one log, in the order the caller lists them.
///
/// The same header rule as [`format_chat_log`], plus a per-conversation heading, because a file
/// of transcripts with no dividers is a file one reads by accident into the wrong conversation.
#[must_use]
pub fn format_all_chats_log(
    account: &str,
    exported_at: Timestamp,
    chats: &[(String, Vec<ChatLogLine>)],
) -> String {
    let mut out = String::new();
    out.push_str("Migo chat logs \u{2014} ");
    out.push_str(account);
    out.push('\n');
    out.push_str("Exported ");
    out.push_str(&log_stamp(exported_at));
    out.push('\n');
    for (title, lines) in chats {
        out.push('\n');
        out.push_str("== ");
        out.push_str(title);
        out.push_str(" ==\n");
        for line in lines {
            out.push_str(&log_stamp(line.at));
            out.push_str("  ");
            out.push_str(&line.author);
            out.push_str("  ");
            out.push_str(&line.text);
            out.push('\n');
        }
    }
    out
}

/// A conversation title as a file name this device will accept.
///
/// Everything that could climb out of a file name — separators, colons, controls, anything not
/// plainly writable — becomes `_`; runs collapse so the result reads as one substitution; a
/// space is an underscore too, not a kept character, because a file name with spaces is quoted
/// by every shell that touches it while the underscore reads the same and never needs quoting;
/// leading and trailing dots, spaces and underscores go, because a leading dot hides the file;
/// and the whole is bounded to 48 characters, which every filesystem this app runs on accepts
/// once suffixed. Empty after all that (a title of only emoji, or only slashes) falls back to
/// `chat`, because the alternative is a nameless file no picker can suggest.
#[must_use]
pub fn sanitize_filename(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    for character in raw.chars() {
        let mapped = if character.is_alphanumeric()
            || character == '-'
            || character == '_'
            || character == '.'
        {
            character
        } else {
            '_'
        };
        // Runs collapse on the *mapped* character, not the incoming one: two slashes in a row
        // are two substitutions of the same underscore, and collapsing only literal
        // underscores would leave "a//b" as "a__b" — two marks for one unusable run.
        if mapped == '_' && cleaned.ends_with('_') {
            continue;
        }
        cleaned.push(mapped);
    }
    // Runs of dots collapse too, so `..` reads as one and never reaches a path component as a
    // parent reference.
    let mut dedotted = String::with_capacity(cleaned.len());
    for character in cleaned.chars() {
        if character == '.' && dedotted.ends_with('.') {
            continue;
        }
        dedotted.push(character);
    }
    let trimmed = dedotted.trim_matches(['.', '_', ' ']);
    let bounded: String = trimmed.chars().take(48).collect();
    if bounded.is_empty() {
        "chat".to_owned()
    } else {
        bounded
    }
}

/// A conversation title as a snapshot's file name: the sanitized title, the `.txt` the log
/// already is, and nothing else — no timestamps in the name, because each conversation keeps
/// exactly one newest snapshot and a per-save timestamp would accumulate files the eviction
/// would then have to hunt.
#[must_use]
pub fn chat_log_filename(title: &str) -> String {
    format!("{}.txt", sanitize_filename(title))
}

/// Which held snapshots to delete once a new one is written, given the existing files
/// oldest-first.
///
/// The rule: one file per conversation (the writer always overwrites its own conversation's
/// file, so the caller never passes two of the same conversation), and at most
/// [`KEEP_CONVERSATIONS`] conversations in all. Everything past the cap is a deletion, oldest
/// first by the caller's ordering — the file system's own `modified`, in the caller's hands,
/// because this function must stay pure and the file system is anything but. The deletions are
/// the FRONT of the list: the caller sorts oldest-first, so what survives a cap is the newest
/// `keep`, and everything before them is exactly what the cap retired.
#[must_use]
pub fn snapshot_evictions<T: Clone>(oldest_first: &[T], keep: usize) -> Vec<T> {
    if oldest_first.len() > keep {
        oldest_first[..oldest_first.len() - keep].to_vec()
    } else {
        Vec::new()
    }
}

/// One saved snapshot as the logs directory holds it, for the Settings list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedLog {
    /// The file itself, absolute as the directory was resolved.
    pub path: PathBuf,
    /// The file's name, without the `.txt` — the sanitized title it was written under.
    pub name: String,
    /// When the file was last written, for the "saved …" line beside it.
    pub modified: Timestamp,
    /// The file's size, for the same line.
    pub bytes: u64,
}

/// Where the snapshots live: a `migo-logs` directory beside `settings.json`, under the
/// platform's data directory — the same root [`crate::settings::Settings::default_path`]
/// resolves, so the logs follow the settings file wherever the platform puts writable
/// per-user data, and survive a vault rotation the way every other non-secret record does.
#[must_use]
pub fn logs_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("io", "migo", "migo-desktop")
        .map(|dirs| dirs.data_dir().join("migo-logs"))
}

/// Writes one conversation's snapshot, overwriting that conversation's own earlier file, then
/// retires whatever the cap says is past it.
///
/// `0o600` on Unix, the vault's own courtesy: a snapshot is *plaintext* of conversations, more
/// readable than the vault's ciphertext ever is, and there is no reason another local account
/// should get to read it.
pub fn write_snapshot(dir: &Path, title: &str, text: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let name = chat_log_filename(title);
    let file = dir.join(&name);
    let temporary = dir.join(format!("{name}.new"));
    fs::write(&temporary, text)?;
    restrict(&temporary)?;
    fs::rename(&temporary, &file)?;
    evict_past_cap(dir);
    Ok(())
}

/// Lists the saved snapshots, newest first, for the Settings list and the storage group's
/// count. A directory that does not exist yet (auto-save never used) is honestly empty.
#[must_use]
pub fn saved_logs(dir: &Path) -> Vec<SavedLog> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "txt") {
            // A leftover `.new` from a crashed write, or anything a person put here: listed
            // nowhere, so the list is a list of logs and not of directory contents.
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .map(Timestamp::from_unix_ms)
            .unwrap_or_else(|| Timestamp::from_unix_ms(0));
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(SavedLog {
            path,
            name,
            modified,
            bytes: metadata.len(),
        });
    }
    // Newest first: the list reads the way the conversations do, and the cap's survivor is
    // the one at the top rather than the one that scrolled. `Reverse` because `sort_by_key`
    // is ascending and the intent is the opposite.
    out.sort_by_key(|log| std::cmp::Reverse(log.modified.as_unix_ms()));
    out
}

/// Deletes every saved snapshot — sign-out's wipe, and the storage group's broom. Keys,
/// settings, and the directory itself stay.
pub fn clear_all(dir: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Writes a transcript to a path the person typed, for the two exports (one conversation, all
/// conversations). Atomic like every other write here: a transcript file that reads as a
/// record but records half a conversation is worse than an error the person can retry.
pub fn write_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// Retires the oldest snapshots past [`KEEP_CONVERSATIONS`], by the file system's own `modified`
/// ordering — the one fact about "oldest" this device can read without keeping a ledger.
fn evict_past_cap(dir: &Path) {
    let mut held = saved_logs(dir);
    held.reverse(); // oldest-first, the order the eviction plan names its deletions in.
    for victim in snapshot_evictions(
        &held.iter().map(|log| log.path.clone()).collect::<Vec<_>>(),
        KEEP_CONVERSATIONS,
    ) {
        if let Err(error) = fs::remove_file(&victim) {
            tracing::warn!("migo-desktop: could not retire an old chat log: {error}");
        }
    }
}

/// Makes a file readable only by its owner — see [`write_snapshot`] for why a *plaintext*
/// transcript deserves at least the vault's own permission hygiene.
fn restrict(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// `modified` as unix milliseconds, for tests that need a stamp with a known value.
#[cfg(test)]
fn stamp_of(epoch_ms: i64) -> Timestamp {
    Timestamp::from_unix_ms(epoch_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transcript's whole shape in one test: the header names the conversation and the
    /// moment, and each line is `stamp  author  text` in the caller's order — the same words
    /// the web and phone clients write, so a person moving between clients reads one shape.
    #[test]
    fn a_log_names_its_conversation_its_moment_and_one_line_per_message() {
        let log = format_chat_log(
            "Team room",
            stamp_of(1_700_000_060_000),
            &[
                ChatLogLine {
                    at: stamp_of(1_700_000_000_000),
                    author: "Alice".to_owned(),
                    text: "First".to_owned(),
                },
                ChatLogLine {
                    at: stamp_of(1_700_000_010_000),
                    author: "Bob".to_owned(),
                    text: "Second".to_owned(),
                },
            ],
        );
        let rows: Vec<&str> = log.split('\n').filter(|row| !row.is_empty()).collect();
        assert_eq!(rows[0], "Migo chat log \u{2014} Team room");
        assert!(
            rows[1].starts_with("Exported "),
            "the header stamps the export"
        );
        assert!(
            rows[2].ends_with("  Alice  First"),
            "a line is the caller's own order"
        );
        assert!(rows[3].ends_with("  Bob  Second"));
    }

    /// A conversation with no messages still states its name and moment, rather than writing
    /// nothing — though the caller refuses to snapshot one in the first place, the formatter
    /// must not be the half that panics or invents.
    #[test]
    fn a_log_with_no_messages_still_states_its_conversation_and_moment() {
        let log = format_chat_log("Quiet chat", stamp_of(1_700_000_000_000), &[]);
        assert!(log.contains("Migo chat log \u{2014} Quiet chat"));
        assert!(log.contains("Exported "));
    }

    /// An out-of-range stamp renders a dash rather than throwing mid-write, and a real one is
    /// the fixed 16 characters of UTC date and clock — pinned to a shape, not to a time a test
    /// in another timezone would fail.
    #[test]
    fn an_out_of_range_timestamp_renders_a_dash_rather_than_throwing() {
        assert_eq!(log_stamp(stamp_of(0)), "\u{2014}");
        assert_eq!(log_stamp(stamp_of(-5)), "\u{2014}");
        assert!(
            has_stamp_shape(&log_stamp(stamp_of(1_700_000_000_000))),
            "a real timestamp is 16 characters of date and clock"
        );
    }

    /// `true` when the stamp is the `YYYY-MM-DD HH:MM` shape, without pulling a regex crate in
    /// for one assertion.
    fn has_stamp_shape(stamp: &str) -> bool {
        let bytes = stamp.as_bytes();
        bytes.len() == 16
            && bytes[..4].iter().all(u8::is_ascii_digit)
            && bytes[4] == b'-'
            && bytes[5..7].iter().all(u8::is_ascii_digit)
            && bytes[7] == b'-'
            && bytes[8..10].iter().all(u8::is_ascii_digit)
            && bytes[10] == b' '
            && bytes[11..13].iter().all(u8::is_ascii_digit)
            && bytes[13] == b':'
            && bytes[14..16].iter().all(u8::is_ascii_digit)
    }

    /// The all-chats log names the account and divides conversations with headings, because a
    /// file of transcripts with no dividers is a file one reads into the wrong conversation.
    #[test]
    fn the_all_chats_log_divides_conversations_with_headings() {
        let log = format_all_chats_log(
            "alice",
            stamp_of(1_700_000_000_000),
            &[
                (
                    "Team room".to_owned(),
                    vec![ChatLogLine {
                        at: stamp_of(1_700_000_000_000),
                        author: "Alice".to_owned(),
                        text: "Hi".to_owned(),
                    }],
                ),
                ("Bob".to_owned(), Vec::new()),
            ],
        );
        assert!(log.contains("Migo chat logs \u{2014} alice"));
        assert!(log.contains("== Team room =="));
        assert!(log.contains("== Bob =="));
    }

    /// Text bodies log in full: the preview's first-line rule is for list rows, and a
    /// transcript that cut a paragraph at its first newline would misreport it.
    #[test]
    fn text_bodies_log_in_full_and_other_kinds_log_their_kind() {
        assert_eq!(
            body_text(&crate::model::Body::Text("two\nlines".to_owned())),
            "two\nlines"
        );
        assert_eq!(
            body_text(&crate::model::Body::VoiceNote {
                media_id: migo_core::Id::from_bytes([1; 16]),
                duration_ms: 1500,
            }),
            "Voice note (2s)"
        );
    }

    /// A title becomes a writable file name: internal spaces are underscores too, not just the
    /// dangerous characters — one rule for everything the shell would have to quote keeps the
    /// mapping from the title a reader can predict.
    #[test]
    fn a_title_becomes_a_writable_file_name() {
        assert_eq!(chat_log_filename("Team room"), "Team_room.txt");
        assert_eq!(chat_log_filename("Bob"), "Bob.txt");
    }

    /// Everything that could climb out of a file name is neutralised, runs collapse so one
    /// substitution reads as one, and leading and trailing dots and spaces go — a leading dot
    /// hides the file.
    #[test]
    fn everything_that_could_climb_out_of_a_file_name_is_neutralised() {
        assert_eq!(chat_log_filename("a/b"), "a_b.txt");
        assert_eq!(chat_log_filename("a\\b"), "a_b.txt");
        assert_eq!(chat_log_filename("a:b"), "a_b.txt");
        assert_eq!(
            chat_log_filename("it's complicated"),
            "it_s_complicated.txt"
        );
        assert_eq!(chat_log_filename("a//b??c"), "a_b_c.txt");
        assert_eq!(chat_log_filename("  ..name.. "), "name.txt");
        // A run of dots collapses rather than surviving as a parent reference.
        assert_eq!(chat_log_filename("a..b"), "a.b.txt");
    }

    /// A title of only symbols falls back to a name the picker can still suggest.
    #[test]
    fn a_title_of_only_symbols_falls_back_to_a_name() {
        assert_eq!(chat_log_filename("???"), "chat.txt");
        assert_eq!(chat_log_filename(""), "chat.txt");
        assert_eq!(chat_log_filename("   "), "chat.txt");
    }

    /// A long title is bounded to a length every filesystem accepts, keeping the front — where
    /// a title's meaning lives.
    #[test]
    fn a_long_title_is_bounded_to_48_characters() {
        let long = "x".repeat(200);
        let name = chat_log_filename(&long);
        assert_eq!(name.len(), 48 + ".txt".len());
        assert!(name.starts_with("xxxx"));
    }

    /// The eviction keeps the NEWEST conversations and names everything past the cap — the
    /// deletion list is the FRONT of the oldest-first list, the bug the phone's first
    /// implementation had and its tests caught. A cap at or beyond the size keeps everything;
    /// a cap of zero is "keep none of these", not a crash; an empty list evicts nothing.
    #[test]
    fn the_eviction_keeps_the_newest_conversations() {
        let held = ["oldest", "older", "newer", "newest"];
        assert_eq!(snapshot_evictions(&held, 2), ["oldest", "older"]);
        assert_eq!(snapshot_evictions(&held, 4), Vec::<&str>::new());
        assert_eq!(snapshot_evictions(&held, 10), Vec::<&str>::new());
        assert_eq!(snapshot_evictions(&held, 0), held);
        assert_eq!(
            snapshot_evictions(&Vec::<&str>::new(), 0),
            Vec::<&str>::new()
        );
    }

    /// The directory halves, end to end against a scratch directory: a snapshot writes under
    /// its sanitized name and evicts past the cap by the file system's own ordering, the same
    /// conversation's second write replaces the first, and the broom takes everything while
    /// leaving the directory itself.
    #[test]
    fn snapshots_write_overwrite_and_retire_past_the_cap() {
        let dir = std::env::temp_dir().join("migo-desktop-chat-log-store");
        let _ = clear_all(&dir);

        let line = ChatLogLine {
            at: stamp_of(1_700_000_000_000),
            author: "Alice".to_owned(),
            text: "Hello".to_owned(),
        };
        let text = format_chat_log("Team room", stamp_of(1_700_000_000_000), &[line.clone()]);
        write_snapshot(&dir, "Team room", &text).expect("write");
        assert_eq!(saved_logs(&dir).len(), 1, "one conversation, one file");
        assert_eq!(saved_logs(&dir)[0].name, "Team_room");

        // The same conversation again: replace, not accumulate.
        write_snapshot(&dir, "Team room", &text).expect("rewrite");
        assert_eq!(saved_logs(&dir).len(), 1);

        // Distinct titles up to and past the cap. The file system's `modified` orders them,
        // and on a filesystem with coarse timestamps two writes in the same instant can order
        // either way — so the assertion is on the count, not on which specific title the cap
        // retired.
        for n in 0..=(KEEP_CONVERSATIONS as u8 + 3) {
            let title = format!("Conversation {n}");
            let body = format_chat_log(&title, stamp_of(1_700_000_000_000), &[line.clone()]);
            write_snapshot(&dir, &title, &body).expect("write");
        }
        assert_eq!(saved_logs(&dir).len(), KEEP_CONVERSATIONS);

        // And the broom takes every one of them, directory and all.
        clear_all(&dir).expect("clear");
        assert!(saved_logs(&dir).is_empty());
        assert!(
            dir.is_dir(),
            "the directory itself stays: it is the auto-save's home"
        );
    }

    /// `logs_dir` sits beside the settings file — same platform data directory, so the two
    /// records a person might want to back up together live together.
    #[test]
    fn the_logs_directory_is_beside_the_settings_file() {
        let settings = crate::settings::Settings::default_path();
        let logs = logs_dir();
        match (settings, logs) {
            (Some(settings), Some(logs)) => {
                assert_eq!(logs.parent(), settings.parent());
                assert_eq!(
                    logs.file_name().and_then(|name| name.to_str()),
                    Some("migo-logs")
                );
            }
            // A platform with no data directory has neither, and the feature honestly no-ops.
            (None, None) => {}
            other => panic!("the two paths resolve together or not at all: {other:?}"),
        }
    }
}
