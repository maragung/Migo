//! Integration tests for `migod`, the composition root and the workspace's only binary.
//!
//! Every other crate is tested for what it *does*. `migod` is different: its job is to *connect*,
//! and the invariants that matter here are the ones that are silent when they hold and catastrophic
//! when they break — the kind no unit test in a leaf crate is positioned to catch, because no leaf
//! crate can see the whole graph. This suite guards six of them.
//!
//! 1. **Production refuses to start unsafe.** A node that reaches a hardened environment with no
//!    token key, with the *documented* placeholder key, or pointed at the *documented* development
//!    database login must refuse to boot — loudly, by naming the field, and without ever echoing
//!    the secret it rejected into a log line an operator might paste into a ticket. The same
//!    posture must *not* fire in development, where those defaults are the point.
//! 2. **Configuration layers and validates predictably.** Defaults, then files, then environment;
//!    an unknown key is a mistake, not a silent no-op; an invalid value is reported against the
//!    field that carried it; and every problem is collected so one restart surfaces the whole list.
//! 3. **The composition graph is acyclic and complete.** Every service can be built against
//!    in-memory backends with no socket and no database; building twice yields two independent
//!    systems that share nothing; and the one adapter that bridges two sibling domains (games into
//!    the economy) forwards, while its null counterpart swallows.
//! 4. **Nothing sensitive reaches a log, a metric, or an error.** The one metric registry the whole
//!    process shares renders no account, device, or node identifier (brief section 174), and the
//!    [`Config`] a panic might print redacts its token key and its database credential.
//! 5. **Shutdown is clean and idempotent.** One signal, shared by clone; triggering it twice is not
//!    a bug; and a task already past the trigger observes it immediately.
//! 6. **The binary's front door is cheap and honest.** `--help` and `--version` are answered by a
//!    pure parser that touches no configuration and no database; an unrecognised argument is a
//!    usage error, never a silently ignored flag. The parser is driven in-process; the built binary
//!    is never spawned.

use migo_core::config::{ConfigError, Environment, StoreBackend, DEVELOPMENT_TOKEN_KEY};
use migo_core::{Config, Secret, Shutdown};
use migod::cli::{self, Command};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Turns `&str` pairs into the owned environment slice the config loader takes. Mirrors the helper
/// the `migo-core` unit tests use, so a config built here is built exactly as the daemon builds it.
fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// Turns `&str` arguments into the owned iterator [`cli::parse`] takes.
fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_string()).collect()
}

/// A real, 32-byte token key encoded as standard base64 — the shape a production node is required
/// to carry. `[7u8; 32]` is arbitrary; what matters is that it decodes to the minimum length.
fn valid_token_key() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// Builds a config from an explicit environment without touching the process environment, then
/// returns the message [`Config::validate`] refuses it with. Panics if validation *passes*, because
/// a test that expected a refusal and got none is a failed test, not a silent one.
fn refusal(pairs: &[(&str, &str)]) -> String {
    Config::from_sources(&[], &env(pairs))
        .expect("configuration should parse")
        .validate()
        .expect_err("configuration should be refused")
        .to_string()
}

// ---------------------------------------------------------------------------
// Area 6: the binary's front door (the in-process argument parser)
// ---------------------------------------------------------------------------
// These run first because they are the cheapest proof the crate links and the surface a human
// touches first. The parser must decide help and version *without* the machinery the serve path
// needs, so none of these tests build a runtime, read a file, or open a connection.

#[test]
fn no_arguments_means_serve() {
    assert_eq!(cli::parse(args(&[])).unwrap(), Command::Serve);
}

#[test]
fn long_help_flag_is_help() {
    assert_eq!(cli::parse(args(&["--help"])).unwrap(), Command::Help);
}

#[test]
fn short_help_flag_is_help() {
    assert_eq!(cli::parse(args(&["-h"])).unwrap(), Command::Help);
}

#[test]
fn long_version_flag_is_version() {
    assert_eq!(cli::parse(args(&["--version"])).unwrap(), Command::Version);
}

#[test]
fn short_version_flag_is_version() {
    assert_eq!(cli::parse(args(&["-V"])).unwrap(), Command::Version);
}

#[test]
fn an_unknown_flag_is_a_usage_error() {
    assert!(cli::parse(args(&["--nope"])).is_err());
}

#[test]
fn a_usage_error_names_the_offending_argument() {
    let error = cli::parse(args(&["--frobnicate"])).unwrap_err();
    assert_eq!(error.arg(), "--frobnicate");
}

#[test]
fn a_usage_error_reads_as_a_usage_error() {
    // The Display is what `main` prints to stderr; it must say what went wrong and where to look,
    // and it must be a real `std::error::Error` so `main` can treat it as one.
    let error = cli::parse(args(&["--frobnicate"])).unwrap_err();
    let rendered = error.to_string();
    assert!(rendered.contains("--frobnicate"), "{rendered}");
    assert!(rendered.contains("Usage"), "{rendered}");
    assert!(rendered.contains("--help"), "{rendered}");
    // It implements the trait, not merely a `to_string`.
    let _: &dyn std::error::Error = &error;
}

#[test]
fn a_stray_positional_argument_is_rejected() {
    // `migod` has no subcommands; a bare word is as much a mistake as a bad flag, and swallowing it
    // silently would hide a typo in an init script.
    assert!(cli::parse(args(&["serve"])).is_err());
}

#[test]
fn the_first_recognised_flag_wins() {
    // `migod --help --version` prints help: the first request recognised is the one honoured,
    // rather than letting a later argument override an earlier one.
    assert_eq!(
        cli::parse(args(&["--help", "--version"])).unwrap(),
        Command::Help
    );
    assert_eq!(
        cli::parse(args(&["--version", "--help"])).unwrap(),
        Command::Version
    );
}

