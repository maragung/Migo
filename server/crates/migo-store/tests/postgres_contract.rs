//! The storage contract, run against PostgreSQL.
//!
//! Needs a real database, so it is opt-in: set `MIGO_TEST_DATABASE_URL` to a
//! PostgreSQL URL whose user may create databases, and every case runs. Without
//! it, each case returns immediately and says so once.
//!
//! ```text
//! MIGO_TEST_DATABASE_URL=postgres://migo@127.0.0.1:5432/postgres cargo test -p migo-store
//! ```
//!
//! Each case gets its own freshly migrated database, named after the case. Sharing
//! one database across cases that run in parallel would make them fail in each
//! other's names, which is the least useful kind of test failure there is.

use std::sync::Arc;

use migo_core::config::{StoreBackend, StoreConfig};
use migo_core::Secret;
use migo_store::{PostgresStore, SharedStore, Store};
use sea_orm::{ConnectionTrait, Database};

#[macro_use]
mod contract;

/// The maintenance URL, or `None` when the suite is not configured to run.
fn maintenance_url() -> Option<String> {
    match std::env::var("MIGO_TEST_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

/// Swaps the database name in a PostgreSQL URL, keeping everything else.
fn with_database(url: &str, database: &str) -> String {
    // Split on the last `/` before any query string: the path is the database name.
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };
    let trimmed = base.trim_end_matches('/');
    let cut = trimmed.rfind('/').unwrap_or(trimmed.len());
    let mut swapped = format!("{}/{database}", &trimmed[..cut]);
    if let Some(query) = query {
        swapped.push('?');
        swapped.push_str(query);
    }
    swapped
}

/// Creates an empty database for one case and returns a migrated store on it.
///
/// The drop-then-create is deliberate: a case that panicked last run left its
/// database behind, and a suite that then reports a stale failure is worse than one
/// that starts clean every time.
async fn setup(case: &str) -> Option<SharedStore> {
    let maintenance = maintenance_url()?;
    let database = format!("migo_contract_{case}");

    // `execute_unprepared`, because `create database` cannot run inside a prepared
    // statement — PostgreSQL refuses it in an implicit transaction block.
    let admin = Database::connect(&maintenance)
        .await
        .expect("MIGO_TEST_DATABASE_URL must be reachable");
    admin
        .execute_unprepared(&format!("drop database if exists {database} with (force)"))
        .await
        .expect("dropping a leftover database");
    admin
        .execute_unprepared(&format!("create database {database}"))
        .await
        .expect("creating the case database");
    admin.close().await.ok();

    let config = StoreConfig {
        backend: StoreBackend::Postgres,
        url: Some(Secret::new(with_database(&maintenance, &database))),
        // Two per case: the suite runs cases in parallel, and a pool sized for
        // production would exhaust the server's connection slots long before the
        // cases run out.
        max_connections: 2,
        acquire_timeout_ms: 10_000,
        statement_timeout_ms: 30_000,
    };
    let store = PostgresStore::connect(&config)
        .await
        .expect("connecting to the case database");
    store.migrate().await.expect("migrating the case database");
    Some(Arc::new(store) as SharedStore)
}

macro_rules! case {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            let Some(store) = setup(stringify!($name)).await else {
                return;
            };
            contract::$name(&store).await;
        }
    };
}

for_each_contract_case!(case);

#[tokio::test]
async fn the_backend_reports_what_it_is_and_that_it_is_up() {
    let Some(store) = setup("backend_identity").await else {
        eprintln!("MIGO_TEST_DATABASE_URL is not set: the PostgreSQL contract did not run");
        return;
    };
    assert_eq!(store.backend_name(), "postgres");
    // Twice, because a migration that is not idempotent fails on the second deploy
    // rather than the first, which is the worst time to find out.
    store.migrate().await.unwrap();
    store.health().await.unwrap();
}

/// Whether this run is required to have a real backend behind it.
///
/// CI sets `MIGO_TEST_REQUIRE_BACKENDS=1`. It stays unset on a laptop, where a developer
/// with no PostgreSQL running should get a green suite rather than a wall of red for a
/// service they were never asked to install.
fn backends_are_required() -> bool {
    matches!(
        std::env::var("MIGO_TEST_REQUIRE_BACKENDS").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Fails when CI believes it ran the PostgreSQL contract and did not.
///
/// Every case above returns early when `MIGO_TEST_DATABASE_URL` is unset, and the suite
/// then reports exactly the same count of passing tests as a run that touched a real
/// database. Deleting the service from the workflow, renaming the variable, or pointing
/// it at a host that stopped resolving would all look like a green build. This is the one
/// test that can tell the difference, and it exists because the alternative is trusting
/// that nobody will ever edit the workflow.
#[test]
fn the_contract_actually_ran_when_it_was_required_to() {
    if backends_are_required() {
        assert!(
            maintenance_url().is_some(),
            "MIGO_TEST_REQUIRE_BACKENDS is set but MIGO_TEST_DATABASE_URL is not: every \
             case in this suite skipped, and the build would have gone green anyway"
        );
    }
}
