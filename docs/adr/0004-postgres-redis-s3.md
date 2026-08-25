# ADR-0004 — PostgreSQL + Redis + S3, no exotic datastore

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §51, §52, §66

## Context

The workload mixes strongly consistent data (accounts, permissions, currency), an
append-only high-volume stream (messages), ephemeral lossy state (presence) and blobs.

## Options

| Option                                                                   | Pros                                                                                       | Cons                                                                            |
| ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| Cassandra/Scylla for messages                                            | Linear write scale, multi-DC native                                                        | Second operational skill set, no transactions where we need them, painful early |
| Postgres for everything incl. presence                                   | One system                                                                                 | Presence churn wrecks WAL and vacuum                                            |
| Postgres (truth, partitioned+shardable) + Redis (ephemeral) + S3 (blobs) | One transactional store to reason about; ephemeral churn stays out of the WAL; cheap blobs | Sharding is manual work when it arrives                                         |

## Decision

PostgreSQL is the source of truth, messages partitioned monthly and shardable by
conversation. Redis holds only reconstructible ephemeral state. S3-compatible storage holds
media, uploaded directly with signed URLs and never proxied through the chat path.

## Consequences

One transactional model, straightforward local development, real backups. When a single
primary is exhausted we shard by conversation — every hot-path query is already
single-conversation, which is why that shard key was chosen now rather than later.
