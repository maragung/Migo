//! The storage contract, as one suite that every backend has to pass.
//!
//! These are not tests of any one implementation. Each function pins an invariant
//! that the trait documentation promises: gapless sequence numbers, idempotent
//! appends, forward-only cursors, symmetric blocks, balanced ledgers, clamped
//! pages. Two backends behind one set of traits are only interchangeable if one
//! set of statements is true of both, so the statements live here, once, and both
//! `memory_contract.rs` and `postgres_contract.rs` run all of them.
//!
//! Every function takes the store it should exercise. Nothing here may name a
//! concrete backend: the moment a case needs to know which one it is talking to,
//! it stops being a contract and belongs in that backend's own file.

#![allow(clippy::too_many_lines)]

use migo_core::{Id, Result, Secret, Timestamp};
use migo_protocol::{
    codes, ConversationKind, EncryptionMode, MessageKind, Platform, RelationshipKind, RoomKind,
    RoomRole,
};
use migo_store::model::*;
use migo_store::traits::*;
use migo_store::SharedStore;

// --- fixtures -------------------------------------------------------------
//
// Ids and timestamps are literals rather than generated values. A test that
// mints its own uuid and reads its own clock cannot fail the same way twice,
// which is the opposite of what a regression test is for.

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

/// Asserts that a call failed with one specific protocol code.
///
/// Comparing the code rather than the message means a reworded internal string
/// does not break a test, while a change of failure *class* does.
#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    match result {
        Ok(_) => panic!("expected error {code}, got success"),
        Err(error) => assert_eq!(
            error.code(),
            code,
            "wrong code, internal message was: {}",
            error.internal_message()
        ),
    }
}

async fn seed_account(store: &SharedStore, value: u128, username: &str) -> Id {
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

async fn seed_device(store: &SharedStore, account_id: Id, value: u128) -> Id {
    store
        .register_device(NewDevice {
            device_id: id(value),
            account_id,
            platform: Platform::Android,
            display_name: "Pixel".to_string(),
            app_version: "0.1.0".to_string(),
            os_version: Some("14".to_string()),
            device_model: Some("Pixel 8".to_string()),
            created_at: ts(2_000),
        })
        .await
        .unwrap()
        .device_id
}

fn session_row(
    value: u128,
    account_id: Id,
    device_id: Id,
    family_id: Id,
    generation: i32,
    created_at: i64,
) -> NewSession {
    NewSession {
        session_id: id(value),
        account_id,
        device_id,
        family_id,
        refresh_hash: vec![value as u8; 32],
        generation,
        created_at: ts(created_at),
        authenticated_at: ts(created_at),
        access_expires_at: ts(created_at + 900_000),
        refresh_expires_at: ts(created_at + 2_592_000_000),
        ip_class: Some("203.0.113.0/24".to_string()),
        user_agent: Some("migo-web/0.1".to_string()),
    }
}

fn key_material(account_id: Id, device_id: Id, one_time: Vec<(i32, Vec<u8>)>) -> PublishedKeys {
    PublishedKeys {
        account_id,
        device_id,
        identity_key: vec![7u8; 64],
        signed_prekey_id: 1,
        signed_prekey: vec![8u8; 32],
        signed_prekey_signature: vec![9u8; 64],
        signed_prekey_expires_at: ts(90_000_000),
        one_time_prekeys: one_time,
        created_at: ts(2_000),
    }
}

async fn seed_group(store: &SharedStore, conversation_id: Id, members: Vec<Id>) -> Conversation {
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

fn room_row(
    room_id: Id,
    conversation_id: Id,
    slug: &str,
    owner_id: Id,
    max_members: i32,
) -> NewRoom {
    NewRoom {
        room_id,
        conversation_id,
        slug: slug.to_string(),
        name: slug.to_string(),
        topic: None,
        kind: RoomKind::Public,
        owner_id,
        home_region: "ap-southeast-1".to_string(),
        max_members,
        encryption: EncryptionMode::Transport,
        created_at: ts(4_000),
    }
}

fn member_row(room_id: Id, account_id: Id, joined_at: i64) -> RoomMember {
    RoomMember {
        room_id,
        account_id,
        role: RoomRole::Member,
        permissions_grant: 0,
        permissions_deny: 0,
        joined_at: ts(joined_at),
        left_at: None,
        muted_until: None,
        banned_until: None,
        ban_reason: None,
        invited_by: None,
    }
}

fn transaction_row(
    value: u128,
    idempotency_key: &str,
    currency: Currency,
    legs: Vec<LedgerLeg>,
) -> NewTransaction {
    NewTransaction {
        tx_id: id(value),
        reason: 1,
        ref_id: None,
        idempotency_key: idempotency_key.to_string(),
        created_by: None,
        currency,
        legs,
        receipt: None,
        created_at: ts(6_000),
    }
}

fn media_row(media_id: Id, owner_id: Id, byte_size: i64) -> MediaObject {
    MediaObject {
        media_id,
        owner_id,
        kind: 0,
        mime: "image/webp".to_string(),
        byte_size,
        width: Some(1024),
        height: Some(768),
        duration_ms: None,
        storage_key: "media/2026/08/object".to_string(),
        conversation_id: None,
        checksum: Some(vec![1u8; 32]),
        scan_status: media_scan::PENDING,
        created_at: ts(7_000),
        deleted_at: None,
    }
}

fn report_row(report_id: Id, reporter_id: Id, subject_id: Id, created_at: i64) -> Report {
    Report {
        report_id,
        reporter_id,
        subject_kind: 0,
        subject_id,
        room_id: None,
        reason: 3,
        note: Some("spam".to_string()),
        evidence_ref: None,
        status: report_status::OPEN,
        created_at: ts(created_at),
        resolved_at: None,
        resolved_by: None,
        resolution: None,
    }
}

fn audit_row(value: u128, action: &str, target_kind: i16, target_id: Id) -> AuditEntry {
    AuditEntry {
        audit_id: id(value),
        actor_id: Some(id(1)),
        actor_kind: 3,
        action: action.to_string(),
        target_kind,
        target_id: Some(target_id),
        summary: "changed something".to_string(),
        reason: None,
        request_id: Some("req-1".to_string()),
        ip_class: Some("203.0.113.0/24".to_string()),
        created_at: ts(8_000 + value as i64),
    }
}

// --- accounts and profiles ------------------------------------------------

pub async fn usernames_collide_case_insensitively(store: &SharedStore) {
    seed_account(store, 1, "Alice").await;

    let clash = store
        .create_account(NewAccount {
            account_id: id(2),
            username: "alice".to_string(),
            email: Some("other@example.test".to_string()),
            phone: None,
            password_hash: Secret::new("$argon2id$stub"),
            locale: "en-US".to_string(),
            country: None,
            created_at: ts(1_100),
        })
        .await;
    expect_code(clash, codes::ALREADY_EXISTS);

    // And the lookup agrees with the uniqueness rule, which is the half that
    // actually matters: an index that rejects a duplicate but cannot find the
    // original is worse than no index.
    let found = store.account_by_username("ALICE").await.unwrap().unwrap();
    assert_eq!(found.account_id, id(1));
    assert_eq!(
        found.username, "Alice",
        "the display form is preserved verbatim"
    );
}

pub async fn email_and_phone_are_unique_too(store: &SharedStore) {
    seed_account(store, 1, "alice").await;

    let same_email = store
        .create_account(NewAccount {
            account_id: id(2),
            username: "bob".to_string(),
            email: Some("ALICE@example.test".to_string()),
            phone: None,
            password_hash: Secret::new("$argon2id$stub"),
            locale: "id-ID".to_string(),
            country: None,
            created_at: ts(1_100),
        })
        .await;
    expect_code(same_email, codes::ALREADY_EXISTS);

    store
        .create_account(NewAccount {
            account_id: id(3),
            username: "carol".to_string(),
            email: None,
            phone: Some("+6281100000000".to_string()),
            password_hash: Secret::new("$argon2id$stub"),
            locale: "id-ID".to_string(),
            country: None,
            created_at: ts(1_200),
        })
        .await
        .unwrap();
    let same_phone = store
        .create_account(NewAccount {
            account_id: id(4),
            username: "dave".to_string(),
            email: None,
            phone: Some("+6281100000000".to_string()),
            password_hash: Secret::new("$argon2id$stub"),
            locale: "id-ID".to_string(),
            country: None,
            created_at: ts(1_300),
        })
        .await;
    expect_code(same_phone, codes::ALREADY_EXISTS);
}

pub async fn a_patch_tells_keep_apart_from_clear(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;

    let with_bio = store
        .update_profile(
            account_id,
            ProfilePatch {
                bio: Patch::Set("halo".to_string()),
                ..Default::default()
            },
            ts(1_500),
        )
        .await
        .unwrap();
    assert_eq!(with_bio.bio.as_deref(), Some("halo"));

    // Keep on every field: a patch that names nothing must change nothing, not
    // null out the columns it did not mention.
    let untouched = store
        .update_profile(account_id, ProfilePatch::default(), ts(1_600))
        .await
        .unwrap();
    assert_eq!(untouched.bio.as_deref(), Some("halo"));
    assert_eq!(untouched.updated_at, ts(1_600));

    let cleared = store
        .update_profile(
            account_id,
            ProfilePatch {
                bio: Patch::Clear,
                ..Default::default()
            },
            ts(1_700),
        )
        .await
        .unwrap();
    assert_eq!(cleared.bio, None);
}

pub async fn search_obeys_privacy_before_relevance(store: &SharedStore) {
    let hidden = seed_account(store, 1, "alicia").await;
    let suspended = seed_account(store, 2, "alina").await;
    let visible = seed_account(store, 3, "alice").await;

    store
        .update_profile(
            hidden,
            ProfilePatch {
                searchable: Some(false),
                ..Default::default()
            },
            ts(1_500),
        )
        .await
        .unwrap();
    store
        .set_status(
            suspended,
            AccountStatus::Suspended,
            Some(ts(9_000_000)),
            ts(1_500),
        )
        .await
        .unwrap();

    let hits = store.search_accounts("Ali", 50).await.unwrap();
    let ids: Vec<Id> = hits.iter().map(|(account, _)| account.account_id).collect();
    assert_eq!(
        ids,
        vec![visible],
        "opting out and being suspended both remove you"
    );

    assert!(store.search_accounts("   ", 50).await.unwrap().is_empty());
}

pub async fn search_clamps_an_abusive_limit(store: &SharedStore) {
    for n in 0..(MAX_PAGE as u128 + 50) {
        seed_account(store, n + 1, &format!("user{n:04}")).await;
    }
    let hits = store.search_accounts("user", u16::MAX).await.unwrap();
    assert_eq!(hits.len(), MAX_PAGE as usize);
}

// --- devices --------------------------------------------------------------

pub async fn last_seen_never_moves_backwards(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;

    store.touch_device(device_id, ts(5_000)).await.unwrap();
    store.touch_device(device_id, ts(4_000)).await.unwrap();
    let device = store.device_by_id(device_id).await.unwrap().unwrap();
    assert_eq!(
        device.last_seen_at,
        ts(5_000),
        "a device with a bad clock cannot rewind itself"
    );
}

pub async fn revoking_a_device_hides_it_but_keeps_the_row(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    seed_device(store, account_id, 12).await;

    store.revoke_device(device_id, ts(6_000)).await.unwrap();
    // Revocation is idempotent, and the second call must not restamp the first.
    store.revoke_device(device_id, ts(7_000)).await.unwrap();

    let listed = store.devices_for_account(account_id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].device_id, id(12));
    let revoked = store.device_by_id(device_id).await.unwrap().unwrap();
    assert_eq!(revoked.revoked_at, Some(ts(6_000)));
}

// --- sessions -------------------------------------------------------------

pub async fn a_rotated_token_cannot_be_exchanged_twice(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    let family = id(100);

    let first = store
        .create_session(session_row(21, account_id, device_id, family, 1, 10_000))
        .await
        .unwrap();
    let second = store
        .rotate_session(
            first.session_id,
            session_row(22, account_id, device_id, family, 2, 11_000),
        )
        .await
        .unwrap();
    assert_eq!(second.generation, 2);
    let superseded = store
        .session_by_id(first.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(superseded.rotated_at, Some(ts(11_000)));

    // The reuse attempt: the same retired token presented again.
    let replay = store
        .rotate_session(
            first.session_id,
            session_row(23, account_id, device_id, family, 2, 12_000),
        )
        .await;
    expect_code(replay, codes::CONFLICT);

    // A rotation must also be a real successor, not an arbitrary insert.
    let wrong_generation = store
        .rotate_session(
            second.session_id,
            session_row(24, account_id, device_id, family, 9, 13_000),
        )
        .await;
    expect_code(wrong_generation, codes::VALIDATION_FAILED);
    let foreign_family = store
        .rotate_session(
            second.session_id,
            session_row(25, account_id, device_id, id(999), 3, 13_000),
        )
        .await;
    expect_code(foreign_family, codes::VALIDATION_FAILED);
}

pub async fn a_session_carries_its_own_authentication_time(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    let family = id(100);

    // Presence was proved at 10_000. The generation that carries it forward was created
    // at 40_000, half a day later. The two must not be conflated: a rotation that reset
    // the authentication stamp would let a stolen refresh token keep itself permanently
    // eligible for password changes and account deletion, which is exactly the operation
    // set that stamp is there to guard.
    let mut first = session_row(21, account_id, device_id, family, 1, 10_000);
    first.authenticated_at = ts(10_000);
    let opened = store.create_session(first).await.unwrap();
    assert_eq!(opened.authenticated_at, ts(10_000));

    let mut next = session_row(22, account_id, device_id, family, 2, 40_000);
    next.authenticated_at = ts(10_000);
    let rotated = store.rotate_session(opened.session_id, next).await.unwrap();
    assert_eq!(rotated.created_at, ts(40_000));
    assert_eq!(rotated.authenticated_at, ts(10_000));

    // A round trip through the backend, not just the value the write returned: a column
    // the insert binds but the mapper skips reads back as the wrong time and nothing
    // else notices.
    let reloaded = store
        .session_by_id(rotated.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.authenticated_at, ts(10_000));
}

pub async fn reuse_detection_kills_the_whole_family(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    let family = id(100);
    let other_family = id(200);

    let first = store
        .create_session(session_row(21, account_id, device_id, family, 1, 10_000))
        .await
        .unwrap();
    store
        .rotate_session(
            first.session_id,
            session_row(22, account_id, device_id, family, 2, 11_000),
        )
        .await
        .unwrap();
    store
        .create_session(session_row(
            31,
            account_id,
            device_id,
            other_family,
            1,
            10_500,
        ))
        .await
        .unwrap();

    let revoked = store
        .revoke_family(family, RevokeReason::ReuseDetected, ts(12_000))
        .await
        .unwrap();
    assert_eq!(
        revoked, 2,
        "every generation dies, not only the one presented"
    );

    let live = store
        .sessions_for_account(account_id, ts(12_100))
        .await
        .unwrap();
    assert_eq!(live.len(), 1, "the unrelated family survives");
    assert_eq!(live[0].family_id, other_family);
    let dead = store
        .session_by_id(first.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dead.revoked_reason, Some(RevokeReason::ReuseDetected));
    assert!(!dead.is_live(ts(12_100)));
}

pub async fn logging_out_other_devices_spares_the_current_one(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    let keep = store
        .create_session(session_row(21, account_id, device_id, id(100), 1, 10_000))
        .await
        .unwrap();
    store
        .create_session(session_row(22, account_id, device_id, id(200), 1, 10_100))
        .await
        .unwrap();
    store
        .create_session(session_row(23, account_id, device_id, id(300), 1, 10_200))
        .await
        .unwrap();

    let revoked = store
        .revoke_account_sessions(
            account_id,
            Some(keep.session_id),
            RevokeReason::PasswordChanged,
            ts(11_000),
        )
        .await
        .unwrap();
    assert_eq!(revoked, 2);
    let live = store
        .sessions_for_account(account_id, ts(11_100))
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].session_id, keep.session_id);
}

