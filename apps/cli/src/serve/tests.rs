//! T5 assembly tests (#1060).
//!
//! Scope: *the wiring*. The transport's socket behaviour is covered by
//! `tests_t3.rs` (which drives the reader directly, bypassing the WebSocket) and
//! the approver's gate by `tests_t4.rs`. What no test covered before this one is
//! the assembly between them, and it has a hazard the compiler cannot see.
//!
//! `take_interaction_rx` returns `None` until `connect` creates the channel
//! (`transport.rs:265`). Taking it early is neither a compile error nor a runtime
//! error — it silently produces an approver that never observes a click and
//! therefore times out, i.e. denies, every prompt. Nothing in production would
//! ever report it. `build_approver` exists to turn that into a loud failure, and
//! the first two tests are what hold it in place.
//!
//! A real `connect()` is not exercisable from here: it dials the WebSocket URL
//! the stub hands back, and both sibling suites avoid it for the same reason.

use ff_core::{Mode, PermissionMatrix};
use ff_transport::ChannelId;
use ff_transport_slack::{SlackApi, SlackTransport};

use super::{Connected, ServeArgs, SlackTokens, APP_TOKEN_VAR, BOT_TOKEN_VAR};
use crate::test_support::{with_env_set, with_env_unset, TestEnv};

fn channel() -> ChannelId {
    ChannelId::new("slack", "C9")
}

/// A `Connected` cannot be obtained without `connect`, so the ordering hazard is
/// a compile error rather than a runtime one. What remains testable — and what
/// this asserts — is that `approver` still refuses when the receiver is absent,
/// which is the case a future `connect` failing halfway would produce.
#[tokio::test]
async fn approver_refuses_when_the_interaction_receiver_is_absent() {
    let mut transport = SlackTransport::new("xapp-a", "xoxb-b");
    // `assume` without a real connect is exactly the "receiver never created"
    // state; production reaches this only via `Connected::open`.
    let mut connected = Connected::assume(&mut transport);

    let err = connected
        .approver(
            SlackApi::new("xoxb-b"),
            channel(),
            Mode::Auto,
            PermissionMatrix::default(),
        )
        .err()
        .expect("no receiver without connect");

    assert!(
        err.contains("interaction receiver already taken"),
        "the error must explain the single-consumer rule, got: {err}"
    );
}

/// The receiver is single-consumer, and the approver is its only consumer.
///
/// Two approvers draining one receiver would each see an arbitrary half of the
/// clicks, so prompts would resolve at random. Repeated builds must never both
/// succeed.
#[tokio::test]
async fn the_interaction_receiver_is_taken_at_most_once() {
    let mut transport = SlackTransport::new("xapp-a", "xoxb-b");
    let mut connected = Connected::assume(&mut transport);

    for attempt in 0..3 {
        assert!(
            connected
                .approver(
                    SlackApi::new("xoxb-b"),
                    channel(),
                    Mode::Auto,
                    PermissionMatrix::default(),
                )
                .is_err(),
            "attempt {attempt}: without a live receiver no approver may be produced"
        );
    }
}

/// The env-var override layer, including the empty-is-absent rule.
///
/// Uses `with_env_set` rather than open-coding `set_var`/`remove_var`: the
/// hand-rolled version this replaces restored the environment on the happy path
/// only, so a failing assertion midway leaked the vars into every later test
/// sharing the process (nextest isolates tests, but doctests do not).
/// Exercise the production resolution path end to end: read `transports.toml`,
/// then resolve the tokens from its `[slack]` section. Mirrors what `serve`
/// does, so these tests cannot pass via a test-only shortcut.
fn load_tokens() -> Result<SlackTokens, String> {
    let config = super::TransportsConfig::read()?;
    SlackTokens::from_parts(&config.slack.unwrap_or_default())
}

#[test]
fn tokens_come_from_the_environment_and_treat_empty_as_absent() {
    let _env = TestEnv::new();

    let err = with_env_unset(|| load_tokens().expect_err("no tokens"));
    assert!(err.contains(APP_TOKEN_VAR), "got: {err}");

    let err = with_env_set(&[(APP_TOKEN_VAR, "xapp-a"), (BOT_TOKEN_VAR, "")], || {
        load_tokens().expect_err("an empty var is absent, not a valid token")
    });
    assert!(err.contains(BOT_TOKEN_VAR), "got: {err}");

    let t = with_env_set(
        &[(APP_TOKEN_VAR, "xapp-a"), (BOT_TOKEN_VAR, "xoxb-b")],
        || load_tokens().expect("both set"),
    );
    assert_eq!(
        (t.app_token.as_str(), t.bot_token.as_str()),
        ("xapp-a", "xoxb-b")
    );
}

/// The allowlist is mandatory because the transport fails closed: an empty
/// `allowed_user_ids` rejects every sender (`transport.rs:186`), so booting with
/// "empty" would ack messages and answer nobody — a silent dead deployment.
///
/// clap no longer enforces it, because `transports.toml` is an equally valid
/// source (#1060 scope bullet 2). The invariant is unchanged and still enforced,
/// just one layer in: `serve` refuses before it opens a socket. This asserts the
/// rejection still happens when *neither* source supplies a user.
#[tokio::test]
async fn serve_refuses_an_empty_allowlist_from_either_source() {
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: ServeArgs,
    }

    let env = TestEnv::new();
    env.write_transports(
        r#"
        [slack]
        app_token = "xapp-a"
        bot_token = "xoxb-b"
        "#,
    );

    // Omitting --allow-user now parses; the file has no `allowed_users` either.
    let w = Wrap::try_parse_from(["x", "--channel", "C1"]).expect("omission parses");
    assert!(w.args.allow_user.is_empty());

    // The env must stay cleared across the await, so use the RAII guard rather
    // than the closure form (a closure would restore before `serve` even runs).
    let _clear = crate::test_support::EnvGuard::unset();
    let err = super::serve(w.args)
        .await
        .expect_err("an empty allowlist must be refused before connecting");
    assert!(
        err.contains("--allow-user") && err.contains("allowed_users"),
        "the error must name both sources; got: {err}"
    );
}

