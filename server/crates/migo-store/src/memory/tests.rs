//! Tests of the in-memory backend itself.
//!
//! The behaviour every backend must share lives in `tests/contract/mod.rs` and is
//! run against this one from `tests/memory_contract.rs`. What is left here is what
//! only makes sense in-process: the private index helpers, the fail-closed enum
//! parsing the store's callers depend on, and the counters that no durable backend
//! can offer.

use migo_core::Secret;
use migo_protocol::{MessageKind, Platform};

use super::*;
use crate::model::{DeviceStatus, Visibility};

// --- fixtures -------------------------------------------------------------

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

async fn seed_account(store: &MemoryStore, value: u128, username: &str) -> Id {
    let account_id = id(value);
    store
        .create_account(NewAccount {
            account_id,
            username: username.to_string(),
            email: Some(format!("{username}@example.test")),
            phone: None,
            password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
            locale: "id-ID".to_string(),
            country: Some("ID".to_string()),
            created_at: ts(1_000),
        })
        .await
        .unwrap();
    store
        .create_profile(Profile {
            account_id,
            display_name: username.to_string(),
            bio: None,
            avatar_media_id: None,
            birth_year: None,
            gender: None,
            show_last_seen: Visibility::Everyone,
            who_can_message: Visibility::Everyone,
            who_can_add: Visibility::Everyone,
            searchable: true,
            updated_at: ts(1_000),
        })
        .await
        .unwrap();
    account_id
}

async fn seed_device(store: &MemoryStore, account_id: Id, value: u128) -> Id {
    store
        .register_device(NewDevice {
            device_id: id(value),
            account_id,
            platform: Platform::Android,
            display_name: "Pixel".to_string(),
            app_version: "0.1.0".to_string(),
            os_version: Some("14".to_string()),
            device_model: Some("Pixel 8".to_string()),
            status: DeviceStatus::Active,
            public_credential: None,
            created_at: ts(2_000),
        })
        .await
        .unwrap()
        .device_id
}

async fn seed_group(store: &MemoryStore, conversation_id: Id, members: Vec<Id>) -> Conversation {
    let created_by = members[0];
    store
        .create_conversation(
            Conversation {
                conversation_id,
                kind: ConversationKind::Group,
                encryption: EncryptionMode::EndToEnd,
                room_id: None,
                last_seq: 0,
                created_by,
                created_at: ts(3_000),
                last_message_at: None,
                archived_at: None,
            },
            members,
        )
        .await
        .unwrap()
}

fn message_row(value: u128, conversation_id: Id, sender_id: Id, created_at: i64) -> NewMessage {
    NewMessage {
        message_id: id(value),
        conversation_id,
        sender_id,
        sender_device: None,
        kind: MessageKind::Text,
        envelope: vec![0xde, 0xad, 0xbe, 0xef],
        reply_to: None,
        expires_at: None,
        created_at: ts(created_at),
    }
}

// --- index helpers --------------------------------------------------------

#[test]
fn folding_is_the_one_definition_of_same_name() {
    assert_eq!(fold("  Alice  "), "alice");
    assert_eq!(fold("ALICE"), fold("alice"));
    assert_eq!(fold("Migo Room"), "migo room");
}

#[test]
fn the_direct_pair_key_ignores_who_asked() {
    let a = id(10);
    let b = id(20);
    assert_eq!(pair(a, b), pair(b, a));
    assert_eq!(pair(a, b), (a, b));
}

// --- fail-closed parsing --------------------------------------------------

#[tokio::test]
async fn an_unknown_status_reads_as_suspended() {
    // Fail-closed parsing, asserted here rather than only in the enum's own
    // module because this is the behaviour the store's callers depend on: a row
    // written by a newer build must not read as "let them in".
    assert_eq!(AccountStatus::from_i16(99), AccountStatus::Suspended);
    assert!(!AccountStatus::from_i16(99).can_sign_in());
    assert_eq!(Visibility::from_i16(99), Visibility::Nobody);
}

// --- the backend itself ---------------------------------------------------

#[tokio::test]
async fn the_backend_reports_what_it_is_and_that_it_is_up() {
    let store = MemoryStore::new();
    assert_eq!(store.backend_name(), "memory");
    store.migrate().await.unwrap();
    store.health().await.unwrap();

    let empty = store.counts();
    assert!(empty.iter().all(|(_, count)| *count == 0));

    let alice = seed_account(&store, 1, "alice").await;
    seed_device(&store, alice, 10).await;
    let conversation = seed_group(&store, id(50), vec![alice]).await;
    store
        .append_message(message_row(61, conversation.conversation_id, alice, 3_000))
        .await
        .unwrap();

    let counts: std::collections::HashMap<&str, usize> = store.counts().into_iter().collect();
    assert_eq!(counts["accounts"], 1);
    assert_eq!(counts["devices"], 1);
    assert_eq!(counts["conversations"], 1);
    assert_eq!(counts["messages"], 1);
    // A fresh store shares nothing with the previous one: a test that leaks state
    // into the next test is worse than no test.
    assert!(MemoryStore::new()
        .counts()
        .iter()
        .all(|(_, count)| *count == 0));
    assert!(MemoryStore::default()
        .counts()
        .iter()
        .all(|(_, count)| *count == 0));
}