pub async fn purging_a_session_forgets_its_refresh_hash(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    let session = store
        .create_session(session_row(21, account_id, device_id, id(100), 1, 10_000))
        .await
        .unwrap();
    let hash = session.refresh_hash.clone();
    assert!(store
        .session_by_refresh_hash(&hash)
        .await
        .unwrap()
        .is_some());

    let purged = store
        .purge_expired_sessions(session.refresh_expires_at.saturating_add_millis(1))
        .await
        .unwrap();
    assert_eq!(purged, 1);
    assert!(store
        .session_by_id(session.session_id)
        .await
        .unwrap()
        .is_none());
    assert!(
        store
            .session_by_refresh_hash(&hash)
            .await
            .unwrap()
            .is_none(),
        "a dangling hash index would resurrect a purged session"
    );
}

// --- key material ---------------------------------------------------------

pub async fn an_already_expired_signed_prekey_is_refused_on_arrival(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;

    let mut keys = key_material(account_id, device_id, Vec::new());
    keys.signed_prekey_expires_at = keys.created_at;
    expect_code(store.publish_keys(keys).await, codes::VALIDATION_FAILED);

    let orphan = key_material(account_id, id(999), Vec::new());
    expect_code(store.publish_keys(orphan).await, codes::NOT_FOUND);
}

pub async fn every_bundle_consumes_exactly_one_one_time_prekey(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    store
        .publish_keys(key_material(
            account_id,
            device_id,
            vec![(1, vec![0xa1; 32]), (2, vec![0xa2; 32])],
        ))
        .await
        .unwrap();
    assert_eq!(
        store
            .one_time_prekey_count(account_id, device_id)
            .await
            .unwrap(),
        2
    );

    let first = store
        .take_key_bundle(account_id, device_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first.one_time_prekey.as_ref().map(|(key_id, _)| *key_id),
        Some(1)
    );
    assert_eq!(
        store
            .one_time_prekey_count(account_id, device_id)
            .await
            .unwrap(),
        1
    );

    let second = store
        .take_key_bundle(account_id, device_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        second.one_time_prekey.as_ref().map(|(key_id, _)| *key_id),
        Some(2)
    );

    // Exhausted, and the bundle still comes back. Failing here would break the
    // conversation; the caller's job is to nudge the owner to publish more.
    let third = store
        .take_key_bundle(account_id, device_id)
        .await
        .unwrap()
        .unwrap();
    assert!(third.one_time_prekey.is_none());
    assert_eq!(third.signed_prekey_id, 1);
    assert_eq!(third.signed_prekey_expires_at, ts(90_000_000));
}

