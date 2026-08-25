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