#[test]
fn a_bad_flag_trailing_a_good_one_is_still_rejected() {
    // Recognising `--help` does not stop the scan: `migod --help --frobnicate` is a misspelling an
    // operator needs told about, not a request to print help and pretend the rest was meant.
    let error = cli::parse(args(&["--help", "--frobnicate"])).unwrap_err();
    assert_eq!(error.arg(), "--frobnicate");
}

#[test]
fn the_version_line_names_the_binary_and_its_version() {
    assert!(
        cli::VERSION_LINE.starts_with("migod "),
        "{}",
        cli::VERSION_LINE
    );
    assert!(
        cli::VERSION_LINE.contains(env!("CARGO_PKG_VERSION")),
        "{}",
        cli::VERSION_LINE
    );
}

#[test]
fn the_help_text_documents_both_flags() {
    assert!(cli::HELP.contains("--help"), "{}", cli::HELP);
    assert!(cli::HELP.contains("--version"), "{}", cli::HELP);
}

#[test]
fn a_usage_error_exits_two_by_convention() {
    // Distinct from the `1` the serve path returns on a runtime failure, so an init system can tell
    // "you invoked me wrong" from "I tried to run and failed".
    assert_eq!(cli::EXIT_USAGE, 2);
}

// ---------------------------------------------------------------------------
// Area 1: production startup refusals
// ---------------------------------------------------------------------------

#[test]
fn production_refuses_a_missing_token_key() {
    let rendered = refusal(&[("MIGO_NODE__ENVIRONMENT", "production")]);
    assert!(rendered.contains("auth.token_key"), "{rendered}");
}

#[test]
fn production_refuses_an_empty_token_key() {
    // An empty environment value means "unset", so an operator who exported an empty key gets the
    // same refusal as one who set nothing at all, rather than a node signing with a zero-length key.
    let rendered = refusal(&[
        ("MIGO_NODE__ENVIRONMENT", "production"),
        ("MIGO_AUTH__TOKEN_KEY", ""),
    ]);
    assert!(rendered.contains("auth.token_key"), "{rendered}");
}

#[test]
fn production_refuses_the_documented_development_key() {
    let rendered = refusal(&[
        ("MIGO_NODE__ENVIRONMENT", "production"),
        ("MIGO_AUTH__TOKEN_KEY", DEVELOPMENT_TOKEN_KEY),
    ]);
    assert!(rendered.contains("development placeholder"), "{rendered}");
}

#[test]
fn the_development_key_refusal_does_not_echo_the_key() {
    // The rejected value must not travel into the error text a user might paste into a bug report.
    let rendered = refusal(&[
        ("MIGO_NODE__ENVIRONMENT", "production"),
        ("MIGO_AUTH__TOKEN_KEY", DEVELOPMENT_TOKEN_KEY),
    ]);
    assert!(!rendered.contains(DEVELOPMENT_TOKEN_KEY), "{rendered}");
}

#[test]
fn production_refuses_a_short_token_key() {
    use base64::Engine as _;
    let short = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
    let rendered = refusal(&[
        ("MIGO_NODE__ENVIRONMENT", "production"),
        ("MIGO_AUTH__TOKEN_KEY", &short),
    ]);
    assert!(rendered.contains("at least 32 bytes"), "{rendered}");
    // And the (secret) key material never appears in the complaint about it.
    assert!(!rendered.contains(&short), "{rendered}");
}

#[test]
fn development_accepts_the_documented_development_key() {
    // The placeholder is the placeholder *for* development; refusing it here would make the default
    // laptop config fail to boot, which is exactly the friction the hardened check is scoped to
    // avoid.
    let config = Config::from_sources(
        &[],
        &env(&[("MIGO_AUTH__TOKEN_KEY", DEVELOPMENT_TOKEN_KEY)]),
    )
    .expect("configuration should parse");
    assert!(config.validate().is_ok());
}

#[test]
fn production_refuses_the_documented_database_credential() {
    // The compose file and CI ship a well-known `migo:migo` Postgres login. A hardened node still
    // pointed at it is authenticating real traffic with a credential every reader of the repository
    // already knows; it must refuse.
    let rendered = refusal(&[
        ("MIGO_NODE__ENVIRONMENT", "production"),
        ("MIGO_STORE__BACKEND", "postgres"),
        ("MIGO_STORE__URL", "postgres://migo:migo@db:5432/migo"),
    ]);
    assert!(rendered.contains("store.url"), "{rendered}");
}

#[test]
fn the_database_credential_refusal_does_not_echo_the_credential() {
    // The whole point is to keep the credential out of logs, so the refusal that flags it must not
    // reproduce it.
    let rendered = refusal(&[
        ("MIGO_NODE__ENVIRONMENT", "production"),
        ("MIGO_STORE__BACKEND", "postgres"),
        ("MIGO_STORE__URL", "postgres://migo:migo@db:5432/migo"),
    ]);
    assert!(!rendered.contains("migo:migo"), "{rendered}");
}

#[test]
fn development_accepts_the_documented_database_credential() {
    // On a laptop, `migo:migo` is the normal login; the credential check is a hardened-only posture.
    let config = Config::from_sources(
        &[],
        &env(&[
            ("MIGO_STORE__BACKEND", "postgres"),
            (
                "MIGO_STORE__URL",
                "postgres://migo:migo@localhost:5432/migo",
            ),
        ]),
    )
    .expect("configuration should parse");
    assert!(config.validate().is_ok());
}