pub async fn republishing_a_prekey_id_does_not_swap_the_key_behind_it(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let device_id = seed_device(store, account_id, 11).await;
    store
        .publish_keys(key_material(
            account_id,
            device_id,
            vec![(1, vec![0xa1; 32])],
        ))
        .await
        .unwrap();

    let added = store
        .add_one_time_prekeys(
            account_id,
            device_id,
            vec![(1, vec![0xff; 32]), (2, vec![0xa2; 32])],
            ts(3_000),
        )
        .await
        .unwrap();
    assert_eq!(added, 1, "the repeated id is ignored, not overwritten");

    let bundle = store
        .take_key_bundle(account_id, device_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bundle.one_time_prekey, Some((1, vec![0xa1; 32])));
}

pub async fn revoking_keys_stops_bundles_and_drops_unconsumed_prekeys(store: &SharedStore) {
    let account_id = seed_account(store, 1, "alice").await;
    let live = seed_device(store, account_id, 11).await;
    let doomed = seed_device(store, account_id, 12).await;
    for device_id in [live, doomed] {
        store
            .publish_keys(key_material(
                account_id,
                device_id,
                vec![(1, vec![0xa1; 32])],
            ))
            .await
            .unwrap();
    }

    store
        .revoke_device_keys(account_id, doomed, ts(9_000))
        .await
        .unwrap();
    assert!(store
        .take_key_bundle(account_id, doomed)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .one_time_prekey_count(account_id, doomed)
            .await
            .unwrap(),
        0
    );

    let bundles = store
        .take_key_bundles_for_account(account_id)
        .await
        .unwrap();
    assert_eq!(bundles.len(), 1, "fanout skips the revoked device");
    assert_eq!(bundles[0].device_id, live);
}

// --- messages -------------------------------------------------------------

pub async fn sequence_numbers_start_at_one_and_leave_no_gaps(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let conversation = seed_group(store, id(50), vec![alice, bob]).await;

    for n in 1..=5u128 {
        let appended = store
            .append_message(message_row(
                60 + n,
                conversation.conversation_id,
                alice,
                3_000 + n as i64,
            ))
            .await
            .unwrap();
        assert!(appended.is_new());
        assert_eq!(appended.message().seq, n as i64);
    }
    let stored = store
        .conversation(conversation.conversation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.last_seq, 5);
    assert_eq!(stored.last_message_at, Some(ts(3_005)));
}

pub async fn a_repeated_message_id_returns_the_original(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let conversation = seed_group(store, id(50), vec![alice]).await;
    let mut row = message_row(61, conversation.conversation_id, alice, 3_100);

    let first = store.append_message(row.clone()).await.unwrap();
    assert!(first.is_new());

    // The retry a dropped connection produces: same client-generated id, and in
    // practice a different payload is not even required for the retry to be a
    // retry.
    row.envelope = vec![0xff];
    let retry = store.append_message(row).await.unwrap();
    assert!(!retry.is_new(), "a retry must not append a second copy");
    assert_eq!(retry.message().seq, 1);
    assert_eq!(retry.message().envelope, vec![0xde, 0xad, 0xbe, 0xef]);

    let stored = store
        .conversation(conversation.conversation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.last_seq, 1,
        "a duplicate must not burn a sequence number"
    );
}

pub async fn appending_to_nothing_is_an_error_not_a_new_conversation(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    expect_code(
        store
            .append_message(message_row(61, id(999), alice, 3_000))
            .await,
        codes::NOT_FOUND,
    );
    expect_code(
        store
            .advance_cursor(id(999), alice, Some(1), None, None, ts(3_000))
            .await,
        codes::NOT_FOUND,
    );
}

pub async fn the_direct_conversation_survives_a_race(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;

    let first = store
        .direct_conversation(alice, bob, id(50), EncryptionMode::EndToEnd, ts(3_000))
        .await
        .unwrap();
    // The loser of the race brings its own id and must still get the winner's
    // row, from either side of the pair.
    let second = store
        .direct_conversation(bob, alice, id(51), EncryptionMode::EndToEnd, ts(3_001))
        .await
        .unwrap();
    assert_eq!(second.conversation_id, first.conversation_id);
    assert!(store.conversation(id(51)).await.unwrap().is_none());

    let members = store.members(first.conversation_id).await.unwrap();
    assert_eq!(members.len(), 2);
    expect_code(
        store
            .direct_conversation(alice, alice, id(52), EncryptionMode::EndToEnd, ts(3_002))
            .await,
        codes::VALIDATION_FAILED,
    );
}

pub async fn history_reads_backwards_and_forwards_over_the_same_window(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let conversation = seed_group(store, id(50), vec![alice]).await;
    for n in 1..=5u128 {
        store
            .append_message(message_row(
                60 + n,
                conversation.conversation_id,
                alice,
                3_000 + n as i64,
            ))
            .await
            .unwrap();
    }
    let seqs = |page: Vec<StoredMessage>| -> Vec<i64> { page.iter().map(|m| m.seq).collect() };

    // Scrolling up: newest first.
    let newest = store
        .history_before(conversation.conversation_id, None, 2)
        .await
        .unwrap();
    assert_eq!(seqs(newest), vec![5, 4]);
    let older = store
        .history_before(conversation.conversation_id, Some(3), 10)
        .await
        .unwrap();
    assert_eq!(seqs(older), vec![2, 1], "the bound is exclusive");

    // Catch-up sync: oldest first, from a sequence the client already has.
    let after = store
        .history_after(conversation.conversation_id, 0, 2)
        .await
        .unwrap();
    assert_eq!(seqs(after), vec![1, 2]);
    let tail = store
        .history_after(conversation.conversation_id, 3, 100)
        .await
        .unwrap();
    assert_eq!(seqs(tail), vec![4, 5]);
    assert!(store
        .history_after(conversation.conversation_id, 5, 10)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .history_after(id(999), 0, 10)
        .await
        .unwrap()
        .is_empty());
}

pub async fn history_clamps_an_abusive_limit(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let conversation = seed_group(store, id(50), vec![alice]).await;
    for n in 1..=(MAX_PAGE as u128 + 50) {
        store
            .append_message(message_row(
                1_000 + n,
                conversation.conversation_id,
                alice,
                3_000,
            ))
            .await
            .unwrap();
    }
    let page = store
        .history_before(conversation.conversation_id, None, u16::MAX)
        .await
        .unwrap();
    assert_eq!(page.len(), MAX_PAGE as usize);
    let forward = store
        .history_after(conversation.conversation_id, 0, u16::MAX)
        .await
        .unwrap();
    assert_eq!(forward.len(), MAX_PAGE as usize);
    // Zero is not "no rows": a caller that forgets the limit gets one row, not
    // an empty page it will read as the end of history.
    assert_eq!(
        store
            .history_after(conversation.conversation_id, 0, 0)
            .await
            .unwrap()
            .len(),
        1
    );
}

pub async fn a_cursor_only_moves_forward_and_never_past_the_end(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let conversation = seed_group(store, id(50), vec![alice, bob]).await;
    for n in 1..=5u128 {
        store
            .append_message(message_row(
                60 + n,
                conversation.conversation_id,
                bob,
                3_000,
            ))
            .await
            .unwrap();
    }
    let conversation_id = conversation.conversation_id;

    assert_eq!(
        store.cursor(conversation_id, alice).await.unwrap().read_seq,
        0
    );

    let forward = store
        .advance_cursor(conversation_id, alice, Some(3), None, Some(3), ts(4_000))
        .await
        .unwrap();
    assert_eq!(forward.delivered_seq, 3);
    assert_eq!(forward.notified_seq, 3);

    let backwards = store
        .advance_cursor(conversation_id, alice, Some(1), None, Some(1), ts(4_100))
        .await
        .unwrap();
    assert_eq!(
        backwards.delivered_seq, 3,
        "a confused client cannot reset read state"
    );
    assert_eq!(backwards.notified_seq, 3);

    // Reading implies delivery, and neither may exceed what exists.
    let read = store
        .advance_cursor(conversation_id, alice, None, Some(9_000), None, ts(4_200))
        .await
        .unwrap();
    assert_eq!(read.read_seq, 5);
    assert_eq!(read.delivered_seq, 5);
    assert_eq!(
        store.cursor(conversation_id, alice).await.unwrap().read_seq,
        5
    );
}

pub async fn deleting_a_message_takes_the_payload_with_it(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let conversation = seed_group(store, id(50), vec![alice]).await;
    let conversation_id = conversation.conversation_id;
    store
        .append_message(message_row(61, conversation_id, alice, 3_000))
        .await
        .unwrap();

    let edited = store
        .edit_message(conversation_id, id(61), vec![1, 1, 1], ts(3_100))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(edited.envelope, vec![1, 1, 1]);
    assert_eq!(edited.edited_at, Some(ts(3_100)));

    let deleted = store
        .delete_message(conversation_id, id(61), alice, ts(3_200))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted.deleted_at, Some(ts(3_200)));
    assert_eq!(deleted.deleted_by, Some(alice));
    assert!(
        deleted.envelope.is_empty(),
        "a deletion that keeps the ciphertext deleted nothing"
    );

    // The tombstone stays visible so every client converges on the deletion.
    let still_there = store
        .message(conversation_id, id(61))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still_there.seq, 1);
    expect_code(
        store
            .edit_message(conversation_id, id(61), vec![2, 2], ts(3_300))
            .await,
        codes::CONFLICT,
    );
    assert!(store
        .message(conversation_id, id(999))
        .await
        .unwrap()
        .is_none());
}

