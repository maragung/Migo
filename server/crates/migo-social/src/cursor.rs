//! The relationship-page cursor, as text.
//!
//! The combined listing behind `RELATIONSHIP_LIST` is a snapshot for graphs that
//! fit in one frame; a caller whose friend list is longer than a page pages one
//! kind at a time, and the position between two pages is this cursor. It follows
//! the conversation-list cursor's design: a keyset rather than an offset, because
//! a friend list reorders itself every time anybody befriends anybody, and
//! `offset=200` on the second request describes a different two-hundredth row
//! than it did on the first — so the rows that moved across the boundary are
//! either shown twice or never shown, and a friend who is never shown is
//! indistinguishable from a friend who was deleted.
//!
//! A keyset names the last row the client actually holds. This listing is ordered
//! newest first, then by the other account's id, so a position is exactly that
//! pair, and the next page resumes strictly after it.
//!
//! # Why it is text and not a struct
//!
//! The wire field is a `String`, and that is the right shape: the client stores
//! it, echoes it back, and never looks inside. Encoding the keyset fields as
//! named protocol fields would publish the list's sort order as part of the
//! protocol, and changing the order later would then be a protocol version
//! rather than a query.
//!
//! # Why it is not signed
//!
//! A signature would buy nothing. The cursor names a position in the *caller's
//! own* graph, and the page is re-authorised on every request: every row that
//! comes back is a row the caller owns. Forging one lets a caller page their own
//! list from a place they made up. A malformed cursor is a client bug, not an
//! attack, and is answered as `VALIDATION_FAILED` rather than as a permission
//! problem — the same answer the conversation cursor gives.

use std::fmt::Write as _;

use migo_core::{Id, Result, Timestamp};
use migo_protocol::fault;

/// Marks the layout, so a future one can be told apart rather than misread.
///
/// Without it, adding a field would make every cursor issued by an old build
/// parse as a truncated version of the new layout — which does not fail, it
/// silently pages from the wrong place.
const VERSION: &str = "v1";

/// Field separator. Not a character either field can contain.
const SEPARATOR: char = '.';

/// Renders a position as the cursor a client will hand back.
#[must_use]
pub fn encode(position: (Timestamp, Id)) -> String {
    let mut out = String::with_capacity(48);
    out.push_str(VERSION);
    out.push(SEPARATOR);
    // `write!` to a `String` cannot fail; the result is discarded rather than
    // unwrapped so that a formatting change can never introduce a panic on a
    // path that serves every page of a large graph.
    let _ = write!(out, "{}", position.0.as_millis());
    out.push(SEPARATOR);
    let _ = write!(out, "{}", position.1);
    out
}

/// Parses a cursor a client handed back.
///
/// Fails with `VALIDATION_FAILED` on anything that is not exactly this layout.
/// Being strict is deliberate: a cursor that parses loosely pages from a position
/// nobody chose, and the symptom is a client that skips friends, which is
/// indistinguishable from data loss to whoever reports it.
pub fn decode(cursor: &str) -> Result<(Timestamp, Id)> {
    let mut parts = cursor.split(SEPARATOR);
    let version = parts.next().unwrap_or_default();
    if version != VERSION {
        return Err(invalid("unrecognised cursor version"));
    }
    let created = parts
        .next()
        .ok_or_else(|| invalid("missing creation time"))?;
    let other = parts.next().ok_or_else(|| invalid("missing account"))?;
    if parts.next().is_some() {
        return Err(invalid("trailing data"));
    }

    Ok((
        Timestamp::from_millis(
            created
                .parse::<i64>()
                .map_err(|_| invalid("creation time is not a timestamp"))?,
        ),
        Id::parse(other).map_err(|_| invalid("account is not an identifier"))?,
    ))
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

    #[test]
    fn a_position_survives_a_round_trip() {
        for original in [
            (Timestamp::from_millis(1_700), Id::from(7)),
            (Timestamp::from_millis(0), Id::from(1)),
        ] {
            let decoded = decode(&encode(original)).expect("its own output parses");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn a_cursor_of_another_layout_is_refused_rather_than_guessed_at() {
        for text in [
            "",
            "v2.1.2",
            "v1.1",
            "v1.1.2.3",
            "v1.not-a-time.2",
            "v1.1.not-an-id",
        ] {
            assert!(
                decode(text).is_err(),
                "{text:?} must not parse as a position"
            );
        }
    }
}
