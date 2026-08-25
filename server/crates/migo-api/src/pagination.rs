//! Cursor pagination, one shape for every listing the surface will grow.
//!
//! Brief section 118 requires every listing to be paginated with a server-side maximum, so that
//! neither a forgetful client nor a hostile one can ask for an unbounded page. The rule lives
//! here rather than in each handler: [`PageParams`] is the query a client sends, and
//! [`effective_limit`](PageParams::effective_limit) clamps whatever it asked for into
//! `1..=MAX_PAGE_SIZE` — a missing limit becomes [`DEFAULT_PAGE_SIZE`], an absurd one becomes
//! [`MAX_PAGE_SIZE`], and neither is an error, because a limit is a hint the server is free to
//! cap. [`Page`] is the envelope a listing returns: the items, and an opaque cursor for the next
//! page when there is one.

use serde::{Deserialize, Serialize};

/// The page size used when a client names none.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// The largest page the server will ever return, whatever a client asks for.
pub const MAX_PAGE_SIZE: u32 = 200;

/// The pagination query a client sends: an opaque cursor into the sequence and a requested size.
///
/// Both are optional. The cursor is whatever the previous [`Page`] handed back, so a client
/// treats it as opaque and the server owns its meaning; the limit is a hint, clamped by
/// [`effective_limit`](PageParams::effective_limit).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PageParams {
    /// The opaque cursor from a previous page, or `None` for the first page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// The requested page size, clamped server-side; `None` uses [`DEFAULT_PAGE_SIZE`].
    #[serde(default)]
    pub limit: Option<u32>,
}

impl PageParams {
    /// The page size to actually use: the requested size clamped to `1..=MAX_PAGE_SIZE`, or
    /// [`DEFAULT_PAGE_SIZE`] when none was requested.
    #[must_use]
    pub fn effective_limit(&self) -> u32 {
        self.limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE)
    }

    /// The cursor to resume from, if any.
    #[must_use]
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
}

/// A single page of a listing: the items, and the cursor to fetch the next page when one exists.
///
/// A `next_cursor` of `None` means the sequence is exhausted; a present one is opaque and is fed
/// back verbatim as [`PageParams::cursor`] for the following page.
#[derive(Clone, Debug, Serialize)]
pub struct Page<T> {
    /// The items on this page, at most [`PageParams::effective_limit`] of them.
    pub items: Vec<T>,
    /// The cursor for the next page, or `None` at the end of the sequence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Assembles a page from its items and the optional next cursor.
    #[must_use]
    pub fn new(items: Vec<T>, next_cursor: Option<String>) -> Self {
        Self { items, next_cursor }
    }
}