pub async fn leaving_a_conversation_keeps_the_membership_row(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let conversation = seed_group(store, id(50), vec![alice, bob]).await;
    let conversation_id = conversation.conversation_id;

    store
        .remove_member(conversation_id, bob, ts(4_000))
        .await
        .unwrap();
    // Idempotent, and the second call must not restamp the departure.
    store
        .remove_member(conversation_id, bob, ts(4_100))
        .await
        .unwrap();

    assert!(!store.is_member(conversation_id, bob).await.unwrap());
    let rows = store.members(conversation_id).await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "history access has to stay answerable after the fact"
    );
    let bob_row = rows.iter().find(|m| m.account_id == bob).unwrap();
    assert_eq!(bob_row.left_at, Some(ts(4_000)));

    // Rejoining clears the departure without resetting "member since".
    store
        .add_member(ConversationMember {
            conversation_id,
            account_id: bob,
            role: 0,
            joined_at: ts(5_000),
            left_at: None,
            muted_until: None,
            pinned: false,
        })
        .await
        .unwrap();
    let rejoined = store.members(conversation_id).await.unwrap();
    assert_eq!(rejoined.len(), 2);
    let bob_row = rejoined.iter().find(|m| m.account_id == bob).unwrap();
    assert_eq!(bob_row.joined_at, ts(3_000));
    assert_eq!(bob_row.left_at, None);

    expect_code(
        store
            .add_member(ConversationMember {
                conversation_id: id(999),
                account_id: bob,
                role: 0,
                joined_at: ts(5_000),
                left_at: None,
                muted_until: None,
                pinned: false,
            })
            .await,
        codes::NOT_FOUND,
    );
}

pub async fn the_conversation_list_is_ordered_by_activity_and_counts_unread(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let carol = seed_account(store, 3, "carol").await;
    let quiet = seed_group(store, id(50), vec![alice, bob]).await;
    let busy = seed_group(store, id(51), vec![alice, bob, carol]).await;
    let theirs = seed_group(store, id(52), vec![bob, carol]).await;
    // Same activity as `quiet` and the same creation time, so the id is the only
    // thing separating them: that is the tie-break paging depends on.
    let twin = seed_group(store, id(53), vec![alice, bob]).await;
    // Never used, so it has no activity at all and must sort last rather than
    // first — which is what a plain `order by ... desc` would do to it.
    let empty = seed_group(store, id(54), vec![alice, bob]).await;

    store
        .append_message(message_row(61, quiet.conversation_id, bob, 3_100))
        .await
        .unwrap();
    for n in 1..=3u128 {
        store
            .append_message(message_row(
                70 + n,
                busy.conversation_id,
                bob,
                3_200 + n as i64,
            ))
            .await
            .unwrap();
    }
    store
        .append_message(message_row(80, theirs.conversation_id, carol, 3_900))
        .await
        .unwrap();
    store
        .append_message(message_row(64, twin.conversation_id, bob, 3_100))
        .await
        .unwrap();
    store
        .advance_cursor(busy.conversation_id, alice, None, Some(1), None, ts(4_000))
        .await
        .unwrap();

    let list = store.conversation_list(alice, 10, 2, None).await.unwrap();
    let ids: Vec<Id> = list
        .iter()
        .map(|s| s.conversation.conversation_id)
        .collect();
    assert_eq!(
        ids,
        vec![
            busy.conversation_id,
            quiet.conversation_id,
            twin.conversation_id,
            empty.conversation_id
        ],
        "activity descending with the unused conversation last, and not a member of theirs"
    );
    assert_eq!(
        list[0].unread, 2,
        "last_seq minus read_seq, never a stored counter"
    );
    assert_eq!(list[1].unread, 1);
    assert_eq!(list[0].last_message.as_ref().unwrap().seq, 3);
    assert_eq!(
        list[0].members.len(),
        2,
        "the member preview is capped by the caller"
    );
    assert_eq!(
        list[0].member.account_id, alice,
        "the summary carries the caller's own membership, not somebody else's"
    );
    assert!(
        !list[0].member.pinned && list[0].member.muted_until.is_none(),
        "mute and pin are answered from the row rather than left to a second read"
    );

    // The same list walked two rows at a time has to arrive at the same order.
    // Paging is where an ordering that is only nearly total shows up, because a
    // row that sorts inconsistently is either handed out twice or never.
    let mut paged: Vec<Id> = Vec::new();
    let mut after: Option<ConversationPosition> = None;
    for _ in 0..4 {
        let page = store.conversation_list(alice, 2, 2, after).await.unwrap();
        if page.is_empty() {
            break;
        }
        after = Some(page[page.len() - 1].position());
        paged.extend(page.iter().map(|s| s.conversation.conversation_id));
    }
    assert_eq!(paged, ids, "keyset paging reproduces the single-page order");
    assert!(
        store
            .conversation_list(alice, 2, 2, after)
            .await
            .unwrap()
            .is_empty(),
        "the position of the last row is past the end, not on it"
    );

    let unread = store.conversations_with_unread(alice).await.unwrap();
    assert_eq!(
        unread,
        vec![
            (quiet.conversation_id, 1, 0),
            (busy.conversation_id, 3, 1),
            (twin.conversation_id, 1, 0)
        ],
        "ordered by id so a reconnect sees a stable list"
    );

    store
        .advance_cursor(quiet.conversation_id, alice, None, Some(1), None, ts(4_100))
        .await
        .unwrap();
    store
        .advance_cursor(busy.conversation_id, alice, None, Some(3), None, ts(4_100))
        .await
        .unwrap();
    store
        .advance_cursor(twin.conversation_id, alice, None, Some(1), None, ts(4_100))
        .await
        .unwrap();
    assert!(store
        .conversations_with_unread(alice)
        .await
        .unwrap()
        .is_empty());
}

pub async fn purging_expired_messages_respects_its_budget_and_never_reuses_a_sequence(
    store: &SharedStore,
) {
    let alice = seed_account(store, 1, "alice").await;
    let conversation = seed_group(store, id(50), vec![alice]).await;
    let conversation_id = conversation.conversation_id;
    for n in 1..=2u128 {
        let mut row = message_row(60 + n, conversation_id, alice, 3_000);
        row.expires_at = Some(ts(5_000));
        store.append_message(row).await.unwrap();
    }
    store
        .append_message(message_row(63, conversation_id, alice, 3_000))
        .await
        .unwrap();

    let first = store.purge_expired_messages(ts(6_000), 1).await.unwrap();
    assert_eq!(first, 1, "the budget is a budget, not a suggestion");
    assert!(store
        .message(conversation_id, id(61))
        .await
        .unwrap()
        .is_none());
    assert!(store
        .message(conversation_id, id(62))
        .await
        .unwrap()
        .is_some());

    let second = store.purge_expired_messages(ts(6_000), 200).await.unwrap();
    assert_eq!(second, 1);
    assert_eq!(
        store.purge_expired_messages(ts(6_000), 200).await.unwrap(),
        0
    );
    assert!(
        store
            .message(conversation_id, id(63))
            .await
            .unwrap()
            .is_some(),
        "a message with no expiry is not a message that expired"
    );

    let appended = store
        .append_message(message_row(64, conversation_id, alice, 7_000))
        .await
        .unwrap();
    assert_eq!(
        appended.message().seq,
        4,
        "the counter lives on the conversation, so a purge cannot make two messages share a seq"
    );
}

// --- rooms ----------------------------------------------------------------

pub async fn creating_a_room_creates_its_conversation_and_its_owner(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let room = store
        .create_room(room_row(id(100), id(200), "migo-lounge", alice, 50))
        .await
        .unwrap();

    assert_eq!(room.member_count, 1, "the owner counts");
    assert_eq!(room.join_policy, join_policy::OPEN);
    assert_eq!(room.slow_mode_seconds, 0);
    assert_eq!(room.updated_at, room.created_at);

    let conversation = store.conversation(id(200)).await.unwrap().unwrap();
    assert_eq!(conversation.kind, ConversationKind::Room);
    assert_eq!(conversation.room_id, Some(id(100)));
    assert!(
        store.is_member(id(200), alice).await.unwrap(),
        "an owner outside their own room's conversation cannot read it"
    );
    let owner = store.room_member(id(100), alice).await.unwrap().unwrap();
    assert_eq!(owner.role, RoomRole::Owner);

    // Nothing about a half-created room is recoverable, so every uniqueness
    // failure has to happen before any of the three rows exist.
    expect_code(
        store
            .create_room(room_row(id(101), id(201), "MIGO-LOUNGE", alice, 50))
            .await,
        codes::ALREADY_EXISTS,
    );
    expect_code(
        store
            .create_room(room_row(id(100), id(202), "other", alice, 50))
            .await,
        codes::ALREADY_EXISTS,
    );
    expect_code(
        store
            .create_room(room_row(id(102), id(200), "other", alice, 50))
            .await,
        codes::ALREADY_EXISTS,
    );
    expect_code(
        store
            .create_room(room_row(id(103), id(203), "empty", alice, 0))
            .await,
        codes::VALIDATION_FAILED,
    );
    assert!(store.room(id(101)).await.unwrap().is_none());
    assert!(store.room_by_slug("Migo-Lounge").await.unwrap().is_some());
    assert!(store.room_by_slug("nothing-here").await.unwrap().is_none());
}

