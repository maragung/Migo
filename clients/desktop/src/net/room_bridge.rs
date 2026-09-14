//! The room bridge, persisted: room id → conversation id, surviving restarts.
//!
//! The wire names a room's conversation id in exactly one moment — the join's answer — and the
//! conversation summary never states the room behind it, so a session that held the mapping only
//! in memory started every process with it empty: no room topic subscribed (no notices, no live
//! counts, no kick detection), no bridge for the member events to patch the encryption audience
//! through, and no room id for the leave to name. The web client keeps its `room-info-store`
//! through a reload for the same reason; the SDK's answer is `rehydrateRoom`, which rebuilds the
//! bridge by re-joining. This desktop speaks its own Rust protocol client rather than the SDK,
//! so the concept is mirrored here at the net layer's own door: the bridge is written the moment
//! a join names it, wiped the moment a leave or a removal forgets it, and read back on every
//! sign-in so the reconnect path's existing room-topic re-subscribe has the ids to work with.
//!
//! Re-watching a topic the account can no longer authorize (a departure that happened on another
//! device while this one was off) is refused quietly by the server's own membership gate, so a
//! stale row costs one declined SUBSCRIBE rather than a wrong behaviour — and the row is cleaned
//! by the next join or the next leave of that room, whichever comes first. What a persisted row
//! cannot cover is a room joined on another device while this one was off *and* never joined
//! here: the conversation summary names no room id, so no client can learn it without a wire
//! change (flagged for a future migration, not worked around here).

use std::fs;
use std::path::{Path, PathBuf};

use migo_core::Id;

/// The bridge store: one file per account, beside the vault.
pub(crate) struct RoomBridgeStore {
    dir: PathBuf,
}

impl RoomBridgeStore {
    /// The store beside the vault — `room-bridges/` under the same directory the encrypted
    /// vault lives in, the same door the voice-note drafts take.
    pub(crate) fn beside(vault: &Path) -> Self {
        let dir = vault
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("room-bridges");
        Self { dir }
    }

    /// Where one account's bridge lives. Keyed by account so a second account signing in over
    /// the same window inherits nothing — not another account's rooms, not their teardowns.
    fn path(&self, account_id: Id) -> PathBuf {
        self.dir.join(format!("{}.rooms", account_id.to_text()))
    }

    /// Reads the account's bridge back: every `(room, conversation)` pair the last session of
    /// this account on this device held. A missing file, an unreadable one, or a line that does
    /// not parse is skipped rather than fatal — the bridge is a cache of one wire moment, and a
    /// row this client cannot name is a row it cannot use, not a reason to refuse the rest.
    pub(crate) fn load(&self, account_id: Id) -> Vec<(Id, Id)> {
        let Ok(text) = fs::read_to_string(self.path(account_id)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let mut halves = line.split_whitespace();
            let (Some(room), Some(conversation), None) =
                (halves.next(), halves.next(), halves.next())
            else {
                continue;
            };
            if let (Ok(room), Ok(conversation)) = (Id::parse(room), Id::parse(conversation)) {
                out.push((room, conversation));
            }
        }
        out
    }

    /// Persists the account's bridge whole, best-effort: a write that fails must not take the
    /// join that caused it with it, and the next join or leave is the retry — the worst a missed
    /// write costs is one restart that starts as this batch's restarts all did.
    pub(crate) fn save(&self, account_id: Id, bridges: &[(Id, Id)]) {
        let _ = fs::create_dir_all(&self.dir);
        let mut text = String::new();
        for (room, conversation) in bridges {
            text.push_str(&room.to_text());
            text.push(' ');
            text.push_str(&conversation.to_text());
            text.push('\n');
        }
        let _ = fs::write(self.path(account_id), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic id: the number rendered as the low bytes of a 128-bit id.
    fn id_of(n: u8) -> Id {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Id::from_bytes(bytes)
    }

    /// The bridge round-trips: what a join wrote, the next process reads back, in the same
    /// pairs — the whole point of the store, and the restart blind spot's fix.
    #[test]
    fn the_bridge_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("migo-room-bridge-{}", std::process::id()));
        let store = RoomBridgeStore { dir: dir.clone() };
        let account = id_of(1);
        let bridges = vec![(id_of(2), id_of(3)), (id_of(4), id_of(5))];
        store.save(account, &bridges);

        assert_eq!(
            store.load(account),
            bridges,
            "the pairs a join wrote are the pairs the next process reads"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// One account's file never answers for another's: the bridge is the account's own set of
    /// rooms, and a second account signing in over the same window inherits nothing.
    #[test]
    fn one_account_does_not_read_another_rooms() {
        let dir =
            std::env::temp_dir().join(format!("migo-room-bridge-other-{}", std::process::id()));
        let store = RoomBridgeStore { dir: dir.clone() };
        store.save(id_of(1), &[(id_of(2), id_of(3))]);

        assert!(
            store.load(id_of(9)).is_empty(),
            "a second account starts with no bridge of the first's"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A line that is not two ids — a truncated write, an edited file, a stray blank — is
    /// skipped rather than fatal: the rows that do parse still load, because one bad line is
    /// not a reason to hand the restart the empty bridge this store exists to avoid.
    #[test]
    fn a_line_that_is_not_two_ids_is_skipped() {
        let dir = std::env::temp_dir().join(format!("migo-room-bridge-bad-{}", std::process::id()));
        let store = RoomBridgeStore { dir: dir.clone() };
        let account = id_of(1);
        let good = (id_of(2), id_of(3));
        let _ = fs::create_dir_all(&store.dir);
        let text = format!(
            "not-an-id-at-all\n\n{} {}\nonly-one\n",
            good.0.to_text(),
            good.1.to_text()
        );
        let _ = fs::write(store.path(account), text);

        assert_eq!(
            store.load(account),
            vec![good],
            "the parsable row loads and the rest is refused quietly"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// An empty bridge wipes the file's content rather than leaving the last session's rows
    /// standing: a leave of the last room must survive a restart as "no bridge", or the next
    /// process re-subscribes a topic the account cannot authorize.
    #[test]
    fn an_empty_bridge_wipes_the_file() {
        let dir =
            std::env::temp_dir().join(format!("migo-room-bridge-wipe-{}", std::process::id()));
        let store = RoomBridgeStore { dir: dir.clone() };
        let account = id_of(1);
        store.save(account, &[(id_of(2), id_of(3))]);
        store.save(account, &[]);

        assert!(
            store.load(account).is_empty(),
            "a wiped bridge reads back empty"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