#[test]
fn staging_is_hardened_like_production() {
    // The refusals are keyed on "is this a hardened environment", not on "is this literally
    // production", so staging gets the same protection.
    let rendered = refusal(&[("MIGO_NODE__ENVIRONMENT", "staging")]);
    assert!(rendered.contains("auth.token_key"), "{rendered}");
}

// ---------------------------------------------------------------------------
// Area 2: configuration precedence and validation
// ---------------------------------------------------------------------------

#[test]
fn the_default_environment_is_development() {
    let config = Config::from_toml_str("", &[]).expect("empty config is all defaults");
    assert_eq!(config.node.environment, Environment::Development);
}

#[test]
fn the_default_store_backend_is_memory() {
    let config = Config::from_toml_str("", &[]).expect("empty config is all defaults");
    assert_eq!(config.store.backend, StoreBackend::Memory);
}

#[test]
fn the_environment_overrides_a_file_value() {
    // Precedence is defaults, then files, then environment. A value set in both must resolve to the
    // environment's.
    let config = Config::from_toml_str(
        "[node]\nenvironment = \"staging\"\n",
        &env(&[("MIGO_NODE__ENVIRONMENT", "production")]),
    )
    .expect("configuration should parse");
    assert_eq!(config.node.environment, Environment::Production);
}

#[test]
fn an_unknown_key_is_rejected() {
    // `deny_unknown_fields` turns a typo into a startup failure rather than a setting that silently
    // does nothing.
    let error = Config::from_toml_str("[node]\nregionn = \"eu\"\n", &[])
        .expect_err("a misspelled key must fail");
    assert!(matches!(error, ConfigError::Schema(_)), "{error:?}");
}

#[test]
fn an_unknown_key_error_names_the_key() {
    let error = Config::from_toml_str("[node]\nregionn = \"eu\"\n", &[])
        .expect_err("a misspelled key must fail");
    assert!(error.to_string().contains("regionn"), "{error}");
}

#[test]
fn an_invalid_enum_value_is_a_schema_error() {
    let error = Config::from_sources(&[], &env(&[("MIGO_NODE__ENVIRONMENT", "teapot")]))
        .expect_err("an unknown environment must fail");
    assert!(matches!(error, ConfigError::Schema(_)), "{error:?}");
    assert!(error.to_string().contains("teapot"), "{error}");
}

#[test]
fn an_invalid_value_is_reported_against_its_field() {
    // A coherence problem names the field that carries it, so the operator reads "fix this key",
    // not "something, somewhere, is wrong".
    let rendered = refusal(&[("MIGO_HTTP__MAX_BODY_BYTES", "0")]);
    assert!(rendered.contains("http.max_body_bytes"), "{rendered}");
}

#[test]
fn every_problem_is_reported_at_once() {
    // One restart should surface the whole list, not the first mistake and then, after a fix, the
    // second.
    let rendered = refusal(&[
        ("MIGO_HTTP__BIND", "not-an-address"),
        ("MIGO_STORE__MAX_CONNECTIONS", "0"),
    ]);
    assert!(rendered.contains("http.bind"), "{rendered}");
    assert!(rendered.contains("store.max_connections"), "{rendered}");
}

#[test]
fn an_empty_environment_value_means_unset() {
    // So exporting `MIGO_NODE__SIGNING_KEY=` reads as "no key", not "an empty key".
    let config = Config::from_sources(&[], &env(&[("MIGO_NODE__SIGNING_KEY", "")]))
        .expect("configuration should parse");
    assert!(config.node.signing_key.is_none());
}

// ---------------------------------------------------------------------------
// Area 4 (part): the config a panic might print redacts its secrets
// ---------------------------------------------------------------------------

#[test]
fn the_config_debug_does_not_leak_the_token_key() {
    let key = valid_token_key();
    let config = Config::from_sources(&[], &env(&[("MIGO_AUTH__TOKEN_KEY", &key)]))
        .expect("configuration should parse");
    let debug = format!("{config:?}");
    assert!(
        !debug.contains(&key),
        "token key leaked into Debug: {debug}"
    );
}

#[test]
fn the_config_debug_does_not_leak_the_database_credential() {
    let config = Config::from_sources(
        &[],
        &env(&[("MIGO_STORE__URL", "postgres://user:s3cr3t@localhost/migo")]),
    )
    .expect("configuration should parse");
    let debug = format!("{config:?}");
    assert!(
        !debug.contains("s3cr3t"),
        "db password leaked into Debug: {debug}"
    );
}

#[test]
fn a_secret_redacts_itself_in_debug() {
    // The mechanism the two tests above rely on, asserted directly: a `Secret`'s Debug is the
    // redaction form, never the bytes.
    let secret = Secret::new("super-secret-value");
    let debug = format!("{secret:?}");
    assert!(!debug.contains("super-secret-value"), "{debug}");
}

#[test]
fn the_summary_is_safe_to_log() {
    // `Config::summary` is printed at startup; it must carry composition and backends but neither
    // the token key nor the database credential.
    let key = valid_token_key();
    let config = Config::from_sources(
        &[],
        &env(&[
            ("MIGO_AUTH__TOKEN_KEY", &key),
            ("MIGO_STORE__URL", "postgres://user:s3cr3t@localhost/migo"),
        ]),
    )
    .expect("configuration should parse");
    let summary = config.summary();
    assert!(!summary.contains(&key), "{summary}");
    assert!(!summary.contains("s3cr3t"), "{summary}");
}

// ---------------------------------------------------------------------------
// Area 5: shutdown is clean and idempotent
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_shutdown_is_not_triggered() {
    assert!(!Shutdown::new().is_triggered());
}

