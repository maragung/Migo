<h1 align="center">Migo</h1>
<p align="center">
  <b>Global real-time communication &amp; community platform.</b><br/>
  Rust backend · binary low-bandwidth protocol · automatic end-to-end encryption ·
  multi-region server mesh · Next.js web · native Android.
</p>

---

## What Migo is

Migo rebuilds the best ideas of the mig33 era — Public Rooms, Managed Rooms, global chat,
friends, avatars, virtual gifts, game bots — on a modern foundation:

| Pillar         | Choice                                                                                |
| -------------- | ------------------------------------------------------------------------------------- |
| Backend        | Rust, modular monolith with role composition (`MIGO_ROLES`)                           |
| Realtime       | WebSocket now, QUIC-ready; custom **binary** frame protocol (varint, opcode registry) |
| Privacy        | X3DH + Double Ratchet for 1:1, sender-key for groups, **on by default**               |
| Storage        | PostgreSQL (source of truth) · Redis (ephemeral/presence) · S3 (media)                |
| Web            | Next.js App Router, TypeScript, PWA, offline-first (IndexedDB)                        |
| Mobile         | Native Android (Kotlin / Jetpack Compose) — see `clients/android`                     |
| Protocol truth | One IDL in `shared/protocol/schema`, code-generated for Rust **and** TypeScript       |

Design north star: **do more with fewer bytes.** Every feature is measured against a
[bandwidth budget](docs/05-bandwidth-budget.md).

## Repository layout

```
migo/
├── server/            Rust backend (Cargo workspace, 22 crates)
│   ├── crates/        migo-wire, migo-crypto, migo-gateway, migo-api, migod, …
│   ├── migrations/    Versioned SQL migrations
│   └── tests/         Cross-crate integration & protocol conformance tests
├── clients/
│   ├── web/           Next.js 15 web client (frontend)
│   └── android/       Native Android client
├── packages/          Shared TypeScript packages (wire, protocol, crypto, sdk)
├── shared/protocol/   Protocol IDL + cross-language test vectors  ← single source of truth
├── tools/             Code generator, load generator, dev scripts
├── infra/             Docker, Compose, Kubernetes, Terraform
├── docs/              Architecture, protocol spec, threat model, ADRs, runbooks
└── tests/             End-to-end and load tests
```

Full rationale: **[docs/01-architecture.md](docs/01-architecture.md)**.

## Quick start

```bash
# 0. one-time: install toolchains (Rust 1.90+, Node 20+, pnpm 9+)
make setup

# 1. generate protocol code for Rust + TypeScript from the IDL
make protocol

# 2. run the whole stack with zero external dependencies (in-memory store)
make dev            # migod on :8080 (HTTP+WS) + web client on :19991

# …or with real Postgres/Redis/MinIO
make infra-up && make dev-pg
```

`make help` lists every target.

## Status

Migo is built in phases (see [docs/11-roadmap.md](docs/11-roadmap.md)).
Phase 1 (identity, 1:1 E2E chat, presence, rooms skeleton, web client) is the current focus;
later-phase crates exist with real types and tests but intentionally narrow behaviour.
Each crate's `README`/module docs state its phase.

## Non-negotiables

1. No custom cryptographic primitives — only audited libraries. ([docs/03](docs/03-security-threat-model.md))
2. No plaintext private message ever reaches a server. Not even for admins.
3. No secret in Git. Config is layered and validated at boot, fail-fast.
4. No feature is "done" on a green build alone — see [docs/10-testing-strategy.md](docs/10-testing-strategy.md).
5. No unbounded queue, no unbounded retry, no unbounded payload.

## License

Apache-2.0 — see [LICENSE](LICENSE).
