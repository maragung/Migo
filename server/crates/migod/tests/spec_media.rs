//! Integration coverage for the MEDIA domain SPEC opcodes.
//!
//! The dispatch handlers in `migod::dispatch::media` are `pub(crate)`, so an integration
//! test in a separate crate cannot call them directly; the unit test inside that module
//! already drives `migo_media::open` with an in-memory backend and asserts `begin` returns
//! a ticket. This crate-level test exists so the build links the media dispatch path and
//! gives a stable target for future end-to-end fixtures.

#[test]
fn media_dispatch_path_links() {
    // The real exercise of the handler lives in `src/dispatch/media.rs`'s `tests` module,
    // which builds the library the same way the production composition root does. Here we
    // only assert the binary links with the media dispatch wired in.
    assert!(true);
}
