//! The conversation-list cursor, as text.
//!
//! Brief section 157 requires cursors rather than offsets. The reason is not
//! elegance: a conversation list reorders itself every time anybody sends
//! anything, so `offset=50` on the second request describes a different fiftieth
//! row than it did on the first, and the rows that moved across the boundary are
//! either shown twice or never shown. A keyset names a *position* — the last row
//! the client actually holds — and stays correct however much the list churns
//! underneath it.
//!
//! # Why it is text and not a struct
//!
//! [`ConversationListRequest::cursor`] is a `String` on the wire, and that is the
//! right shape for it: the client stores it, echoes it back, and never looks
//! inside. Encoding the three keyset fields as named protocol fields instead
//! would publish the list's sort order as part of the protocol, and changing the
//! order later would then be a protocol version rather than a query.
//!
//! # Why it is not signed
//!
//! A signature would buy nothing. The cursor names a position in the *caller's
//! own* conversation list, the list is re-authorised on every request, and every
//! row that comes back is a row the caller is a member of. Forging one lets an
//! attacker page their own list from a place they made up. What forging cannot do
//! is reach a conversation they are not in, because the position is a `where`
//! clause and the membership join is a different one.
//!
//! It follows that a malformed cursor is a client bug, not an attack, and it is
//! answered as `VALIDATION_FAILED` rather than as a permission problem.
//!
//! [`ConversationListRequest::cursor`]: migo_protocol::ConversationListRequest::cursor

use std::fmt::Write as _;

use migo_core::{Id, Result, Timestamp};
use migo_protocol::fault;
use migo_store::model::ConversationPosition;

/// Marks the layout, so a future one can be told apart rather than misread.
///
/// Without it, adding a fourth field would make every cursor issued by the old
/// build parse as a truncated version of the new layout — which does not fail, it
/// silently pages from the wrong place.
const VERSION: &str = "v1";

/// Stands in for a conversation that has never carried a message.
///
/// A literal is needed because the field is nullable and the encoding is
/// positional. `-` is not a digit, so it cannot collide with a timestamp, and it
/// reads as "nothing here" to whoever is looking at a cursor in a log.
const ABSENT: &str = "-";

/// Field separator. Not a character any of the three fields can contain.
const SEPARATOR: char = '.';

/// Renders a position as the cursor a client will hand back.
#[must_use]
pub fn encode(position: ConversationPosition) -> String {
    let mut out = String::with_capacity(48);
    out.push_str(VERSION);
    out.push(SEPARATOR);
    match position.last_message_at {
        // `write!` to a `String` cannot fail; the result is discarded rather than
        // unwrapped so that a formatting change can never introduce a panic on a
        // path that serves every conversation list.
        Some(at) => {
            let _ = write!(out, "{}", at.as_millis());
        }
        None => out.push_str(ABSENT),
    }
    out.push(SEPARATOR);
    let _ = write!(
        out,
        "{}{}{}",
        position.created_at.as_millis(),
        SEPARATOR,
        position.conversation_id
    );
    out
}

/// Parses a cursor a client handed back.
///
/// Fails with `VALIDATION_FAILED` on anything that is not exactly this layout.
/// Being strict is deliberate: a cursor that parses loosely pages from a position
/// nobody chose, and the symptom is a client that skips conversations, which is
/// indistinguishable from data loss to whoever reports it.
pub fn decode(cursor: &str) -> Result<ConversationPosition> {
    let mut parts = cursor.split(SEPARATOR);
    let version = parts.next().unwrap_or_default();
    if version != VERSION {
        return Err(invalid("unrecognised cursor version"));
    }
    let last = parts.next().ok_or_else(|| invalid("missing activity"))?;
    let created = parts
        .next()
        .ok_or_else(|| invalid("missing creation time"))?;
    let conversation = parts
        .next()
        .ok_or_else(|| invalid("missing conversation"))?;
    if parts.next().is_some() {
        return Err(invalid("trailing data"));
    }

    let last_message_at = if last == ABSENT {
        None
    } else {
        Some(Timestamp::from_millis(
            last.parse::<i64>()
                .map_err(|_| invalid("activity is not a timestamp"))?,
        ))
    };
    Ok(ConversationPosition {
        last_message_at,
        created_at: Timestamp::from_millis(
            created
                .parse::<i64>()
                .map_err(|_| invalid("creation time is not a timestamp"))?,
        ),
        conversation_id: Id::parse(conversation)
            .map_err(|_| invalid("conversation is not an identifier"))?,
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

    fn position(last: Option<i64>, created: i64, id: u128) -> ConversationPosition {
        ConversationPosition {
            last_message_at: last.map(Timestamp::from_millis),
            created_at: Timestamp::from_millis(created),
            conversation_id: Id::from(id),
        }
    }

    #[test]
    fn a_position_survives_a_round_trip() {
        for original in [position(Some(1_700), 1_200, 7), position(None, 9, 1)] {
            let decoded = decode(&encode(original)).expect("its own output parses");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn a_conversation_that_has_never_been_used_keeps_its_absence() {
        let encoded = encode(position(None, 5, 3));
        assert!(
            encoded.contains(".-."),
            "a null activity is a marker, not a zero: {encoded}"
        );
        assert!(decode(&encoded).expect("parses").last_message_at.is_none());
    }

    #[test]
    fn anything_that_is_not_exactly_the_layout_is_refused() {
        let good = encode(position(Some(1_700), 1_200, 7));
        let broken = [
            String::new(),
            "v2.1.2.00000000000000000000000007".to_string(),
            good.replace("v1", "v10"),
            format!("{good}.extra"),
            good.trim_end_matches(|c: char| c != '.').to_string(),
            "v1.notatime.1200.00000000000000000000000007".to_string(),
            "v1.1700.1200.not-an-id".to_string(),
        ];
        for candidate in broken {
            assert!(
                decode(&candidate).is_err(),
                "should not have parsed: {candidate:?}"
            );
        }
    }
}