#[test]
fn triggering_a_shutdown_flips_it() {
    let shutdown = Shutdown::new();
    shutdown.trigger();
    assert!(shutdown.is_triggered());
}

#[test]
fn triggering_a_shutdown_twice_is_idempotent() {
    // A second SIGTERM, or a manual trigger racing the signal handler, must not panic or reset.
    let shutdown = Shutdown::new();
    shutdown.trigger();
    shutdown.trigger();
    assert!(shutdown.is_triggered());
}

#[test]
fn a_cloned_shutdown_shares_the_signal() {
    // The gateway, the axum server, and the signal handler all hold clones of one signal; a trigger
    // on any must be seen by all.
    let shutdown = Shutdown::new();
    let clone = shutdown.clone();
    shutdown.trigger();
    assert!(clone.is_triggered());
}

#[tokio::test]
async fn cancelled_returns_immediately_once_triggered() {
    // A task that starts awaiting the signal after it has already fired must not block forever.
    let shutdown = Shutdown::new();
    shutdown.trigger();
    shutdown.cancelled().await;
}

// ---------------------------------------------------------------------------
// Area 3: the composition graph is acyclic and complete, plus the port adapters
// ---------------------------------------------------------------------------
// `App::build` against the default configuration is the whole graph assembled with in-memory
// backends. It opens no socket and reaches no database: the memory store connects to nothing, and
// in development an absent signing key yields an ephemeral secret rather than a refusal. If any
// crate's constructor were ordered before one of its arguments, or a cycle introduced, none of
// these would build.

use std::collections::HashMap;
use std::sync::Arc;

use migo_auth::{DeviceClaim, Registration, RequestContext};
use migo_core::{Id, Timestamp};
use migo_games::{Rewards, Unrewarded};
use migo_media::Storage;
use migo_moderation::{Powers, Roster};
use migo_protocol::Platform;
use migod::ports::{EconomyRewards, FsStorage, StaffRoster};

