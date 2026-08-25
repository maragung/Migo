# ADR-0001 — Modular monolith with role composition, not microservices

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §92, §99

## Context

Migo spans ~20 subsystems and must run on a laptop and across a multi-region cluster. The
default industry reflex is a microservice per subsystem, which buys independent scaling and
costs network hops, distributed transactions, deploy orchestration and a debugging tax —
all before the first user.

## Options

| Option                                                               | Pros                                                                               | Cons                                                                                 |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Microservices from day one                                           | Independent scaling/deploys                                                        | Ops complexity, latency per hop, hard local dev, premature boundaries that are wrong |
| Single monolithic crate                                              | Simplest build                                                                     | Boundaries erode; everything depends on everything within a quarter                  |
| Modular monolith, one crate per subsystem, roles selected at runtime | Compile-time boundaries, one artefact, N topologies, split later without rewriting | Shared process failure domain until split; needs discipline on layering              |

## Decision

One binary `migod`; each subsystem is its own crate with downward-only dependencies;
active subsystems are chosen by `MIGO_NODE__ROLES`. A role is extracted into its own
deployable only when it needs a different scaling axis, failure domain, or release cadence.

## Consequences

Local development is a single `cargo run`. Cross-subsystem calls are function calls today
and can become mesh RPCs behind the same trait later. We must enforce the layering (a
dependency cycle is a build error) and keep per-role resource accounting so one role
cannot starve another in a shared process.
