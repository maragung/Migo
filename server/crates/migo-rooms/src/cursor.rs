//! The room browse cursor, as text.
//!
//! Brief section 157 requires every listing to page by cursor rather than offset,
//! and the room directory is the listing whose order moves the most: it is ranked
//! by member count, and every join and leave re-ranks it. A keyset does not freeze
//! the ranking — it names the last row the client holds, so a room whose rank rose
//! past the position between two pages can appear again and one that fell can be
//! skipped, which is the honest behaviour for a directory that re-ranks live. What
//! it prevents is the offset's failure mode, where the rows that moved across the
//! boundary are shown twice or never shown at all.
//!
//! The layout discipline follows the conversation-list cursor exactly: text rather
//! than named protocol fields, unsigned, and strict on parse.

use std::fmt::Write as _;

use migo_core::{Id, Result, Timestamp};
use migo_protocol::fault;
use migo_store::model::RoomPosition;

/// Marks the layout, so a future one can be told apart rather than misread.
///
/// Without it, adding a fourth field would make every cursor issued by the old
/// build parse as a truncated version of the new layout — which does not fail, it
/// silently pages from the wrong place.
const VERSION: &str = "v1";

/// Field separator. Not a character any of the three fields can contain.
const SEPARATOR: char = '.';

/// Renders a position as the cursor a client will hand back.
#[must_use]
pub fn encode(position: RoomPosition) -> String {
    let mut out = String::with_capacity(48);
    out.push_str(VERSION);
    out.push(SEPARATOR);
    // `write!` to a `String` cannot fail; the result is discarded rather than
    // unwrapped so a formatting change can never panic a path that serves every
    // directory page.
    let _ = write!(
        out,
        "{}{}{}{}{}",
        position.member_count,
        SEPARATOR,
        position.created_at.as_millis(),
        SEPARATOR,
        position.room_id
    );
    out
}

/// Parses a cursor a client handed back.
///
/// Fails with `VALIDATION_FAILED` on anything that is not exactly this layout,
/// which is a client bug rather than an attack: the cursor names a position in a
/// public directory, and every row that comes back is a row anyone may browse.
pub fn decode(cursor: &str) -> Result<RoomPosition> {
    let mut parts = cursor.split(SEPARATOR);
    let version = parts.next().unwrap_or_default();
    if version != VERSION {
        return Err(invalid("unrecognised cursor version"));
    }
    let count = parts
        .next()
        .ok_or_else(|| invalid("missing member count"))?;
    let created = parts
        .next()
        .ok_or_else(|| invalid("missing creation time"))?;
    let room = parts.next().ok_or_else(|| invalid("missing room"))?;
    if parts.next().is_some() {
        return Err(invalid("trailing data"));
    }
    Ok(RoomPosition {
        member_count: count
            .parse::<i32>()
            .map_err(|_| invalid("member count is not a number"))?,
        created_at: Timestamp::from_millis(
            created
                .parse::<i64>()
                .map_err(|_| invalid("creation time is not a timestamp"))?,
        ),
        room_id: Id::parse(room).map_err(|_| invalid("room is not an identifier"))?,
    })
}

/// One code and one field name for every way a cursor can be wrong.
///
/// The `why` reaches the server's own logs, not the client.
fn invalid(why: &'static str) -> migo_core::Error {
    fault::validation("cursor", why)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(count: i32, created: i64, id: u128) -> RoomPosition {
        RoomPosition {
            member_count: count,
            created_at: Timestamp::from_millis(created),
            room_id: Id::from(id),
        }
    }

    #[test]
    fn a_position_survives_a_round_trip() {
        for original in [position(5, 1_700, 7), position(0, 9, 1)] {
            let decoded = decode(&encode(original)).expect("its own output parses");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn anything_that_is_not_exactly_the_layout_is_refused() {
        let good = encode(position(5, 1_700, 7));
        let broken = [
            String::new(),
            "v2.5.1700.00000000000000000000000007".to_string(),
            good.replace("v1", "v10"),
            format!("{good}.extra"),
            "v1.notanumber.1700.00000000000000000000000007".to_string(),
            "v1.5.notatime.00000000000000000000000007".to_string(),
            "v1.5.1700.not-an-id".to_string(),
            "v1.5.1700".to_string(),
        ];
        for candidate in broken {
            assert!(
                decode(&candidate).is_err(),
                "should not have parsed: {candidate:?}"
            );
        }
    }
}