/// A development environment with in-memory backends plus a real token key. `migo_auth` requires a
/// token key to sign session tokens in *every* environment; the bare defaults leave it unset — they
/// validate, but cannot open authentication — so a development operator supplies one exactly as
/// this does. Extra pairs are appended and take precedence.
fn dev_env(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    pairs.push(("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()));
    pairs.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
    );
    pairs
}

/// Builds an `App` against a development configuration with in-memory backends: memory store and
/// cache, filesystem media, an ephemeral node secret, and no socket bound. The one place these
/// tests hold the whole graph at once.
async fn build_default_app() -> migod::App {
    let config = Config::from_sources(&[], &dev_env(&[])).expect("configuration should parse");
    migod::App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

#[tokio::test]
async fn a_development_configuration_builds_the_whole_graph() {
    // That this returns at all is the proof the twenty-one-crate graph is acyclic and every
    // service's dependencies are constructed before it.
    let app = build_default_app().await;
    assert_eq!(app.bind, Config::default().http.bind);
}

#[tokio::test]
async fn a_built_app_starts_with_an_untriggered_shutdown() {
    let app = build_default_app().await;
    assert!(!app.shutdown.is_triggered());
}

#[tokio::test]
async fn building_twice_yields_independent_authenticators() {
    // Two builds must not alias a service. If a constructor cached a global, these would be the
    // same pointer.
    let first = build_default_app().await;
    let second = build_default_app().await;
    assert!(!Arc::ptr_eq(&first.auth, &second.auth));
}

#[tokio::test]
async fn building_twice_yields_independent_economies() {
    let first = build_default_app().await;
    let second = build_default_app().await;
    assert!(!Arc::ptr_eq(&first.economy, &second.economy));
}

#[tokio::test]
async fn building_twice_yields_independent_games() {
    let first = build_default_app().await;
    let second = build_default_app().await;
    assert!(!Arc::ptr_eq(&first.games, &second.games));
}

#[tokio::test]
async fn building_twice_yields_independent_registries() {
    // One registry per process, but a fresh one per build; two apps never share a metric namespace.
    let first = build_default_app().await;
    let second = build_default_app().await;
    assert!(!Arc::ptr_eq(&first.registry, &second.registry));
}

// ---------------------------------------------------------------------------
// Area 4 (part): the shared metric registry carries no identity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_metric_registry_carries_no_node_identity() {
    // Brief section 174: metrics are not labelled by account, device, node, or peer id. Build with
    // a distinctive node id and assert the registry the whole process shares — the very instance
    // the REST API renders at `/metrics` — never spells it out.
    let config = Config::from_sources(
        &[],
        &dev_env(&[("MIGO_NODE__ID", "migonode-pii-canary-7f3a")]),
    )
    .expect("configuration should parse");
    let app = migod::App::build(&config)
        .await
        .expect("the configuration must build");
    let rendered = app.registry.render();
    assert!(
        !rendered.contains("migonode-pii-canary-7f3a"),
        "the node id reached a metric: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Area 3 (part): the null reward sink vs the real economy bridge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_null_reward_sink_swallows_experience() {
    // `Unrewarded` is the default when no economy is wired; a game that finishes on such a node
    // still succeeds, dropping the credit rather than failing the game.
    Unrewarded
        .award_experience(
            Id::from(1u128),
            10,
            Id::from(2u128),
            Timestamp::from_millis(1),
        )
        .await
        .expect("the null sink accepts an award");
}

#[tokio::test]
async fn the_null_reward_sink_swallows_a_win() {
    Unrewarded
        .mark_winner(Id::from(1u128), Id::from(2u128), Timestamp::from_millis(1))
        .await
        .expect("the null sink accepts a win");
}

/// Registers one real account through a built app and returns its id.
///
/// An invented id will not do: a reward is a write against a wallet, a wallet belongs to an
/// account, and the economy rightly refuses to credit an account that does not exist. The only
/// way to make one through [`migod::App`] is the front door every client uses, which is also what
/// makes these tests exercise two domains wired to the same store rather than one in isolation.
async fn registered_account(app: &migod::App, username: &str) -> Id {
    app.auth
        .register(
            Registration {
                username: username.to_string(),
                email: None,
                phone: None,
                password: Secret::new("correct-horse-battery-staple"),
                locale: "en-US".to_string(),
                country: None,
                device: DeviceClaim::new(Platform::Web, "integration test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(Timestamp::from_millis(1)),
        )
        .await
        .expect("a development app registers an account")
        .account_id
}

#[tokio::test]
async fn the_economy_bridge_forwards_a_game_credit() {
    // The real adapter, over the real economy from a built app: a finished game credits experience
    // as an economy award tagged `Source::Game`. A malformed award, or one aimed at an account the
    // economy cannot find, would error here.
    let app = build_default_app().await;
    let account = registered_account(&app, "creditplayer").await;
    let rewards = EconomyRewards::new(app.economy.clone());
    rewards
        .award_experience(account, 25, Id::from(2002u128), Timestamp::from_millis(1))
        .await
        .expect("the credit forwards to the economy");
}

#[tokio::test]
async fn the_economy_bridge_credit_is_idempotent_per_game() {
    // The adapter tags the award with the game id, so replaying a finished game's reward adds
    // nothing a second time — and, crucially, does not error.
    let app = build_default_app().await;
    let account = registered_account(&app, "replayplayer").await;
    let rewards = EconomyRewards::new(app.economy.clone());
    let game = Id::from(2002u128);
    let now = Timestamp::from_millis(1);
    rewards
        .award_experience(account, 25, game, now)
        .await
        .expect("first credit");
    rewards
        .award_experience(account, 25, game, now)
        .await
        .expect("a replayed credit is absorbed, not rejected");
}

#[tokio::test]
async fn the_economy_bridge_forwards_a_win_as_a_badge() {
    // A win becomes a `GameChampion` badge grant; the grant is idempotent, so replaying it is safe.
    let app = build_default_app().await;
    let account = registered_account(&app, "winnerplayer").await;
    let rewards = EconomyRewards::new(app.economy.clone());
    let game = Id::from(2002u128);
    let now = Timestamp::from_millis(1);
    rewards
        .mark_winner(account, game, now)
        .await
        .expect("the win forwards to the economy");
    rewards
        .mark_winner(account, game, now)
        .await
        .expect("a replayed win is absorbed, not rejected");
}

#[tokio::test]
async fn the_economy_bridge_refuses_a_credit_for_an_account_that_does_not_exist() {
    // The complement of the three above, and the reason they need a real account: the bridge does
    // not invent a wallet for an unknown id. `Games` treats this refusal as a dropped reward and
    // counts it rather than failing the finished game, so the error must surface here to be
    // droppable there — swallowing it inside the adapter would hide a real inconsistency.
    let app = build_default_app().await;
    let rewards = EconomyRewards::new(app.economy.clone());
    let error = rewards
        .award_experience(
            Id::from(404u128),
            25,
            Id::from(2002u128),
            Timestamp::from_millis(1),
        )
        .await
        .expect_err("an unknown account cannot be credited");
    assert_eq!(error.code(), migo_protocol::codes::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Area 3 (part): the staff roster port
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_empty_staff_roster_grants_nobody_anything() {
    // The development posture: nobody is staff, so every operator action is refused.
    let roster = StaffRoster::empty();
    assert_eq!(roster.powers(Id::from(7u128)).await.unwrap(), Powers::NONE);
}

#[tokio::test]
async fn a_configured_staff_roster_grants_the_listed_powers() {
    let operator = Id::from(7u128);
    let mut staff = HashMap::new();
    staff.insert(operator, Powers::SUSPEND);
    let roster = StaffRoster::new(staff);
    assert_eq!(roster.powers(operator).await.unwrap(), Powers::SUSPEND);
}

#[tokio::test]
async fn a_configured_staff_roster_grants_strangers_nothing() {
    // An account not on the roster resolves to `NONE`, never an error: "not staff" is the common
    // case, asked on every operator request.
    let mut staff = HashMap::new();
    staff.insert(Id::from(7u128), Powers::SUSPEND);
    let roster = StaffRoster::new(staff);
    assert_eq!(roster.powers(Id::from(8u128)).await.unwrap(), Powers::NONE);
}

// ---------------------------------------------------------------------------
// Area 3 (part): the filesystem storage port
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filesystem_storage_normalises_a_trailing_slash() {
    // A doubled slash in a media URL is a real bug when the public base is configured with a
    // trailing slash, as URLs often are. `new` strips it so joining a key yields exactly one.
    let storage = FsStorage::new(std::env::temp_dir(), "https://media.example/m/");
    let grant = storage
        .sign_download("objectkey", Timestamp::from_millis(1000))
        .await
        .expect("an unsigned download URL is always available");
    assert_eq!(grant.expose(), "https://media.example/m/objectkey");
}

#[tokio::test]
async fn filesystem_storage_refuses_a_traversal_key() {
    // Keys are server-generated and flat; a key that tries to climb out of the media root is
    // refused rather than followed (defence in depth).
    let storage = FsStorage::new(std::env::temp_dir(), "https://media.example/m");
    assert!(storage.head("../../etc/passwd", 16).await.is_err());
}

#[tokio::test]
async fn filesystem_storage_refuses_an_empty_key() {
    let storage = FsStorage::new(std::env::temp_dir(), "https://media.example/m");
    assert!(storage.head("", 16).await.is_err());
}

#[tokio::test]
async fn filesystem_storage_refuses_an_absolute_key() {
    let storage = FsStorage::new(std::env::temp_dir(), "https://media.example/m");
    assert!(storage.head("/etc/passwd", 16).await.is_err());
}

#[tokio::test]
async fn filesystem_storage_reports_an_absent_object_as_absent() {
    // A safe key under a root that holds nothing is `Ok(None)`, not an error: "not there" is a
    // normal answer the media service's size check and sweeper both depend on.
    let storage = FsStorage::new(std::env::temp_dir(), "https://media.example/m");
    let head = storage
        .head("migod-absent-object-canary-4b1c9e", 16)
        .await
        .expect("an absent object is not an error");
    assert!(head.is_none());
}

// ---------------------------------------------------------------------------
// Area 7: the gateway's `SUBSCRIBE` authorization seam reaches the domain.
//
// The test file that proves the contract the four other crates uphold is
// `migo-gateway/tests/gateway.rs`. Here we drive the same seam through the
// real `AppDispatcher` built by `migod`, against the real domain services
// against the in-memory store, so the seam is exercised end to end: a
// `TopicRequest` goes in, the dispatcher's `authorize_topics` consults the
// store, and the verdict is what the wire would carry.
// ---------------------------------------------------------------------------

use migo_auth::Identity as AuthIdentity;
use migo_gateway::{Dispatcher as GatewayDispatcher, TopicRequest as GatewayTopicRequest};
use migo_messaging::Caller as MessageCaller;
use migo_protocol::{ConversationKind, RoomKind, Topic, TopicKind};
use migo_ratelimit::TrustTier;
use migo_rooms::NewRoomRequest;
use migod::dispatch::AppDispatcher;

/// The `Identity` a `SUBSCRIBE` arrives with, built from a registered account so the dispatcher's
/// domain calls (membership, privacy gate) have real rows to read.
///
/// Device and session ids are not what the dispatcher looks at — only the account id and the
/// privacy/membership rows behind it matter — so they only have to be non-nil. Two reserved
/// sentinel values keep the seam honest with the messages a real gateway would forward, without
/// requiring the test to mint and track a third id alongside the account.
fn identity_for(account: Id, username: &str) -> AuthIdentity {
    AuthIdentity {
        claims: migo_auth::Claims {
            account_id: account,
            device_id: id(0xD1_0000),
            session_id: id(0x5E_0000),
            capabilities: migo_auth::Capabilities::NONE,
            issued_at: Timestamp::from_millis(1),
            expires_at: Timestamp::from_millis(1_700_000_000_000),
            authenticated_at: Timestamp::from_millis(1),
        },
        username: username.to_string(),
        tier: TrustTier::Established,
        capabilities: migo_auth::Capabilities::NONE,
    }
}

fn id(v: u128) -> Id {
    Id::from(v)
}

fn now() -> Timestamp {
    Timestamp::from_millis(1)
}

/// A real `AppDispatcher` wired to the same services `App::build` wires the production gateway
/// to. `App` does not re-export the dispatcher it built — `GatewayServices::dispatcher` holds
/// the only public reference — so the harness re-constructs the equivalent value from the
/// service handles `App` does expose. The two instances are functionally interchangeable
/// because the only state `AppDispatcher` holds is the six `Arc<dyn Trait>` handles, and those
/// are what we re-use; the test is therefore a property of the seam, not of the binary's
/// specific pointer to it.
struct DispatcherHarness {
    app: migod::App,
    dispatcher: AppDispatcher,
}

async fn dispatcher() -> DispatcherHarness {
    let app = build_default_app().await;
    // The dispatcher's own store handle: a fresh in-memory store is correct here, because
    // the profile-update path is the only consumer and the test seeds its own account.
    let app_store: migo_store::SharedStore = std::sync::Arc::new(migo_store::MemoryStore::new());
    let dispatcher = AppDispatcher::new(
        app_store.clone(),
        app.messaging.clone(),
        app.presence.clone(),
        app.rooms.clone(),
        app.keys.clone(),
        app.social.clone(),
        app.games.clone(),
        app.media.clone(),
        app.economy.clone(),
        app.moderation.clone(),
        app.notify.clone(),
        app.federation.clone(),
        app.bots.clone(),
        app.calls.clone(),
    );
    DispatcherHarness { app, dispatcher }
}

/// A direct conversation between `a` and `b`, the simplest conversation the messaging service
/// offers, and the one whose `is_participant` row the dispatcher asks.
async fn direct_conversation(app: &migod::App, a: Id, b: Id) -> Id {
    let caller = MessageCaller::new(a, id(0xD1_0001), TrustTier::Established, now());
    let summary = app
        .messaging
        .create(
            &caller,
            migo_protocol::ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![a, b],
                title: None,
            },
        )
        .await
        .expect("a direct conversation between two real accounts must build");
    summary.conversation_id
}

#[tokio::test]
async fn authorize_topics_grants_the_caller_their_own_user_topic() {
    let h = dispatcher().await;
    let alice = registered_account(&h.app, "alice").await;
    let identity = identity_for(alice, "alice");

    let verdict = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&identity, id(0x5E_0000), now()),
            &[Topic {
                kind: TopicKind::User,
                id: alice,
            }],
        )
        .await;

    assert_eq!(
        verdict,
        vec![true],
        "the caller's own presence topic is always theirs"
    );
}

#[tokio::test]
async fn authorize_topics_grants_conversation_membership() {
    let h = dispatcher().await;
    let alice = registered_account(&h.app, "alice").await;
    let bob = registered_account(&h.app, "bob").await;
    let carol = registered_account(&h.app, "carol").await;
    let conversation = direct_conversation(&h.app, alice, bob).await;

    let alice_identity = identity_for(alice, "alice");
    let carol_identity = identity_for(carol, "carol");

    let alice_verdict = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&alice_identity, id(0x5E_0000), now()),
            &[Topic {
                kind: TopicKind::Conversation,
                id: conversation,
            }],
        )
        .await;
    assert_eq!(
        alice_verdict,
        vec![true],
        "a member of the conversation may subscribe to it"
    );

    let carol_verdict = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&carol_identity, id(0x5E_0000), now()),
            &[Topic {
                kind: TopicKind::Conversation,
                id: conversation,
            }],
        )
        .await;
    assert_eq!(
        carol_verdict,
        vec![false],
        "a stranger to the conversation is not granted its topic"
    );

    // An id that no conversation has ever used is refused, indistinguishably from "you are not
    // in it" — the probe-resisting conflation section 48 demands.
    let absent = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&alice_identity, id(0x5E_0000), now()),
            &[Topic {
                kind: TopicKind::Conversation,
                id: id(0xDEAD_BEEF),
            }],
        )
        .await;
    assert_eq!(
        absent,
        vec![false],
        "an absent conversation is refused exactly the same way as a non-member"
    );
}

