//! The feature-bits contract: what a node advertises, and when (brief section 148).
//!
//! The advertised set is the whole truth about what a client can negotiate — the gateway
//! masks every HELLO's requested bits against exactly the set `App::build` settles on, so
//! a bit promised but not served is a client timeout waiting to happen. These tests pin
//! the four bits that were specification until now:
//!
//! * `VOICE_NOTE`, `GROUP_CALL`, and `RICH_PRESENCE` are advertised unconditionally: the
//!   surfaces they describe are part of this build and not tied to a listener. Their
//!   gating is field-level (RICH_PRESENCE's `PROFILE_UPDATE.custom_status`) or
//!   informational (voice notes ride the MEDIA opcodes every deployed client already
//!   uses; the SFU opcodes are deliberately not gated on GROUP_CALL — the calls section
//!   records that server decision).
//! * `FEDERATION` is advertised only while the mesh listener is bound, the same
//!   listener-conditional rule `QUIC` and `TCP_TRANSPORT` follow: a bit that promises a
//!   transport or a link must not be promised by a node that is not serving it.
//!
//! Built against the in-memory development configuration, the same way the listener tests
//! build it: one environment pair at a time, so each test observes the advertisement of
//! exactly one listener configuration.

use base64::Engine as _;

use migo_core::Config;
use migo_protocol::features;
use migod::App;

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

fn env(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> =
        vec![("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key())];
    pairs.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
    );
    pairs
}

async fn build_app(extra: &[(&str, &str)]) -> App {
    let config = Config::from_sources(&[], &env(extra)).expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

#[tokio::test]
async fn the_node_advertises_the_voice_note_group_call_and_rich_presence_bits() {
    let app = build_app(&[]).await;
    for (bit, name) in [
        (features::VOICE_NOTE, "VOICE_NOTE"),
        (features::GROUP_CALL, "GROUP_CALL"),
        (features::RICH_PRESENCE, "RICH_PRESENCE"),
    ] {
        assert_ne!(
            app.features & bit,
            0,
            "the {name} bit is advertised unconditionally: its surface is part of this build"
        );
    }
}

#[tokio::test]
async fn a_node_without_a_mesh_listener_advertises_no_federation() {
    let app = build_app(&[]).await;
    assert_eq!(
        app.features & features::FEDERATION,
        0,
        "the FEDERATION bit must not be advertised when no mesh listener is bound — the bit \
         promises server-to-server links this node is not accepting"
    );
}

#[tokio::test]
async fn a_node_with_a_mesh_listener_bound_advertises_the_federation_bit() {
    let app = build_app(&[("MIGO_NODE__MESH_BIND", "127.0.0.1:0")]).await;
    assert_ne!(
        app.features & features::FEDERATION,
        0,
        "the FEDERATION bit is advertised exactly while the mesh listener is bound, the same \
         listener-conditional rule QUIC and TCP_TRANSPORT follow"
    );
}