pub async fn a_full_room_refuses_a_newcomer_but_not_a_member(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let carol = seed_account(store, 3, "carol").await;
    store
        .create_room(room_row(id(100), id(200), "duo", alice, 2))
        .await
        .unwrap();

    store
        .join_room(member_row(id(100), bob, 4_100))
        .await
        .unwrap();
    assert_eq!(store.room(id(100)).await.unwrap().unwrap().member_count, 2);
    expect_code(
        store.join_room(member_row(id(100), carol, 4_200)).await,
        codes::CONFLICT,
    );

    // A member who is already in the room is not consuming a second seat.
    store
        .join_room(member_row(id(100), bob, 4_300))
        .await
        .unwrap();
    assert_eq!(store.room(id(100)).await.unwrap().unwrap().member_count, 2);

    store.leave_room(id(100), bob, ts(4_400)).await.unwrap();
    assert_eq!(store.room(id(100)).await.unwrap().unwrap().member_count, 1);
    assert!(!store.is_member(id(200), bob).await.unwrap());
    store.leave_room(id(100), bob, ts(4_500)).await.unwrap();

    store
        .join_room(member_row(id(100), carol, 4_600))
        .await
        .unwrap();
    assert_eq!(store.room(id(100)).await.unwrap().unwrap().member_count, 2);
    expect_code(
        store.join_room(member_row(id(999), carol, 4_700)).await,
        codes::NOT_FOUND,
    );
    // The count is derived, so it can be rebuilt from the rows at any time.
    assert_eq!(store.recount_room(id(100)).await.unwrap(), 2);
    expect_code(store.recount_room(id(999)).await, codes::NOT_FOUND);
}

pub async fn a_ban_is_not_shed_by_leaving_and_coming_back(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    store
        .create_room(room_row(id(100), id(200), "lounge", alice, 50))
        .await
        .unwrap();
    store
        .join_room(member_row(id(100), bob, 4_100))
        .await
        .unwrap();

    store
        .set_room_sanction(
            id(100),
            bob,
            None,
            Some(Timestamp::MAX),
            Some("flooding".to_string()),
            ts(4_200),
        )
        .await
        .unwrap();

    let banned = store.room_member(id(100), bob).await.unwrap().unwrap();
    assert!(banned.is_banned(ts(4_300)));
    assert!(
        banned.is_banned(Timestamp::from_millis(i64::MAX - 1)),
        "permanent means permanent"
    );
    assert_eq!(
        banned.left_at,
        Some(ts(4_200)),
        "a ban removes them from the room"
    );
    assert_eq!(banned.ban_reason.as_deref(), Some("flooding"));
    assert!(
        !store.is_member(id(200), bob).await.unwrap(),
        "a ban that leaves the conversation membership keeps delivering the messages"
    );
    assert_eq!(store.room(id(100)).await.unwrap().unwrap().member_count, 1);

    // The row survives the departure precisely so the ban can be enforced on the
    // way back in; join_room does not check it, the caller does.
    let rejoined = store
        .join_room(member_row(id(100), bob, 5_000))
        .await
        .unwrap();
    assert!(rejoined.is_banned(ts(5_001)));
    assert_eq!(
        rejoined.joined_at,
        ts(4_100),
        "rejoining is not a new membership"
    );

    // Lifting it is the only way out, and "not banned" is a null rather than a
    // timestamp in the past.
    store
        .set_room_sanction(id(100), bob, None, None, None, ts(6_000))
        .await
        .unwrap();
    let lifted = store.room_member(id(100), bob).await.unwrap().unwrap();
    assert_eq!(lifted.banned_until, None);
    assert!(!lifted.is_banned(ts(6_001)));

    store
        .set_room_sanction(id(100), bob, Some(ts(7_000)), None, None, ts(6_100))
        .await
        .unwrap();
    let muted = store.room_member(id(100), bob).await.unwrap().unwrap();
    assert!(muted.is_muted(ts(6_500)));
    assert!(!muted.is_muted(ts(7_000)), "the expiry is exclusive");
    assert!(muted.is_active(), "a mute is not a removal");

    expect_code(
        store
            .set_room_sanction(id(100), id(9), None, None, None, ts(6_200))
            .await,
        codes::NOT_FOUND,
    );
    expect_code(
        store
            .set_room_sanction(id(999), bob, None, None, None, ts(6_200))
            .await,
        codes::NOT_FOUND,
    );
}

pub async fn ownership_moves_in_one_step_or_not_at_all(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let carol = seed_account(store, 3, "carol").await;
    store
        .create_room(room_row(id(100), id(200), "lounge", alice, 50))
        .await
        .unwrap();
    store
        .join_room(member_row(id(100), bob, 4_100))
        .await
        .unwrap();

    // Every refusal happens before any write, which is the only reason this can be
    // one method instead of three calls a caller has to sequence correctly.
    expect_code(
        store
            .transfer_room_ownership(id(100), bob, alice, ts(4_200))
            .await,
        codes::CONFLICT,
    );
    expect_code(
        store
            .transfer_room_ownership(id(100), alice, carol, ts(4_200))
            .await,
        codes::NOT_FOUND,
    );
    expect_code(
        store
            .transfer_room_ownership(id(999), alice, bob, ts(4_200))
            .await,
        codes::NOT_FOUND,
    );
    let untouched = store.room(id(100)).await.unwrap().unwrap();
    assert_eq!(untouched.owner_id, alice, "a refusal writes nothing");
    assert_eq!(untouched.updated_at, untouched.created_at);

    store
        .transfer_room_ownership(id(100), alice, bob, ts(4_300))
        .await
        .unwrap();

    let room = store.room(id(100)).await.unwrap().unwrap();
    assert_eq!(room.owner_id, bob);
    assert_eq!(room.updated_at, ts(4_300));
    assert_eq!(
        store.room_member(id(100), bob).await.unwrap().unwrap().role,
        RoomRole::Owner
    );
    assert_eq!(
        store
            .room_member(id(100), alice)
            .await
            .unwrap()
            .unwrap()
            .role,
        RoomRole::Manager,
        "the outgoing owner is demoted, not removed"
    );

    // Idempotent for the current owner, so a retried transfer is not a conflict.
    store
        .transfer_room_ownership(id(100), bob, bob, ts(4_400))
        .await
        .unwrap();
    assert_eq!(
        store.room(id(100)).await.unwrap().unwrap().updated_at,
        ts(4_300),
        "a no-op transfer does not stamp the room"
    );
}

pub async fn the_roster_pages_by_role_then_seniority(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let carol = seed_account(store, 3, "carol").await;
    let dave = seed_account(store, 4, "dave").await;
    store
        .create_room(room_row(id(100), id(200), "lounge", alice, 50))
        .await
        .unwrap();
    store
        .join_room(member_row(id(100), carol, 4_100))
        .await
        .unwrap();
    store
        .join_room(member_row(id(100), bob, 4_200))
        .await
        .unwrap();
    store
        .join_room(member_row(id(100), dave, 4_300))
        .await
        .unwrap();
    store
        .set_room_role(id(100), dave, RoomRole::Moderator, ts(4_400))
        .await
        .unwrap();

    let first = store.room_members(id(100), 2, None).await.unwrap();
    let names: Vec<Id> = first.iter().map(|m| m.account_id).collect();
    assert_eq!(names, vec![alice, dave], "owner, then moderator");

    let second = store.room_members(id(100), 2, Some(dave)).await.unwrap();
    let names: Vec<Id> = second.iter().map(|m| m.account_id).collect();
    assert_eq!(names, vec![carol, bob], "then by join time, not by id");

    assert!(store
        .room_members(id(100), 2, Some(bob))
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .room_members(id(999), 10, None)
        .await
        .unwrap()
        .is_empty());

    store.leave_room(id(100), carol, ts(4_500)).await.unwrap();
    let active = store.room_members(id(100), 100, None).await.unwrap();
    assert_eq!(active.len(), 3, "the roster is who is here now");
    assert!(
        store.room_member(id(100), carol).await.unwrap().is_some(),
        "the row stays"
    );

    expect_code(
        store
            .set_room_role(id(100), id(9), RoomRole::Helper, ts(4_600))
            .await,
        codes::NOT_FOUND,
    );
    store
        .set_room_permissions(id(100), bob, 0b110, 0b010, ts(4_700))
        .await
        .unwrap();
    let bob_row = store.room_member(id(100), bob).await.unwrap().unwrap();
    assert_eq!(bob_row.permissions_grant, 0b110);
    assert_eq!(bob_row.permissions_deny, 0b010);
    expect_code(
        store
            .set_room_permissions(id(100), id(9), 1, 0, ts(4_800))
            .await,
        codes::NOT_FOUND,
    );
}