#[tokio::test]
async fn authorize_topics_grants_room_membership() {
    let h = dispatcher().await;
    let owner = registered_account(&h.app, "room_owner").await;
    let guest = registered_account(&h.app, "room_guest").await;

    // The owner creates a public room; the guest never asks to join it. `authorize` with an
    // empty mask is the membership check the dispatcher performs.
    let room_caller = migo_rooms::Caller::new(owner, id(0xD1_0001), TrustTier::Established, now());
    let room = h
        .app
        .rooms
        .create(
            &room_caller,
            NewRoomRequest {
                slug: "presence-room".to_string(),
                name: "Presence Room".to_string(),
                topic: None,
                kind: RoomKind::Public,
                max_members: None,
            },
        )
        .await
        .expect("a public room with a unique slug and an authenticated owner must build");

    let owner_identity = identity_for(owner, "owner");
    let guest_identity = identity_for(guest, "guest");

    let owner_verdict = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&owner_identity, id(0x5E_0000), now()),
            &[Topic {
                kind: TopicKind::Room,
                id: room.room_id,
            }],
        )
        .await;
    assert_eq!(
        owner_verdict,
        vec![true],
        "the owner of a room may hold its topic"
    );

    let guest_verdict = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&guest_identity, id(0x5E_0000), now()),
            &[Topic {
                kind: TopicKind::Room,
                id: room.room_id,
            }],
        )
        .await;
    assert_eq!(
        guest_verdict,
        vec![false],
        "a non-member is refused the room's topic"
    );
}

