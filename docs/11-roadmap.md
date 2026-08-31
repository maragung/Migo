# 11 — Roadmap

Phases follow brief §127. The ordering principle (brief §130): **security, reliability,
messaging, encryption, bandwidth, rooms, moderation, multi-region, performance, economy,
games, social.** Features never jump the queue ahead of core stability.

## Phase 0 — Foundation _(done)_

Repository structure, protocol IDL + code generation for Rust and TypeScript, wire codec
with limits and vectors, crypto primitives and key management, configuration and
telemetry, storage abstraction with in-memory and Postgres backends, cost-based rate
limiting, gateway session state machine, REST surface, web client shell with offline
store and connection manager, docs and ADRs, dev tooling.

## Phase 1 — Identity & private chat _(current)_

Registration and login (password + passkey scaffolding), device sessions with rotating
refresh tokens, profile basics, prekey publication and bundle fetch, 1:1 chat with X3DH +
Double Ratchet, cursor sync, receipts, offline queue, reconnect/resume, web client chat UI,
Android client skeleton, single-region gateway.

**Exit criteria:** two devices exchange E2E messages across a restart and a network
outage, with nothing lost or duplicated; the server database contains no plaintext;
load test at 10k concurrent sessions holds the SLO.

## Phase 2 — Social & rooms

Friends, follows, blocks, privacy modes, presence with aggregation, Public Rooms, join/leave,
room message fanout, discovery v1, group chat with sender-key encryption, media upload with
signed URLs and thumbnails, notifications with redacted push payloads.

## Phase 3 — Managed Rooms & moderation

Six-role hierarchy, permission overrides, member approval and invites, bans/mutes/warnings,
slow mode, word and link filters, pinned and scheduled announcements, audit logs, reports and
review queue, moderator dashboard, admin panel, automated abuse detection ladder.

## Phase 4 — Economy & progression

Double-entry ledger, coins/gems/points, gifts (animated, limited, collectible), shop and
cosmetics, avatar items and frames, XP with anti-farming, levels, badges, achievements,
leaderboards with snapshotting.

## Phase 5 — Bots & games

Bot registry, permission review, sandboxed runtime with quotas, Bot API and Rust SDK,
game engine with server-authoritative state and deltas, built-in games (dice, coin, RPS,
guess, trivia, word, 2048, tic-tac-toe), tournaments, game leaderboards, bot marketplace.

## Phase 6 — Feed, events & translation

Social feed with ranking, posts/polls/reactions/comments, hashtags and mentions, event
system with scheduling and rewards, on-demand translation, advanced discovery.

## Phase 7 — Federation & scale

Full mesh handshake and routing gossip, room home regions and edge shards, automatic
failover and ownership transfer, cross-region replication, regional relays, capacity
autoscaling, QUIC client data paths and federation-over-QUIC (the optional QUIC
listener on the server is already built), per-region feature flags.

## Post-MVP (brief §129)

Voice and video calls, advanced avatars (3D), creator systems, richer bot marketplace,
advanced analytics.

## Where the code stands today

| Area                                                             | State                                                        |
| ---------------------------------------------------------------- | ------------------------------------------------------------ |
| Protocol IDL + codegen (Rust & TS)                               | Implemented, vector-tested                                   |
| Wire codec + limits + fuzz targets                               | Implemented                                                  |
| Crypto: identity, tokens, Argon2id, AEAD, HKDF, X3DH, ratchet    | Implemented, cross-language vectors                          |
| Config, telemetry, error taxonomy, IDs                           | Implemented                                                  |
| Store: traits + in-memory + Postgres + migrations                | Implemented (Phase 1 scope)                                  |
| Cache: traits + in-memory + Redis                                | Implemented                                                  |
| Rate limiting (cost-based)                                       | Implemented                                                  |
| Auth: register/login/sessions/refresh rotation/devices           | Implemented                                                  |
| Messaging: conversations, seq, dedup, receipts, offline queue    | Implemented                                                  |
| Gateway: sessions, subscriptions, backpressure, batching, resume | Implemented                                                  |
| REST API                                                         | Implemented (Phase 1 surface)                                |
| Web client: shell, offline store, connection manager, chat UI    | Implemented                                                  |
| Presence, rooms, social, media, notify                           | Phase 2 — real types and tests, narrow behaviour             |
| Moderation, economy, games, bots                                 | Phase 3–5 — engine skeletons with tests                      |
| Federation                                                       | Phase 7 — handshake implemented and tested, routing skeleton |
| Android                                                          | Skeleton + architecture notes                                |

Each crate's module documentation states its phase. Thin is intentional and labelled;
thin _and unlabelled_ is a bug.
