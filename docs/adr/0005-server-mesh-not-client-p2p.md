# ADR-0005 — Federation is server-to-server; clients never peer directly

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §4, §7, §53–55

## Context

The brief uses "multi P2P server". Client-to-client P2P is a tempting reading and a trap.

## Options

| Option                              | Pros                                                                                  | Cons                                                                                       |
| ----------------------------------- | ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Client P2P for chat                 | Fewer server bytes                                                                    | Exposes user IPs, NAT traversal, no moderation, unreliable when peers sleep, worse battery |
| Central region only                 | Simple                                                                                | Global latency, single failure domain                                                      |
| Authenticated encrypted server mesh | Privacy preserved, moderation possible, reliable store-and-forward, regional failover | We build node identity, handshake, routing and replication                                 |

## Decision

Server-to-server mesh with Ed25519 node identity, mutual authentication (both nonces and
the peer id inside every signature, domain-separated), allow-listed nodes, sequence-based
replay protection and key rotation. Client P2P is reserved for optional future direct
media/calls with explicit consent, never for chat.

## Consequences

User IPs stay private and moderation stays possible. We own the mesh's correctness,
including the rule that a room has exactly one sequencing owner — ownership is never taken
over unilaterally during a partition.
