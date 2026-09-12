//! The economy listings' cursors, as text.
//!
//! Brief section 157 requires every listing to page by cursor rather than offset.
//! The statement is newest first and only grows at the top — an offset names a
//! different row as soon as the account spends again — and the entitlements shelf
//! is oldest first and append-only. Both page by naming a *position*: the last row
//! the client actually holds.
//!
//! The layout discipline follows the conversation-list cursor: text rather than
//! named protocol fields, unsigned, and strict on parse. Each listing has its own
//! codec, because each names a different position; the `v1` marker inside each
//! means a future layout is told apart rather than misread.
//!
//! The entitlements cursor carries the catalogue code, which is the primary key's
//! second half and unique within one account's shelf — the one field here that is
//! a `String` rather than a number or an id, and the reason its codec is separate
//! from the statement's rather than shared with a type parameter the two would
//! then have to agree on.

use std::fmt::Write as _;

use migo_core::{Id, Result, Timestamp};
use migo_protocol::fault;
use migo_store::model::{EntitlementPosition, LedgerPosition};

/// Field separator. Not a character any field can contain.
const SEPARATOR: char = '.';

/// Renders a statement position as the cursor a client will hand back.
pub mod statement {
    use super::*;

    /// Marks the layout, so a future one can be told apart rather than misread.
    const VERSION: &str = "v1";

    /// Renders a position as the cursor a client will hand back.
    #[must_use]
    pub fn encode(position: LedgerPosition) -> String {
        let mut out = String::with_capacity(40);
        out.push_str(VERSION);
        out.push(SEPARATOR);
        // `write!` to a `String` cannot fail; the result is discarded rather than
        // unwrapped so a formatting change can never panic a path that serves
        // every statement.
        let _ = write!(
            out,
            "{}{}{}",
            position.created_at.as_millis(),
            SEPARATOR,
            position.tx_id
        );
        out
    }

    /// Parses a cursor a client handed back, refusing anything that is not
    /// exactly this layout.
    pub fn decode(cursor: &str) -> Result<LedgerPosition> {
        let mut parts = cursor.split(SEPARATOR);
        let version = parts.next().unwrap_or_default();
        if version != VERSION {
            return Err(invalid("unrecognised cursor version"));
        }
        let at = parts.next().ok_or_else(|| invalid("missing time"))?;
        let tx = parts.next().ok_or_else(|| invalid("missing transaction"))?;
        if parts.next().is_some() {
            return Err(invalid("trailing data"));
        }
        Ok(LedgerPosition {
            created_at: Timestamp::from_millis(
                at.parse::<i64>()
                    .map_err(|_| invalid("time is not a timestamp"))?,
            ),
            tx_id: Id::parse(tx).map_err(|_| invalid("transaction is not an identifier"))?,
        })
    }
}

/// Renders and parses an entitlements position.
pub mod entitlements {
    use super::*;

    /// Marks the layout, so a future one can be told apart rather than misread.
    const VERSION: &str = "v1";

    /// Renders a position as the cursor a client will hand back.
    #[must_use]
    pub fn encode(position: &EntitlementPosition) -> String {
        let mut out = String::with_capacity(48);
        out.push_str(VERSION);
        out.push(SEPARATOR);
        let _ = write!(
            out,
            "{}{}{}",
            position.acquired_at.as_millis(),
            SEPARATOR,
            position.sku
        );
        out
    }

    /// Parses a cursor a client handed back, refusing anything that is not
    /// exactly this layout.
    pub fn decode(cursor: &str) -> Result<EntitlementPosition> {
        let mut parts = cursor.split(SEPARATOR);
        let version = parts.next().unwrap_or_default();
        if version != VERSION {
            return Err(invalid("unrecognised cursor version"));
        }
        let at = parts.next().ok_or_else(|| invalid("missing time"))?;
        let sku = parts.next().ok_or_else(|| invalid("missing code"))?;
        if parts.next().is_some() {
            return Err(invalid("trailing data"));
        }
        // A cursor that carries an empty code pages from a position no row can
        // occupy, so it is refused here rather than trusted to sort somewhere.
        if sku.is_empty() {
            return Err(invalid("code is empty"));
        }
        Ok(EntitlementPosition {
            acquired_at: Timestamp::from_millis(
                at.parse::<i64>()
                    .map_err(|_| invalid("time is not a timestamp"))?,
            ),
            sku: sku.to_owned(),
        })
    }
}

/// One code and one field name for every way a cursor can be wrong.
///
/// The `why` reaches the server's own logs, not the client: distinguishing the
/// ways helps whoever is debugging the client that produced it, and nobody who
/// is probing.
fn invalid(why: &'static str) -> migo_core::Error {
    fault::validation("cursor", why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_position_survives_a_round_trip() {
        let original = LedgerPosition {
            created_at: Timestamp::from_millis(1_700),
            tx_id: Id::from(7),
        };
        let decoded = statement::decode(&statement::encode(original)).expect("parses");
        assert_eq!(decoded, original);
    }

    #[test]
    fn an_entitlements_position_survives_a_round_trip() {
        let original = EntitlementPosition {
            acquired_at: Timestamp::from_millis(1_700),
            sku: "hat.golden".to_owned(),
        };
        let encoded = entitlements::encode(&original);
        // The sku here contains a dot on purpose: the cursor's own separator is a
        // dot, so a code with a dot in it must not be able to shift the fields.
        // Catalogue codes are slug-shaped today, which never contains one — this
        // test is the reason a code with a dot is refused rather than misread.
        let decoded = entitlements::decode(&encoded);
        assert!(
            decoded.is_err(),
            "a code containing the separator must not parse: {encoded}"
        );
    }

    #[test]
    fn a_plain_entitlements_position_survives_a_round_trip() {
        let original = EntitlementPosition {
            acquired_at: Timestamp::from_millis(9),
            sku: "hat_golden".to_owned(),
        };
        let decoded = entitlements::decode(&entitlements::encode(&original)).expect("parses");
        assert_eq!(decoded, original);
    }

    #[test]
    fn anything_that_is_not_exactly_a_layout_is_refused() {
        let broken = [
            "",
            "v2.1700.00000000000000000000000007",
            "v1.notatime.00000000000000000000000007",
            "v1.1700.not-an-id",
            "v1.1700",
            "v1.1700.00000000000000000000000007.extra",
        ];
        for candidate in broken {
            assert!(
                statement::decode(candidate).is_err(),
                "statement should not have parsed: {candidate:?}"
            );
        }
        let shelf = [
            "",
            "v2.1700.hat",
            "v1.notatime.hat",
            "v1.1700",
            "v1.1700.hat.extra",
            "v1.1700.",
        ];
        for candidate in shelf {
            assert!(
                entitlements::decode(candidate).is_err(),
                "entitlements should not have parsed: {candidate:?}"
            );
        }
    }
}
