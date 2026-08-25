# ADR-0009 — Deterministic simulation testing for network behaviour

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §94, §96, §134

## Context

The bugs that hurt a messenger most — lost message on reconnect, duplicate after retry,
wrong order after a partition, stuck queue after clock skew — are timing-dependent and
almost impossible to reproduce from a user report.

## Decision

The sync engine, connection manager and fanout path depend on injected `Clock`, `Rng` and
`Transport` traits. A simulation harness drives them with a virtual clock and a seeded
fault injector (delay, reorder, duplicate, drop, partition, mid-frame disconnect, skew).
Seeds are printed on failure and replay exactly: `SIM_SEED=1234 cargo test`.

## Consequences

No wall-clock sleeps in tests, so the suite stays fast, and network bugs become
reproducible artefacts. The cost is a discipline: no direct `Instant::now()`, `rand::random()`
or socket use in the affected layers — enforced in review.