pub async fn archiving_a_room_closes_it_without_deleting_it(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    store
        .create_room(room_row(id(100), id(200), "lounge", alice, 50))
        .await
        .unwrap();
    let mut second = room_row(id(101), id(201), "quiet", alice, 50);
    second.kind = migo_protocol::RoomKind::Managed;
    store.create_room(second).await.unwrap();
    store
        .join_room(member_row(id(100), bob, 4_100))
        .await
        .unwrap();

    let browse = store.browse_rooms(None, 10).await.unwrap();
    let ids: Vec<Id> = browse.iter().map(|r| r.room_id).collect();
    assert_eq!(ids, vec![id(100), id(101)], "busiest first");
    let public = store
        .browse_rooms(Some(RoomKindFilter::Public), 10)
        .await
        .unwrap();
    assert_eq!(public.len(), 1);
    let managed = store
        .browse_rooms(Some(RoomKindFilter::Managed), 10)
        .await
        .unwrap();
    assert_eq!(managed[0].room_id, id(101));
    assert_eq!(store.rooms_for_account(bob).await.unwrap().len(), 1);

    store.archive_room(id(100), ts(9_000)).await.unwrap();
    store.archive_room(id(100), ts(9_100)).await.unwrap();

    let room = store.room(id(100)).await.unwrap().unwrap();
    assert_eq!(
        room.archived_at,
        Some(ts(9_000)),
        "archiving twice is not a restamp"
    );
    assert!(
        store.room_by_slug("lounge").await.unwrap().is_some(),
        "old links still resolve"
    );
    assert_eq!(
        store
            .conversation(id(200))
            .await
            .unwrap()
            .unwrap()
            .archived_at,
        Some(ts(9_000))
    );
    let browse = store.browse_rooms(None, 10).await.unwrap();
    assert_eq!(browse.len(), 1, "an archived room is not on the shelf");
    expect_code(
        store.join_room(member_row(id(100), id(3), 9_200)).await,
        codes::CONFLICT,
    );
    store.archive_room(id(999), ts(9_300)).await.unwrap();
}

pub async fn updating_a_room_can_clear_a_topic_without_clearing_a_name(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    store
        .create_room(room_row(id(100), id(200), "lounge", alice, 50))
        .await
        .unwrap();

    let named = store
        .update_room(
            id(100),
            Some("Migo Lounge".to_string()),
            Patch::Set("chat".to_string()),
            Some(5),
            Some(join_policy::APPROVAL),
            ts(5_000),
        )
        .await
        .unwrap();
    assert_eq!(named.name, "Migo Lounge");
    assert_eq!(named.topic.as_deref(), Some("chat"));
    assert_eq!(named.slow_mode_seconds, 5);
    assert_eq!(named.join_policy, join_policy::APPROVAL);
    assert_eq!(named.updated_at, ts(5_000));

    let kept = store
        .update_room(id(100), None, Patch::Keep, None, None, ts(5_100))
        .await
        .unwrap();
    assert_eq!(
        kept.name, "Migo Lounge",
        "no change is not a change to empty"
    );
    assert_eq!(kept.topic.as_deref(), Some("chat"));
    assert_eq!(kept.slow_mode_seconds, 5);

    let cleared = store
        .update_room(id(100), None, Patch::Clear, Some(0), None, ts(5_200))
        .await
        .unwrap();
    assert_eq!(cleared.topic, None);
    assert_eq!(cleared.slow_mode_seconds, 0);

    expect_code(
        store
            .update_room(id(100), None, Patch::Keep, Some(-1), None, ts(5_300))
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        store
            .update_room(id(999), None, Patch::Keep, None, None, ts(5_400))
            .await,
        codes::NOT_FOUND,
    );
}

// --- social graph ---------------------------------------------------------

fn edge(account_id: Id, other_id: Id, kind: RelationshipKind, created_at: i64) -> Relationship {
    Relationship {
        account_id,
        other_id,
        kind,
        created_at: ts(created_at),
        accepted_at: None,
    }
}

pub async fn a_block_stops_contact_in_both_directions(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let carol = seed_account(store, 3, "carol").await;

    store
        .put_relationship(edge(alice, bob, RelationshipKind::Block, 5_000))
        .await
        .unwrap();

    // Asking one way round is how "blocked users can still reply in threads"
    // bugs get shipped, so the question only exists in the symmetric form.
    assert!(store.is_blocked_either_way(alice, bob).await.unwrap());
    assert!(store.is_blocked_either_way(bob, alice).await.unwrap());
    assert!(!store.is_blocked_either_way(alice, carol).await.unwrap());

    store
        .remove_relationship(alice, bob, RelationshipKind::Block)
        .await
        .unwrap();
    assert!(!store.is_blocked_either_way(bob, alice).await.unwrap());
    // Removing something that is not there is the same outcome the caller wanted.
    store
        .remove_relationship(alice, bob, RelationshipKind::Block)
        .await
        .unwrap();

    expect_code(
        store
            .put_relationship(edge(alice, alice, RelationshipKind::Block, 5_100))
            .await,
        codes::VALIDATION_FAILED,
    );
}

pub async fn accepting_a_friend_request_writes_both_sides(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;

    store
        .put_relationship(edge(bob, alice, RelationshipKind::PendingOutgoing, 5_000))
        .await
        .unwrap();
    store
        .put_relationship(edge(alice, bob, RelationshipKind::PendingIncoming, 5_000))
        .await
        .unwrap();

    // Re-sending a request must not make an old one look new; a request-spam
    // detector reads created_at.
    let resent = store
        .put_relationship(edge(bob, alice, RelationshipKind::PendingOutgoing, 5_900))
        .await
        .unwrap();
    assert_eq!(resent.created_at, ts(5_000));

    let pending = store
        .inbound_relationships(alice, RelationshipKind::PendingOutgoing, 10)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].account_id, bob);

    store.accept_friend(alice, bob, ts(6_000)).await.unwrap();

    for (owner, peer) in [(alice, bob), (bob, alice)] {
        let friend = store
            .relationship(owner, peer, RelationshipKind::Friend)
            .await
            .unwrap()
            .expect("a friendship stored on one side only is not a friendship");
        assert_eq!(
            friend.created_at,
            ts(5_000),
            "friends since the request, not the accept"
        );
        assert_eq!(friend.accepted_at, Some(ts(6_000)));
    }
    assert!(store
        .relationship(alice, bob, RelationshipKind::PendingIncoming)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .relationship(bob, alice, RelationshipKind::PendingOutgoing)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .relationships(alice, RelationshipKind::Friend, 10)
            .await
            .unwrap()
            .len(),
        1
    );

    // Accepting a request nobody made would let one account add itself to
    // another's friend list.
    expect_code(
        store.accept_friend(alice, id(9), ts(6_100)).await,
        codes::NOT_FOUND,
    );
    expect_code(
        store.accept_friend(alice, bob, ts(6_200)).await,
        codes::NOT_FOUND,
    );
}

pub async fn relationship_pages_are_newest_first_and_clamped(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    for n in 2..=(MAX_PAGE as u128 + 10) {
        let other = seed_account(store, n, &format!("user{n}")).await;
        store
            .put_relationship(edge(
                alice,
                other,
                RelationshipKind::Follow,
                5_000 + n as i64,
            ))
            .await
            .unwrap();
        store
            .put_relationship(edge(
                other,
                alice,
                RelationshipKind::Follow,
                5_000 + n as i64,
            ))
            .await
            .unwrap();
    }
    let follows = store
        .relationships(alice, RelationshipKind::Follow, u16::MAX)
        .await
        .unwrap();
    assert_eq!(follows.len(), MAX_PAGE as usize);
    assert!(
        follows[0].created_at > follows[1].created_at,
        "newest first"
    );

    let followers = store
        .inbound_relationships(alice, RelationshipKind::Follow, u16::MAX)
        .await
        .unwrap();
    assert_eq!(followers.len(), MAX_PAGE as usize);
    assert!(store
        .relationships(alice, RelationshipKind::Block, 10)
        .await
        .unwrap()
        .is_empty());
}

// --- ledger ---------------------------------------------------------------

/// Opens the three accounts every economy test needs: a mint, and a wallet each
/// for two users.
async fn seed_ledger(store: &SharedStore) -> (Id, Id, Id) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let mint = store
        .ledger_account(
            None,
            LedgerAccountKind::Mint,
            Currency::Coins,
            id(300),
            ts(6_000),
        )
        .await
        .unwrap();
    let alice_wallet = store
        .ledger_account(
            Some(alice),
            LedgerAccountKind::User,
            Currency::Coins,
            id(301),
            ts(6_000),
        )
        .await
        .unwrap();
    let bob_wallet = store
        .ledger_account(
            Some(bob),
            LedgerAccountKind::User,
            Currency::Coins,
            id(302),
            ts(6_000),
        )
        .await
        .unwrap();
    (
        mint.ledger_account_id,
        alice_wallet.ledger_account_id,
        bob_wallet.ledger_account_id,
    )
}

fn leg(ledger_account_id: Id, amount: i64) -> LedgerLeg {
    LedgerLeg {
        ledger_account_id,
        amount,
    }
}