#[tokio::test]
async fn authorize_topics_honours_the_presence_privacy_gate() {
    let h = dispatcher().await;
    let alice = registered_account(&h.app, "alice").await;
    let bob = registered_account(&h.app, "bob").await;

    // The default privacy for a fresh profile is `Friends` (1) on `show_last_seen`, so a
    // stranger cannot see bob's last-seen time. Authorising bob's presence topic for alice —
    // who is not bob's friend — must therefore come back false.
    let alice_identity = identity_for(alice, "alice");

    let verdict = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&alice_identity, id(0x5E_0000), now()),
            &[Topic {
                kind: TopicKind::User,
                id: bob,
            }],
        )
        .await;
    assert_eq!(
        verdict,
        vec![false],
        "presence is gated by `show_last_seen`, and a stranger defaults to denied"
    );
}

#[tokio::test]
async fn authorize_topics_refuses_unknown_and_game_topics() {
    let h = dispatcher().await;
    let alice = registered_account(&h.app, "alice").await;
    let alice_identity = identity_for(alice, "alice");

    let verdict = h
        .dispatcher
        .authorize_topics(
            &GatewayTopicRequest::new(&alice_identity, id(0x5E_0000), now()),
            &[
                Topic {
                    kind: TopicKind::Unknown,
                    id: id(0x1234),
                },
                Topic {
                    kind: TopicKind::Game,
                    id: id(0x5678),
                },
            ],
        )
        .await;
    assert_eq!(
        verdict,
        vec![false, false],
        "no broadcast targets `Unknown` or `Game` topics, so both are refused"
    );
}

// ---------------------------------------------------------------------------
// Area 8: opcode 144 NOTIFICATION_EVENT round-trips through the gateway.
//
// The only IDL opcode that was still marked SCHEMA in migo.md section 177 is
// opcode 144, a server-to-client push. There is no inbound dispatcher arm
// for it — clients cannot send it, and the gateway already enforces that —
// so the test below is the *outbound* half: a server-side call to
// `Gateway::emit_notification` must reach a session that has subscribed to
// the recipient's `User` topic, in the form of a binary frame whose opcode
// is `NOTIFICATION_EVENT` (144).
// ---------------------------------------------------------------------------

use bytes::Bytes;
use migo_gateway::Transport;
use migo_gateway::TransportError;
use migo_protocol::{from_frame, Frame, Hello, NotificationEvent, NotificationKind, Opcode};
use std::collections::VecDeque;
use std::sync::Mutex;

/// A `Transport` over a pair of byte queues, just enough to drive a session to a subscription
/// and capture the frames the gateway sends back. The queues are shared so the test can both
/// push scripted client bytes and read the server's replies.
#[derive(Clone, Default)]
struct FakeTransport {
    inbound: Arc<Mutex<VecDeque<Bytes>>>,
    outbound: Arc<Mutex<Vec<Bytes>>>,
    closed: Arc<Mutex<bool>>,
    park: Arc<Mutex<bool>>,
}

