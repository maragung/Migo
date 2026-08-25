//! The storage contract, run against the in-memory backend.
//!
//! There are no assertions in this file on purpose. It exists to bind the suite in
//! `contract/mod.rs` to one backend, so the list of cases cannot drift between
//! backends: the macro names them, and both runners expand the same macro.

use std::sync::Arc;

use migo_store::{MemoryStore, SharedStore};

#[macro_use]
mod contract;

fn store() -> SharedStore {
    Arc::new(MemoryStore::new())
}

macro_rules! case {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            contract::$name(&store()).await;
        }
    };
}

for_each_contract_case!(case);
