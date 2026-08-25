# ADR-0007 — Virtual currency as an append-only double-entry ledger

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §29, §87

## Context

Users will insist their coins vanished, and some will try to duplicate them. A mutable
`balance` column plus retries on a flaky mobile network is how both happen.

## Decision

Append-only `ledger_entry` rows; every transaction is ≥ 2 legs summing to zero; balances
are projections (with periodic snapshots); every transaction carries a unique
`idempotency_key`; all economy mutations are audit-logged. No cash-out, no wagering
mechanics.

## Consequences

Balance reads cost a little more (mitigated by snapshots) and we gain a complete, auditable
history, safe retries, and the ability to answer "where did my coins go?" exactly. Bugs
become correctable by compensating entries rather than by hand-editing balances.
