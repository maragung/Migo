# 12 — Coding standards

All code, comments, identifiers, commit messages and log messages are in **English**.
User-facing strings live in translation files, never in code.

## Rust

- Edition 2021, `rustfmt` default, `clippy -D warnings`. No exceptions committed without
  a justifying comment on the `#[allow]`.
- `#![forbid(unsafe_code)]` in every crate. If a crate ever needs `unsafe`, that is an ADR.
- **No `unwrap()`/`expect()`/`panic!` outside tests and startup.** A panic in a request
  handler takes down other users' sessions. Startup may panic — that is fail-fast.
- Errors: `thiserror` for libraries, one error enum per crate, mapped to a stable
  protocol error code at the transport boundary. `anyhow` only in binaries.
- Fallible functions return `Result<T, crate::Error>`; the crate re-exports `Result<T>`.
- No `async` in a lock. No `.await` while holding a `std::sync::Mutex`. Prefer message
  passing over shared mutable state on the hot path.
- Bounded channels only. An unbounded channel is a memory leak with a scheduler.
- `Bytes` for anything that will be cloned into many sends; `Vec<u8>` for owned buffers.
- Public items in a library crate have doc comments; every crate has a `//!` module doc
  stating its responsibility **and its phase**.
- Tests live next to the code (`mod tests`) for units, in `tests/` for integration.

## TypeScript

- `strict: true`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`. No `any`;
  `unknown` plus a narrowing function.
- No default exports (except Next.js pages/layouts, which require them).
- Server-state via the SDK's typed client; UI state in the store. **Components never
  fetch directly** — they read the local store, which the sync engine fills.
- Every user-visible string goes through `t()`.
- No `Date.now()` in render paths; time comes from a clock module so tests can control it.

## Naming

| Thing               | Convention                        | Example                        |
| ------------------- | --------------------------------- | ------------------------------ |
| Rust crate          | `migo-<area>`                     | `migo-messaging`               |
| Rust type / TS type | `PascalCase`                      | `ConversationCursor`           |
| Rust fn / var       | `snake_case`                      | `resolve_permissions`          |
| TS fn / var         | `camelCase`                       | `resolvePermissions`           |
| Protocol opcode     | `SCREAMING_SNAKE`                 | `MESSAGE_SEND`                 |
| Metric              | `migo_<subsystem>_<thing>_<unit>` | `migo_fanout_duration_seconds` |
| Feature flag        | `features.<area>_<thing>`         | `features.social_feed`         |
| Error symbol        | `SCREAMING_SNAKE`, stable forever | `RATE_LIMITED`                 |
| DB table            | `snake_case`, singular            | `room_member`                  |
| Migration           | `NNNN_description.sql`            | `0007_add_room_slow_mode.sql`  |

## Comments

Explain **why**, not what. Code says what it does; a comment exists because the reason is
not visible locally — a protocol constraint, a performance trade-off, a bug it prevents.
No commented-out code, no `TODO` without an issue reference.

## Reviews

A reviewer checks, in order: correctness, security (authz, input, secrets), failure modes
(what happens when this dependency is down?), bandwidth cost, observability, tests, then
style. Style comments last, because a tool should have caught them already.

## Commits

`<area>: <imperative summary>` (≤ 72 chars), e.g. `gateway: bound outbound queue per session`.
Body explains why. One logical change per commit; a mechanical rename is its own commit so
the real change stays reviewable.

## Dependencies

Adding one asks: is it maintained (release in the last year)? audited or widely used?
does it pull a tree we now own? could 50 lines of our own replace it? Crypto and
protocol dependencies get the strictest review. `cargo audit` / `pnpm audit` run in CI
(brief §93).
