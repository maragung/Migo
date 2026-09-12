//! The inbox cursor, as text.
//!
//! Brief section 157 requires every listing to page by cursor rather than offset,
//! and the inbox is the listing that moves the most: every gift, level, badge,
//! invitation, and missed call lands at its top. `offset=20` on the second request
//! describes a different twentieth row than it did on the first. A keyset names a
//! *position* — the last row the client actually holds — and stays correct however
//! much arrives above it.
//!
//! The layout discipline follows the conversation-list cursor exactly: text rather
//! than named protocol fields (the client stores it, echoes it, and never looks
//! inside, and publishing the sort order as protocol would make changing it a
//! protocol version), unsigned rather than signed (the cursor names a position in
//! the caller's own inbox and the read is re-authorised on every request, so a
//! forged one pages nobody's list but the forger's), and strict on parse — a
//! cursor that parses loosely pages from a position nobody chose, and the symptom
//! is a client that skips notifications, which is indistinguishable from a
//! delivery failure to whoever reports it.

use std::fmt::Write as _;

use migo_core::{Id, Result, Timestamp};
use migo_protocol::fault;
use migo_store::model::NotificationPosition;

/// Marks the layout, so a future one can be told apart rather than misread.
///
/// Without it, adding a third field would make every cursor issued by the old
/// build parse as a truncated version of the new layout — which does not fail, it
/// silently pages from the wrong place.
const VERSION: &str = "v1";

/// Field separator. Not a character either field can contain.
const SEPARATOR: char = '.';

/// Renders a position as the cursor a client will hand back.
#[must_use]
pub fn encode(position: NotificationPosition) -> String {
    let mut out = String::with_capacity(40);
    out.push_str(VERSION);
    out.push(SEPARATOR);
    // `write!` to a `String` cannot fail; the result is discarded rather than
    // unwrapped so that a formatting change can never introduce a panic on a
    // path that serves every inbox.
    let _ = write!(
        out,
        "{}{}{}",
        position.created_at.as_millis(),
        SEPARATOR,
        position.notification_id
    );
    out
}

/// Parses a cursor a client handed back.
///
/// Fails with `VALIDATION_FAILED` on anything that is not exactly this layout,
/// which is a client bug rather than an attack: the cursor names a position in
/// the caller's own inbox, and every row that comes back is a row that was
/// already theirs.
pub fn decode(cursor: &str) -> Result<NotificationPosition> {
    let mut parts = cursor.split(SEPARATOR);
    let version = parts.next().unwrap_or_default();
    if version != VERSION {
        return Err(invalid("unrecognised cursor version"));
    }
    let at = parts.next().ok_or_else(|| invalid("missing time"))?;
    let notification = parts
        .next()
        .ok_or_else(|| invalid("missing notification"))?;
    if parts.next().is_some() {
        return Err(invalid("trailing data"));
    }
    Ok(NotificationPosition {
        created_at: Timestamp::from_millis(
            at.parse::<i64>()
                .map_err(|_| invalid("time is not a timestamp"))?,
        ),
        notification_id: Id::parse(notification)
            .map_err(|_| invalid("notification is not an identifier"))?,
    })
}

/// One code and one field name for every way a cursor can be wrong.
///
/// The `why` reaches the server's own logs, not the client: distinguishing "not a
/// timestamp" from "trailing data" helps whoever is debugging the client that
/// produced it, and helps nobody who is probing.
fn invalid(why: &'static str) -> migo_core::Error {
    fault::validation("cursor", why)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(at: i64, id: u128) -> NotificationPosition {
        NotificationPosition {
            created_at: Timestamp::from_millis(at),
            notification_id: Id::from(id),
        }
    }

    #[test]
    fn a_position_survives_a_round_trip() {
        let original = position(1_700, 7);
        let decoded = decode(&encode(original)).expect("its own output parses");
        assert_eq!(decoded, original);
    }

    #[test]
    fn anything_that_is_not_exactly_the_layout_is_refused() {
        let good = encode(position(1_700, 7));
        let broken = [
            String::new(),
            "v2.1700.00000000000000000000000007".to_string(),
            good.replace("v1", "v10"),
            format!("{good}.extra"),
            "v1.notatime.00000000000000000000000007".to_string(),
            "v1.1700.not-an-id".to_string(),
            "v1.1700".to_string(),
        ];
        for candidate in broken {
            assert!(
                decode(&candidate).is_err(),
                "should not have parsed: {candidate:?}"
            );
        }
    }
}
