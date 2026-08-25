# Contributing to Migo

## Before you write code

1. Read [docs/12-coding-standards.md](docs/12-coding-standards.md) and the doc for the
   area you are touching.
2. Check [docs/11-roadmap.md](docs/11-roadmap.md): a Phase 5 crate is thin on purpose.
3. If your change decides something lasting, open an ADR first
   ([docs/adr/0000-template.md](docs/adr/0000-template.md)). It is cheaper to argue about
   a one-page ADR than about a 3 000-line pull request.

## Local setup

```bash
make setup        # toolchains + JS deps
make protocol     # generate Rust + TypeScript from the protocol IDL
make dev          # migod (in-memory) + web client
```

No Docker, Postgres or Redis is required for the default dev loop — the in-memory
backends are real implementations of the same traits, not mocks.

## The loop

```bash
make check         # fast: does it compile?
make test-server   # Rust tests
make test-contract # store + cache contract suites against real Postgres/Redis
make lint          # clippy -D warnings + eslint
make ci            # everything CI will run
```

`make test-contract` is the one that needs `MIGO_TEST_DATABASE_URL` and
`MIGO_TEST_REDIS_URL`. Without them the suites still pass, because the cases for a
missing backend early-return — correct on a laptop, and the reason CI sets both
([docs/10-testing-strategy.md](docs/10-testing-strategy.md) §3).

## Changing the protocol

The IDL in `shared/protocol/schema` is the single source of truth.

1. Edit the schema. **Adding** an optional field, an opcode, an enum variant or an error
   code is backward compatible. Changing or removing a _required_ field is not, and needs
   an ADR plus a protocol version plan.
2. `make protocol` — regenerates Rust and TypeScript. Generated files are committed so
   diffs are reviewable and builds are reproducible; never hand-edit them.
3. Add vectors in `shared/protocol/vectors` and make both languages pass them.
4. Declare a rate-limit cost for a new opcode. CI rejects opcodes without one.

## Pull requests

- One logical change. If the description needs the word "also", split it.
- Fill in the definition-of-done checklist from
  [docs/10-testing-strategy.md](docs/10-testing-strategy.md) §1.
- State the bandwidth impact of anything that touches the wire.
- Green CI is necessary, not sufficient. Say how you verified behaviour under reconnect
  and offline.

## What gets a change rejected quickly

- A secret, key or token in the diff.
- A new cryptographic construction written by hand.
- `unwrap()` in a request path.
- An unbounded queue, retry loop or payload size.
- A permission check on the client only.
- A user-facing string hardcoded in a component.
- A hot-path metric label containing a user or room id.
- A generated file edited by hand.