impl FakeTransport {
    fn new() -> Self {
        Self::default()
    }

    fn client(&self, bytes: Bytes) {
        self.inbound.lock().unwrap().push_back(bytes);
    }

    fn sent(&self) -> Vec<Bytes> {
        self.outbound.lock().unwrap().clone()
    }

    fn keep_open(&self) {
        *self.park.lock().unwrap() = true;
    }
}

#[async_trait::async_trait]
impl Transport for FakeTransport {
    async fn recv(&mut self) -> Result<Option<Bytes>, TransportError> {
        let next = self.inbound.lock().unwrap().pop_front();
        match next {
            Some(bytes) => Ok(Some(bytes)),
            None => {
                if *self.park.lock().unwrap() {
                    std::future::pending().await
                } else {
                    Ok(None)
                }
            }
        }
    }

    async fn send(&mut self, frame: Bytes) -> Result<(), TransportError> {
        self.outbound.lock().unwrap().push(frame);
        Ok(())
    }

    async fn close(&mut self) {
        *self.closed.lock().unwrap() = true;
    }
}

#[tokio::test]
async fn emit_notification_reaches_a_subscribed_session_as_opcode_144() {
    // Build a full app so the gateway is wired against the real domain services and
    // an in-memory store. The user has to be real so the dispatcher's authorize_topics
    // path grants their own `User` topic.
    let app = build_default_app().await;
    let bob = registered_account(&app, "bobnotif").await;
    let now = app.clock.now();
    let bob_password = Secret::new("correct-horse-battery-staple");
    // A second sign-in to obtain a token for bob that the fake gateway-side transport
    // session can use to authenticate. The issued_at / expires_at are computed from
    // the context's now, so the token must be minted against the app's clock — not
    // against an arbitrary epoch — or the gateway-side verifier will see it as
    // already expired.
    let bob_sign_in = app
        .auth
        .sign_in(
            migo_auth::SignIn {
                identifier: "bobnotif".to_string(),
                password: bob_password,
                device: migo_auth::DeviceClaim::new(Platform::Web, "notif test"),
                captcha: None,
                server: None,
            },
            &migo_auth::RequestContext::at(now),
        )
        .await
        .expect("bob can sign in");
    let bob_token = bob_sign_in.access_token;
    let bob_device = bob_sign_in.device_id;

    // Drive a fake transport through the gateway: hello with token, subscribe to
    // bob's user topic, then yield so the gateway's writer has a chance to flush
    // before the test calls `emit_notification`.
    let transport = FakeTransport::new();
    transport.keep_open();
    let transport_for_serve = transport.clone();
    let shutdown = app.shutdown.clone();
    let gateway = app.gateway.clone();
    let serve = tokio::spawn(async move {
        gateway
            .serve(
                transport_for_serve,
                migo_auth::RequestContext::at(Timestamp::from_millis(1)),
            )
            .await;
    });

    // The hello+token promotes the session to Ready in one frame.
    let hello = Hello {
        protocol_version: migo_protocol::PROTOCOL_VERSION,
        access_token: Some(bob_token),
        device_id: Some(bob_device),
        ..Default::default()
    };
    let hello_frame = migo_protocol::to_frame(Opcode::Hello.to_wire(), 1, &hello)
        .expect("hello encodes")
        .encode()
        .expect("hello frame encodes");
    transport.client(hello_frame);

    // Subscribe to bob's user topic. The dispatcher's authorize_topics grants
    // bob's own topic and refuses anything else.
    let subscribe = migo_protocol::SubscribeRequest {
        topics: vec![migo_protocol::Topic {
            kind: migo_protocol::TopicKind::User,
            id: bob,
        }],
    };
    let sub_frame = migo_protocol::to_frame(Opcode::Subscribe.to_wire(), 2, &subscribe)
        .expect("subscribe encodes")
        .encode()
        .expect("subscribe frame encodes");
    transport.client(sub_frame);

    // Give the gateway time to process hello + subscribe before emitting the
    // notification. Without this yield the notification can race the subscribe
    // and find no subscribers yet.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Server-side trigger. This is the seam a domain crate would call when it has
    // something to wake a user about; the test calls it directly to assert the
    // round-trip without needing an end-to-end trigger path.
    let event = NotificationEvent {
        kind: NotificationKind::Mention,
        at: Timestamp::from_millis(1_700_000_000_000),
        title: Some("ping".to_string()),
        body: None,
        conversation_id: None,
        room_id: None,
        actor_id: None,
    };
    app.gateway.emit_notification(bob, &event, now);

    // Give the hub's writer a moment to deliver.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // The first two outbound frames are the WELCOME and the SUBSCRIBE response; the
    // third, if the round-trip works, is the NOTIFICATION_EVENT frame.
    let sent = transport.sent();
    let frames: Vec<Frame> = sent
        .iter()
        .cloned()
        .map(|b| Frame::decode(b).expect("an outbound frame must decode"))
        .collect();
    let notification_frame = frames
        .iter()
        .find(|frame| {
            frame.header.opcode == Opcode::NotificationEvent.to_wire() && !frame.header.is_error()
        })
        .expect("a NOTIFICATION_EVENT frame is broadcast to the subscriber");
    let decoded: NotificationEvent =
        from_frame(notification_frame).expect("the broadcast decodes as NotificationEvent");
    assert_eq!(decoded.kind, NotificationKind::Mention);
    assert_eq!(decoded.title.as_deref(), Some("ping"));

    // Clean teardown so the spawned task exits.
    shutdown.trigger();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), serve).await;
}