pub async fn a_wallet_is_opened_once_however_often_it_is_asked_for(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let first = store
        .ledger_account(
            Some(alice),
            LedgerAccountKind::User,
            Currency::Coins,
            id(301),
            ts(6_000),
        )
        .await
        .unwrap();
    // Two requests racing to open the same wallet each bring their own id; only
    // one wallet may exist, or half the balance would be invisible.
    let second = store
        .ledger_account(
            Some(alice),
            LedgerAccountKind::User,
            Currency::Coins,
            id(999),
            ts(6_100),
        )
        .await
        .unwrap();
    assert_eq!(second.ledger_account_id, first.ledger_account_id);
    assert_eq!(second.created_at, ts(6_000));

    // Currency and purpose are part of the identity: the same user has a separate
    // wallet per currency.
    let gems = store
        .ledger_account(
            Some(alice),
            LedgerAccountKind::User,
            Currency::Gems,
            id(302),
            ts(6_200),
        )
        .await
        .unwrap();
    assert_ne!(gems.ledger_account_id, first.ledger_account_id);
    assert_eq!(store.balance(first.ledger_account_id).await.unwrap(), 0);
    expect_code(store.balance(id(998)).await, codes::NOT_FOUND);
}

pub async fn a_transfer_moves_value_without_creating_any(store: &SharedStore) {
    let (mint, alice_wallet, bob_wallet) = seed_ledger(store).await;

    let minted = store
        .post_transaction(transaction_row(
            400,
            "mint-1",
            Currency::Coins,
            vec![leg(mint, -100), leg(alice_wallet, 100)],
        ))
        .await
        .unwrap();
    assert!(minted.is_new());
    assert_eq!(store.balance(alice_wallet).await.unwrap(), 100);
    assert_eq!(
        store.balance(mint).await.unwrap(),
        -100,
        "the mint goes negative by design, and that negative is the total ever issued"
    );

    store
        .post_transaction(transaction_row(
            401,
            "gift-1",
            Currency::Coins,
            vec![leg(alice_wallet, -30), leg(bob_wallet, 30)],
        ))
        .await
        .unwrap();
    assert_eq!(store.balance(alice_wallet).await.unwrap(), 70);
    assert_eq!(store.balance(bob_wallet).await.unwrap(), 30);
    assert_eq!(
        store.currency_sum(Currency::Coins).await.unwrap(),
        0,
        "an invariant that nothing checks is a wish"
    );
    assert_eq!(store.currency_sum(Currency::Gems).await.unwrap(), 0);

    let statement = store.ledger_history(alice_wallet, 10).await.unwrap();
    assert_eq!(statement.len(), 2);
    assert_eq!(
        statement[0].1, -30,
        "newest first, signed from this account's side"
    );
    assert_eq!(statement[0].0.tx_id, id(401));
    assert_eq!(statement[1].1, 100);
    assert!(store.ledger_history(id(998), 10).await.unwrap().is_empty());
}

pub async fn a_retried_payment_charges_once(store: &SharedStore) {
    let (mint, alice_wallet, _) = seed_ledger(store).await;
    let legs = vec![leg(mint, -100), leg(alice_wallet, 100)];

    let first = store
        .post_transaction(transaction_row(
            400,
            "purchase-1",
            Currency::Coins,
            legs.clone(),
        ))
        .await
        .unwrap();
    // The retry after a timeout, with a fresh transaction id because the client
    // does not know the first attempt landed.
    let retry = store
        .post_transaction(transaction_row(
            401,
            "purchase-1",
            Currency::Coins,
            legs.clone(),
        ))
        .await
        .unwrap();
    assert!(!retry.is_new());
    assert_eq!(retry.transaction().tx_id, first.transaction().tx_id);
    assert_eq!(
        store.balance(alice_wallet).await.unwrap(),
        100,
        "charged once, not twice"
    );
    assert_eq!(
        store.ledger_history(alice_wallet, 10).await.unwrap().len(),
        1,
        "the retry left no second entry to reconcile against"
    );

    // A reused transaction id under a different key is a caller bug, not a
    // retry, and must not silently merge into the earlier row.
    expect_code(
        store
            .post_transaction(transaction_row(400, "purchase-2", Currency::Coins, legs))
            .await,
        codes::ALREADY_EXISTS,
    );
}

pub async fn the_ledger_refuses_anything_that_does_not_balance(store: &SharedStore) {
    let (mint, alice_wallet, bob_wallet) = seed_ledger(store).await;
    let alice = id(1);
    let gems = store
        .ledger_account(
            Some(alice),
            LedgerAccountKind::User,
            Currency::Gems,
            id(303),
            ts(6_000),
        )
        .await
        .unwrap()
        .ledger_account_id;

    let cases: Vec<(&str, Vec<LedgerLeg>)> = vec![
        (
            "value created from nothing",
            vec![leg(mint, -100), leg(alice_wallet, 200)],
        ),
        ("one leg is not a transfer", vec![leg(alice_wallet, 100)]),
        (
            "a zero leg carries no meaning",
            vec![leg(alice_wallet, 0), leg(bob_wallet, 0)],
        ),
        (
            "overflow is not a balance",
            vec![leg(alice_wallet, i64::MAX), leg(bob_wallet, i64::MAX)],
        ),
    ];
    for (index, (why, legs)) in cases.into_iter().enumerate() {
        let key = format!("bad-{index}");
        let result = store
            .post_transaction(transaction_row(
                500 + index as u128,
                &key,
                Currency::Coins,
                legs,
            ))
            .await;
        match result {
            Ok(_) => panic!("the ledger accepted: {why}"),
            Err(error) => assert_eq!(error.code(), codes::VALIDATION_FAILED, "{why}"),
        }
    }

    // One currency per transaction: coins and gems are not exchangeable by
    // pretending they are the same unit.
    expect_code(
        store
            .post_transaction(transaction_row(
                510,
                "mixed",
                Currency::Coins,
                vec![leg(alice_wallet, -10), leg(gems, 10)],
            ))
            .await,
        codes::VALIDATION_FAILED,
    );
    // A leg against an account that does not exist would post value nowhere.
    expect_code(
        store
            .post_transaction(transaction_row(
                511,
                "ghost",
                Currency::Coins,
                vec![leg(alice_wallet, -10), leg(id(997), 10)],
            ))
            .await,
        codes::NOT_FOUND,
    );

    // The leg cap exists because Postgres derives each entry's key from the
    // transaction id and the leg index; an unbounded fan-out is a way to make one
    // write hold a lock over an arbitrary number of accounts.
    let mut many = vec![leg(mint, -(MAX_LEDGER_LEGS as i64))];
    for n in 0..MAX_LEDGER_LEGS {
        let owner = seed_account(store, 600 + n as u128, &format!("holder{n}")).await;
        let wallet = store
            .ledger_account(
                Some(owner),
                LedgerAccountKind::User,
                Currency::Coins,
                id(700 + n as u128),
                ts(6_000),
            )
            .await
            .unwrap();
        many.push(leg(wallet.ledger_account_id, 1));
    }
    assert_eq!(many.len(), MAX_LEDGER_LEGS + 1);
    expect_code(
        store
            .post_transaction(transaction_row(520, "wide", Currency::Coins, many))
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(
        store.currency_sum(Currency::Coins).await.unwrap(),
        0,
        "nothing was written"
    );
}

// --- media and safety -----------------------------------------------------

pub async fn a_deleted_media_row_outlives_its_bytes(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let media = store
        .create_media(media_row(id(800), alice, 4_096))
        .await
        .unwrap();
    assert_eq!(media.scan_status, media_scan::PENDING);

    expect_code(
        store.create_media(media_row(id(800), alice, 4_096)).await,
        codes::ALREADY_EXISTS,
    );
    expect_code(
        store.create_media(media_row(id(801), alice, 0)).await,
        codes::VALIDATION_FAILED,
    );

    store
        .set_media_scan_status(id(800), media_scan::CLEAN, ts(7_100))
        .await
        .unwrap();
    assert_eq!(
        store.media(id(800)).await.unwrap().unwrap().scan_status,
        media_scan::CLEAN
    );
    expect_code(
        store
            .set_media_scan_status(id(999), media_scan::CLEAN, ts(7_100))
            .await,
        codes::NOT_FOUND,
    );

    store.delete_media(id(800), ts(7_200)).await.unwrap();
    store.delete_media(id(800), ts(7_300)).await.unwrap();
    let deleted = store.media(id(800)).await.unwrap().unwrap();
    assert_eq!(
        deleted.deleted_at,
        Some(ts(7_200)),
        "the row is the sweeper's work list, so it stays until the bytes are gone"
    );
    assert_eq!(deleted.storage_key, "media/2026/08/object");
    store.delete_media(id(999), ts(7_400)).await.unwrap();
}

pub async fn the_moderation_queue_is_oldest_first_and_resolves_once(store: &SharedStore) {
    let alice = seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    let moderator = seed_account(store, 3, "mod").await;
    store
        .create_report(report_row(id(900), alice, bob, 8_000))
        .await
        .unwrap();
    store
        .create_report(report_row(id(901), bob, alice, 8_100))
        .await
        .unwrap();
    expect_code(
        store
            .create_report(report_row(id(900), alice, bob, 8_200))
            .await,
        codes::ALREADY_EXISTS,
    );

    let queue = store.open_reports(10).await.unwrap();
    let ids: Vec<Id> = queue.iter().map(|r| r.report_id).collect();
    assert_eq!(
        ids,
        vec![id(900), id(901)],
        "whoever waited longest is served first"
    );

    store
        .resolve_report(id(900), report_status::ACTIONED, 2, moderator, ts(8_500))
        .await
        .unwrap();
    let resolved = store.report(id(900)).await.unwrap().unwrap();
    assert_eq!(resolved.status, report_status::ACTIONED);
    assert_eq!(resolved.resolution, Some(2));
    assert_eq!(resolved.resolved_by, Some(moderator));
    assert_eq!(resolved.resolved_at, Some(ts(8_500)));

    let queue = store.open_reports(10).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].report_id, id(901));

    // Two moderators opening the same report is normal; both of them acting on it
    // is not, and the second one has to be told.
    expect_code(
        store
            .resolve_report(id(900), report_status::DISMISSED, 0, moderator, ts(8_600))
            .await,
        codes::CONFLICT,
    );
    expect_code(
        store
            .resolve_report(id(999), report_status::DISMISSED, 0, moderator, ts(8_700))
            .await,
        codes::NOT_FOUND,
    );
    assert!(store.report(id(999)).await.unwrap().is_none());
}

