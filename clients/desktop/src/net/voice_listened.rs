//! Voice-note listened marks, persisted: one set per account, surviving restarts.
//!
//! Section 179's rule is that "mark as listened and unlistened" is the receiver's own local
//! state — nothing is sent, the sender is never told, and marking a note unlistened does not
//! unsay a receipt that already went. That makes the marks this device's memory of what this
//! account has heard, and a memory that died at every restart would make every session's
//! thread look unheard; so the set is written beside the vault, the same door the voice-note
//! drafts and the room bridges take, and read back on every sign-in.
//!
//! The store holds media ids, not message ids: a voice note's media id is minted once per
//! uploaded object and names the note on every device, while a message id is the row it
//! landed in — the same shape the playing state and the media cache are keyed by, so the
//! bubble that asks "have I heard this?" asks with the id it already holds.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use migo_core::Id;

/// The listened-marks store: one file per account, beside the vault.
pub(crate) struct VoiceListenedStore {
    dir: PathBuf,
}

impl VoiceListenedStore {
    /// The store beside the vault — `voice-listened/` under the same directory the encrypted
    /// vault lives in, the same door the draft store and the room bridges take.
    pub(crate) fn beside(vault: &Path) -> Self {
        let dir = vault
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("voice-listened");
        Self { dir }
    }

    /// Where one account's marks live. Keyed by account so a second account signing in over
    /// the same window inherits nothing — not another account's heard notes, not their
    /// unmarks.
    fn path(&self, account_id: Id) -> PathBuf {
        self.dir.join(format!("{}.listened", account_id.to_text()))
    }

    /// Reads the account's marks back: every voice note the last session of this account on
    /// this device had listened to. A missing file, an unreadable one, or a line that does
    /// not parse is skipped rather than fatal — a mark this client cannot name is a mark it
    /// cannot draw, not a reason to refuse the rest.
    pub(crate) fn load(&self, account_id: Id) -> HashSet<Id> {
        let Ok(text) = fs::read_to_string(self.path(account_id)) else {
            return HashSet::new();
        };
        text.lines()
            .filter_map(|line| Id::parse(line).ok())
            .collect()
    }

    /// Persists the account's marks whole, best-effort: a write that fails must not take the
    /// click that caused it with it, and the next mark or unmark is the retry — the worst a
    /// missed write costs is one restart that starts with the set as the last good write
    /// left it.
    pub(crate) fn save(&self, account_id: Id, marks: &HashSet<Id>) {
        let _ = fs::create_dir_all(&self.dir);
        let mut text = String::new();
        for media_id in marks {
            text.push_str(&media_id.to_text());
            text.push('\n');
        }
        let _ = fs::write(self.path(account_id), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic id: the number rendered as the low bytes of a 128-bit id, so notes
    /// and accounts can be named in tests the way `idOf(n)` names them across the SDK's
    /// tests.
    fn id_of(n: u8) -> Id {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Id::from_bytes(bytes)
    }

    /// The marks round-trip: what a listen wrote, the next process reads back — the whole
    /// point of the store, and the fix for a thread that would otherwise look unheard after
    /// every restart.
    #[test]
    fn the_marks_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("migo-voice-listened-{}", std::process::id()));
        let store = VoiceListenedStore { dir: dir.clone() };
        let account = id_of(1);
        let marks = HashSet::from([id_of(2), id_of(3)]);
        store.save(account, &marks);

        assert_eq!(
            store.load(account),
            marks,
            "the marks one session wrote are the marks the next process reads"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// One account's file never answers for another's, and an account with no file has
    /// heard nothing: the marks are the account's own memory on this device, and a second
    /// account signing in over the same window inherits nothing.
    #[test]
    fn one_account_does_not_read_another_marks() {
        let dir =
            std::env::temp_dir().join(format!("migo-voice-listened-other-{}", std::process::id()));
        let store = VoiceListenedStore { dir: dir.clone() };
        assert!(
            store.load(id_of(1)).is_empty(),
            "no file means nothing heard"
        );
        let marks = HashSet::from([id_of(9)]);
        store.save(id_of(1), &marks);
        assert!(store.load(id_of(2)).is_empty());
        assert_eq!(store.load(id_of(1)), marks);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A line that does not parse is skipped rather than fatal — a hand-edited or truncated
    /// file costs the one mark nobody can name, not the whole set.
    #[test]
    fn a_broken_line_costs_only_itself() {
        let dir =
            std::env::temp_dir().join(format!("migo-voice-listened-bad-{}", std::process::id()));
        let store = VoiceListenedStore { dir: dir.clone() };
        let account = id_of(4);
        let good = id_of(5);
        fs::create_dir_all(&dir).expect("the store's directory is created");
        fs::write(
            store.path(account),
            format!("not an id\n{}\n", good.to_text()),
        )
        .expect("the hand-written file");
        assert_eq!(store.load(account), HashSet::from([good]));
        let _ = fs::remove_dir_all(&dir);
    }
}
