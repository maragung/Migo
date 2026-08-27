//! Cache keys.
//!
//! A key is `m:<scope>:<tail>`. The scope is a `&'static str` chosen by this crate,
//! so a typo is a compile-time concern rather than a silent second keyspace. The
//! tail is caller data, and caller data reaches here from the network: an
//! idempotency key or a client-supplied cache tag is attacker-controlled. If the
//! tail were pasted in raw, a tail of `evil:m:route` would let one namespace write
//! into another's keys. So the tail is percent-escaped on the way in.
//!
//! Escaping rather than rejecting, because rejection would need a `Result` on every
//! key construction for a case that has no sensible recovery, and escaping is
//! reversible: two different tails cannot produce the same key.
//!
//! The `m:` prefix is fixed rather than configurable. Two Migo deployments sharing
//! one Redis instance is already solved by the database number in the URL
//! (`redis://host/0` versus `redis://host/1`), and a configurable prefix would add a
//! setting whose only purpose is to be got wrong.

use std::fmt::{self, Write as _};

use migo_core::Id;

/// The single-character prefix every Migo key carries.
///
/// One character, because it is transmitted on every Redis command and there are
/// hundreds of thousands of them per second.
pub const PREFIX: &str = "m";

/// Namespace for values a domain crate caches for itself: leaderboards, rendered
/// profiles, idempotency markers. Anything with no dedicated trait.
pub const SCOPE_KV: &str = "kv";
/// Namespace for window counters.
pub const SCOPE_COUNTER: &str = "cnt";
/// Namespace for token buckets.
///
/// Separate from [`SCOPE_COUNTER`] even though both are abuse control, because the two
/// are different Redis types — a counter is a string, a bucket is a hash — and one
/// caller reusing a tail across both would meet `WRONGTYPE` at runtime instead of at
/// compile time. A scope per type makes the collision unrepresentable.
pub const SCOPE_BUCKET: &str = "tb";
/// Namespace for the per-account presence hash.
pub const SCOPE_PRESENCE: &str = "pres";
/// Namespace for the per-conversation typing hash.
pub const SCOPE_TYPING: &str = "typ";
/// Namespace for the per-device session route.
pub const SCOPE_ROUTE: &str = "rt";
/// Namespace for the per-account index of session routes.
pub const SCOPE_ROUTE_INDEX: &str = "rti";

/// Longest tail a key may carry.
///
/// Redis itself allows keys up to 512 MB, which is not a limit so much as an
/// invitation. 256 bytes is more than any legitimate tail needs — an id renders to
/// 26 characters — and it bounds what an unauthenticated caller can make the server
/// hash and store per key.
pub const MAX_TAIL_BYTES: usize = 256;

/// A namespaced cache key.
///
/// Constructed, never parsed: nothing in Migo reads a key back out of the cache and
/// needs to know which scope it came from, so there is no `from_str`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey(String);

impl CacheKey {
    /// A key in `scope` naming `tail`.
    ///
    /// `tail` is escaped and truncated at [`MAX_TAIL_BYTES`]. Truncation happens on
    /// the escaped form and at a character boundary, so the result is always valid
    /// UTF-8 and always a prefix of what was asked for.
    ///
    /// `scope` is a literal and is not escaped, so it must already be safe: lowercase
    /// ASCII words joined by underscores. A colon in a scope would be a scope pretending
    /// to be two scopes, which makes a key prefix ambiguous, so it is refused.
    #[must_use]
    pub fn new(scope: &'static str, tail: &str) -> Self {
        debug_assert!(
            !scope.is_empty() && scope.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "scope must be lowercase ASCII words joined by underscores, got {scope:?}"
        );
        let mut text = String::with_capacity(PREFIX.len() + scope.len() + tail.len() + 2);
        text.push_str(PREFIX);
        text.push(':');
        text.push_str(scope);
        text.push(':');
        escape_into(&mut text, tail);
        Self(text)
    }

    /// A key in `scope` naming an id. The common case, and it needs no escaping:
    /// [`Id::to_text`] is Crockford base32.
    #[must_use]
    pub fn of_id(scope: &'static str, id: Id) -> Self {
        Self::new(scope, &id.to_text())
    }

    /// A key in `scope` naming a pair of ids, ordered as given.
    ///
    /// The two halves are separated by `/`, which `escape_into` never emits, so a
    /// pair key can never collide with a single-id key or with another pair.
    #[must_use]
    pub fn of_pair(scope: &'static str, first: Id, second: Id) -> Self {
        Self::new(scope, &format!("{}/{}", first.to_text(), second.to_text()))
    }

    /// The key as Redis and the memory backend see it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The key itself, not `CacheKey("...")`: keys appear in log lines and error
        // messages, and one layer of quoting there is enough.
        write!(f, "{}", self.0)
    }
}

/// Appends `tail` with everything outside the safe set percent-escaped.
///
/// Safe means unreserved in the key grammar: ASCII letters, digits, `-`, `_`, `.`,
/// and `/`. Everything else — `:` above all, but also whitespace, control bytes, and
/// any non-ASCII byte — becomes `%XX`. `%` itself escapes to `%25` so the mapping
/// stays injective.
fn escape_into(out: &mut String, tail: &str) {
    for byte in tail.bytes() {
        if out.len() >= PREFIX.len() + MAX_TAIL_BYTES {
            break;
        }
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                out.push(byte as char);
            }
            // `write!` to a String cannot fail; the result is discarded rather than
            // unwrapped so a formatting change can never panic a request.
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_carries_its_prefix_and_scope() {
        let key = CacheKey::new(SCOPE_KV, "leaderboard.weekly");
        assert_eq!(key.as_str(), "m:kv:leaderboard.weekly");
    }

    #[test]
    fn a_hostile_tail_cannot_reach_another_namespace() {
        // Without escaping this would be `m:kv:m:rt:0198`, which the route namespace
        // would answer to.
        let key = CacheKey::new(SCOPE_KV, "m:rt:0198");
        assert_eq!(key.as_str(), "m:kv:m%3Art%3A0198");
    }

    #[test]
    fn escaping_is_injective() {
        // The pair that motivates escaping `%`: without `%25`, these two collide.
        let a = CacheKey::new(SCOPE_KV, "a:b");
        let b = CacheKey::new(SCOPE_KV, "a%3Ab");
        assert_ne!(a, b);
    }

    #[test]
    fn control_bytes_and_non_ascii_do_not_survive_raw() {
        let key = CacheKey::new(SCOPE_KV, "hai\r\n mig\u{f8}");
        assert_eq!(key.as_str(), "m:kv:hai%0D%0A%20mig%C3%B8");
    }

    #[test]
    fn a_long_tail_is_cut_at_the_limit_and_stays_utf8() {
        let key = CacheKey::new(SCOPE_KV, &"\u{f8}".repeat(500));
        assert!(key.as_str().len() <= PREFIX.len() + MAX_TAIL_BYTES + SCOPE_KV.len() + 2);
        assert!(std::str::from_utf8(key.as_str().as_bytes()).is_ok());
    }

    #[test]
    fn a_pair_key_cannot_be_confused_with_a_single() {
        let first = Id::from(1u128);
        let second = Id::from(2u128);
        let pair = CacheKey::of_pair(SCOPE_COUNTER, first, second);
        let single = CacheKey::of_id(SCOPE_COUNTER, first);
        assert_ne!(pair, single);
        assert!(pair.as_str().contains('/'));
    }
}
