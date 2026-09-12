# ADR-0008 — Bounded session queues with a per-class drop policy

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §14, §15, §18, §55, §70

## Context

A phone in a tunnel holds a socket while the server accumulates frames for it. Unbounded
buffering turns one slow consumer into a node-wide OOM; a fixed small buffer that drops
indiscriminately loses messages.

## Decision

Every session has a bounded outbound queue. Each frame is classified `Critical`
(never dropped), `Coalescable` (newest replaces oldest per key) or `Droppable` (dropped and
counted). A session whose queue stays full past a deadline is closed with
`RESUME_REQUIRED`; the client resumes by cursor, losing nothing.

## Consequences

Memory per connection is bounded and predictable, so capacity planning is arithmetic.
Presence/typing storms cost O(1) per session. Closing a lagging socket is cheap **because**
resume-by-cursor exists — the two features are a package, and neither may be removed alone.

## Amendment — 2026-09-12: where the deadline is measured, and what the close says

Implementation (section 160) refined two points of the Decision:

1. **The deadline lives on the writer's drain, not on the queue's fullness.** The queue
   itself can never be the signal: the writer drains it into the socket at every loop
   head, so fullness never survives long enough for a tick to observe. The observable
   slow consumer is the writer's handoff — `transport.send`. A drain that never completes
   is abandoned at twice `LAGGING_DEADLINE_MS` (an abandoned write leaves a partial
   record, so nothing more is sent on that socket); a drain that completes but took
   longer than the deadline closes at the clean record boundary it just reached.
2. **The close is `SESSION_LAGGING`, not `RESUME_REQUIRED`.** The registry (§161) gives
   the 1000 class "fatal, cannot retry" semantics, which contradicts the instruction we
   actually want the client to follow — reconnect now and resume. A drain that completed
   on a clean boundary therefore sends a `RECONNECT_HINT` with
   `CloseReason::SessionLagging` and `after_ms: 0` directly on the transport before the
   FIN; a drain abandoned mid-record sends nothing, and the retained resume buffer is
   what makes that FIN recoverable.