/// The file can supply the allowlist on its own, so an operator who configures
/// `transports.toml` fully does not also need the flag.
#[test]
fn the_allowlist_can_come_from_the_file() {
    let env = TestEnv::new();
    env.write_transports(
        r#"
        [slack]
        app_token = "xapp-a"
        bot_token = "xoxb-b"
        allowed_users = ["U1", "U2"]
        "#,
    );

    let config = super::TransportsConfig::read().expect("valid toml");
    assert_eq!(
        config.slack.expect("[slack] present").allowed_users,
        vec!["U1", "U2"]
    );
}

/// A typo in a credential key must be an error, not a silently missing token —
/// otherwise the reader is sent to look at the environment for a value they can
/// plainly see in the file.
#[test]
fn an_unknown_key_in_the_file_is_rejected() {
    let env = TestEnv::new();
    env.write_transports(
        r#"
        [slack]
        app_tokn = "xapp-typo"
        bot_token = "xoxb-b"
        "#,
    );

    let err = super::TransportsConfig::read().expect_err("a typo must not be ignored");
    assert!(
        err.contains("app_tokn") || err.contains("unknown field"),
        "got: {err}"
    );
}

#[test]
fn an_explicit_allowlist_still_parses() {
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: ServeArgs,
    }

    let w = Wrap::try_parse_from(["x", "--channel", "C1", "--allow-user", "U1"])
        .expect("an explicit allowlist parses");
    assert_eq!(w.args.allow_user, vec!["U1"]);
}

#[test]
fn serve_defaults_to_auto_and_splits_the_allowlist() {
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: ServeArgs,
    }

    let w = Wrap::parse_from(["x", "--channel", "C1", "--allow-user", "U1"]);
    assert!(matches!(w.args.mode, crate::ModeArg::Auto));

    // Comma-splitting matters: `--allow-user U1,U2` is the documented form, and
    // without `value_delimiter` it parses as a single id literally named "U1,U2",
    // which would silently allowlist nobody.
    let w = Wrap::parse_from(["x", "--channel", "C1", "--allow-user", "U1,U2"]);
    assert_eq!(w.args.allow_user, vec!["U1", "U2"]);
}

// ---------------------------------------------------------------------------
// #1060 scope bullet 2: read `transports.toml`. Env vars stay as an override
// so container/CI deployments (and these tests) can inject without a file —
// the same layering `host.rs` already uses for the provider config, where a
// per-connection value wins and the env var is the global override.
// ---------------------------------------------------------------------------

/// The file is the documented source of tokens, so a bare `transports.toml`
/// with no environment set must be enough to boot.
#[test]
fn tokens_are_read_from_transports_toml() {
    let env = TestEnv::new();
    env.write_transports(
        r#"
        [slack]
        app_token = "xapp-from-file"
        bot_token = "xoxb-from-file"
        "#,
    );

    let tokens = with_env_unset(|| load_tokens().expect("file alone is sufficient"));
    assert_eq!(tokens.app_token, "xapp-from-file");
    assert_eq!(tokens.bot_token, "xoxb-from-file");
}

/// A deployment that sets the env var must be able to override a checked-in
/// file without editing it. This is the layer `host.rs:66` establishes.
#[test]
fn the_environment_overrides_the_file() {
    let env = TestEnv::new();
    env.write_transports(
        r#"
        [slack]
        app_token = "xapp-from-file"
        bot_token = "xoxb-from-file"
        "#,
    );

    let tokens = with_env_set(&[(APP_TOKEN_VAR, "xapp-from-env")], || {
        load_tokens().expect("file supplies what the env does not")
    });
    assert_eq!(
        tokens.app_token, "xapp-from-env",
        "the env var must win over the file"
    );
    assert_eq!(
        tokens.bot_token, "xoxb-from-file",
        "an unset var must not blank out the file's value"
    );
}

/// #1060 acceptance 2: a missing token is a clear error, not a panic. The
/// message has to name *both* places a reader could fix it, or they will edit
/// the file when the env var is what is shadowing it (and vice versa).
#[test]
fn a_missing_token_names_both_sources() {
    let _env = TestEnv::new();

    let err = with_env_unset(|| load_tokens().expect_err("nothing configured anywhere"));
    assert!(err.contains("transports.toml"), "got: {err}");
    assert!(err.contains(APP_TOKEN_VAR), "got: {err}");
}

/// An empty string in the file is a half-finished edit, not a credential —
/// it must be treated as absent exactly as an empty env var already is.
#[test]
fn an_empty_value_in_the_file_is_absent_not_valid() {
    let env = TestEnv::new();
    env.write_transports(
        r#"
        [slack]
        app_token = "xapp-a"
        bot_token = ""
        "#,
    );

    let err = with_env_unset(|| load_tokens().expect_err("an empty token is not a token"));
    assert!(
        err.contains(BOT_TOKEN_VAR) || err.contains("bot_token"),
        "got: {err}"
    );
}