pub async fn the_audit_log_is_newest_first_and_scoped_to_one_target(store: &SharedStore) {
    // The actor exists because an audit entry names one, and a backend with real
    // foreign keys will not accept a reference to an account that was never created.
    seed_account(store, 1, "alice").await;
    let bob = seed_account(store, 2, "bob").await;
    store
        .append_audit(audit_row(1, "room.member.ban", 0, bob))
        .await
        .unwrap();
    store
        .append_audit(audit_row(2, "account.suspend", 0, bob))
        .await
        .unwrap();
    store
        .append_audit(audit_row(3, "room.archive", 2, id(100)))
        .await
        .unwrap();

    let about_bob = store.audit_for_target(0, bob, 10).await.unwrap();
    let actions: Vec<&str> = about_bob.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(
        actions,
        vec!["account.suspend", "room.member.ban"],
        "newest first"
    );
    assert_eq!(about_bob[0].ip_class.as_deref(), Some("203.0.113.0/24"));

    assert_eq!(
        store.audit_for_target(2, id(100), 10).await.unwrap().len(),
        1
    );
    assert!(
        store.audit_for_target(2, bob, 10).await.unwrap().is_empty(),
        "the same id under a different kind is a different thing"
    );
    assert_eq!(store.audit_for_target(0, bob, 1).await.unwrap().len(), 1);
}

/// A game's lock token moves on every write, even inside one millisecond.
///
/// The token is `updated_at`, and the store is handed the time by its caller, so two moves
/// computed from the same state and stamped with the same millisecond would otherwise leave
/// the token identical: the second writer would find its own now-stale expectation satisfied
/// and would overwrite a move it never saw. That is exactly the lost update the compare-and-set
/// exists to prevent, so the token has to end up strictly past the value it replaced.
pub async fn a_game_token_moves_even_within_one_millisecond(store: &SharedStore) {
    let conversation = id(9_001);
    let alice = seed_account(store, 9_002, "gamer_one").await;
    let bob = seed_account(store, 9_003, "gamer_two").await;
    store
        .create_conversation(
            Conversation {
                conversation_id: conversation,
                kind: ConversationKind::Group,
                encryption: EncryptionMode::Transport,
                room_id: None,
                last_seq: 0,
                created_by: alice,
                created_at: ts(1_000),
                last_message_at: None,
                archived_at: None,
            },
            vec![alice, bob],
        )
        .await
        .expect("the conversation seeds");
    let game_id = id(9_004);
    let session = store
        .create_game(NewGame {
            game_id,
            kind: 0,
            conversation_id: conversation,
            state: vec![1, 0, 0],
            turn_of: Some(alice),
            stake_currency: None,
            stake_amount: None,
            at: ts(5_000),
        })
        .await
        .expect("the game is created");
    // Both writers read the same row and so name the same token, and both are stamped with the
    // same millisecond -- the same millisecond the row was created in, at that.
    let token = session.updated_at;
    let first = store
        .advance_game(AdvanceGame {
            game_id,
            expected_updated_at: token,
            state: vec![1, 1, 0],
            turn_of: Some(bob),
            status: game_status::OPEN,
            at: ts(5_000),
        })
        .await
        .expect("the store answers")
        .expect("the first move lands");
    assert!(
        first.updated_at > token,
        "the token must move: {:?} did not pass {token:?}",
        first.updated_at
    );
    let second = store
        .advance_game(AdvanceGame {
            game_id,
            expected_updated_at: token,
            state: vec![1, 0, 2],
            turn_of: Some(alice),
            status: game_status::OPEN,
            at: ts(5_000),
        })
        .await
        .expect("the store answers");
    assert!(
        second.is_none(),
        "the second move named a token that is no longer current and must be refused"
    );
    let stored = store
        .game(game_id)
        .await
        .expect("the store answers")
        .expect("the game exists");
    assert_eq!(
        stored.state,
        vec![1, 1, 0],
        "the move that won the race is the one in the row"
    );
    // And the winner can carry on from the token it was handed back.
    let third = store
        .advance_game(AdvanceGame {
            game_id,
            expected_updated_at: first.updated_at,
            state: vec![1, 1, 2],
            turn_of: Some(alice),
            status: game_status::FINISHED,
            at: ts(5_000),
        })
        .await
        .expect("the store answers")
        .expect("a move against the current token lands");
    assert!(third.updated_at > first.updated_at);
    assert_eq!(
        third.finished_at,
        Some(ts(5_000)),
        "the end time is the real one"
    );
    // A terminal game takes nothing further, token or no token.
    assert!(
        store
            .advance_game(AdvanceGame {
                game_id,
                expected_updated_at: third.updated_at,
                state: vec![1, 1, 1],
                turn_of: None,
                status: game_status::OPEN,
                at: ts(5_000),
            })
            .await
            .expect("the store answers")
            .is_none(),
        "a decided game cannot be reopened"
    );
    assert!(
        store
            .abandon_game(game_id, ts(6_000))
            .await
            .expect("the store answers")
            .is_none(),
        "a decided game cannot be abandoned"
    );
}

/// Names every case in the suite, so a backend file lists none of them.
///
/// A test that exists but is only wired into one backend is worse than no test:
/// it reads as coverage while proving nothing about the other one. Adding a case
/// here adds it to every backend at once, and that is the only way to add one.
#[macro_export]
macro_rules! for_each_contract_case {
    ($case:ident) => {
        $case!(usernames_collide_case_insensitively);
        $case!(email_and_phone_are_unique_too);
        $case!(a_patch_tells_keep_apart_from_clear);
        $case!(search_obeys_privacy_before_relevance);
        $case!(search_clamps_an_abusive_limit);
        $case!(last_seen_never_moves_backwards);
        $case!(revoking_a_device_hides_it_but_keeps_the_row);
        $case!(a_rotated_token_cannot_be_exchanged_twice);
        $case!(a_session_carries_its_own_authentication_time);
        $case!(reuse_detection_kills_the_whole_family);
        $case!(logging_out_other_devices_spares_the_current_one);
        $case!(purging_a_session_forgets_its_refresh_hash);
        $case!(an_already_expired_signed_prekey_is_refused_on_arrival);
        $case!(every_bundle_consumes_exactly_one_one_time_prekey);
        $case!(republishing_a_prekey_id_does_not_swap_the_key_behind_it);
        $case!(revoking_keys_stops_bundles_and_drops_unconsumed_prekeys);
        $case!(sequence_numbers_start_at_one_and_leave_no_gaps);
        $case!(a_repeated_message_id_returns_the_original);
        $case!(appending_to_nothing_is_an_error_not_a_new_conversation);
        $case!(the_direct_conversation_survives_a_race);
        $case!(history_reads_backwards_and_forwards_over_the_same_window);
        $case!(history_clamps_an_abusive_limit);
        $case!(a_cursor_only_moves_forward_and_never_past_the_end);
        $case!(deleting_a_message_takes_the_payload_with_it);
        $case!(leaving_a_conversation_keeps_the_membership_row);
        $case!(the_conversation_list_is_ordered_by_activity_and_counts_unread);
        $case!(purging_expired_messages_respects_its_budget_and_never_reuses_a_sequence);
        $case!(creating_a_room_creates_its_conversation_and_its_owner);
        $case!(a_full_room_refuses_a_newcomer_but_not_a_member);
        $case!(a_ban_is_not_shed_by_leaving_and_coming_back);
        $case!(the_roster_pages_by_role_then_seniority);
        $case!(ownership_moves_in_one_step_or_not_at_all);
        $case!(archiving_a_room_closes_it_without_deleting_it);
        $case!(updating_a_room_can_clear_a_topic_without_clearing_a_name);
        $case!(a_block_stops_contact_in_both_directions);
        $case!(accepting_a_friend_request_writes_both_sides);
        $case!(relationship_pages_are_newest_first_and_clamped);
        $case!(a_wallet_is_opened_once_however_often_it_is_asked_for);
        $case!(a_transfer_moves_value_without_creating_any);
        $case!(a_retried_payment_charges_once);
        $case!(the_ledger_refuses_anything_that_does_not_balance);
        $case!(a_deleted_media_row_outlives_its_bytes);
        $case!(the_moderation_queue_is_oldest_first_and_resolves_once);
        $case!(the_audit_log_is_newest_first_and_scoped_to_one_target);
        $case!(a_game_token_moves_even_within_one_millisecond);
    };
}
