# ADR-0010 — Generated protocol code is committed

- **Status:** Accepted · **Date:** 2026-08-18

## Context

Generated Rust/TypeScript can be produced at build time or committed. Build-time
generation keeps the tree clean but hides the blast radius of a schema change and requires
Node in every Rust build.

## Decision

Commit generated sources. `make protocol-check` fails CI if they are stale. Generated files
carry a "do not edit" header and are excluded from review-required paths but **not** from
review visibility.

## Consequences

A schema change shows its full effect in the diff, `cargo build` needs no Node, and builds
are reproducible offline. The cost is remembering `make protocol` — which CI enforces.

Committing generated code means it is also subject to every other gate that touches the
tree, and that had a consequence worth recording: **the generator must emit output already
in canonical form**. It did not at first, so `make fmt` reformatted the committed Rust and
`make protocol-check` then declared it stale — two gates that could not both pass, with
whichever ran last deciding which one failed. The generator now pipes its Rust through
`rustfmt --edition 2021` before comparing or writing, which makes `cargo fmt` a permanent
no-op on those files. TypeScript output is not yet formatted by anything in the repository,
so the generator's own layout is canonical there; the day prettier joins the toolchain it
needs the same treatment, for the same reason.

The general rule this is an instance of: a committed artefact has to be a _fixpoint_ of
every tool allowed to rewrite it, or two gates will disagree and the disagreement will
present as a mystery.
