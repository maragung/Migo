/**
 * The generated half of the protocol, and nothing else.
 *
 * Every export here comes from `src/generated.ts`, which `make protocol` writes from
 * `shared/protocol/schema`. The re-export exists so that consumers import
 * `@migo/protocol` rather than a path ending in `generated.js`: the file name is an
 * implementation detail of the code generator, and the day the generator emits two files
 * instead of one, no caller should have to change.
 *
 * Nothing hand-written may be added to `generated.ts` — it carries a DO NOT EDIT header
 * and is overwritten on every schema change. Hand-written helpers belong in this package
 * as separate modules, re-exported from here, or in `@migo/sdk` if they carry policy.
 *
 * The Rust side of this pair is the `migo-protocol` crate, generated from the same schema
 * by the same tool in the same run. That is what makes the two agree; the vectors in
 * `shared/protocol/vectors` are what proves it.
 */

export * from './generated.js';
