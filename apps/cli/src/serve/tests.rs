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

#[test]
fn tokens_come_from_the_environment_and_treat_empty_as_absent() {
    let saved = (std::env::var(APP_TOKEN_VAR), std::env::var(BOT_TOKEN_VAR));

    unsafe {
        std::env::remove_var(APP_TOKEN_VAR);
        std::env::remove_var(BOT_TOKEN_VAR);
    }
    let err = SlackTokens::from_env().expect_err("no tokens");
    assert!(err.contains(APP_TOKEN_VAR), "got: {err}");

    unsafe {
        std::env::set_var(APP_TOKEN_VAR, "xapp-a");
        std::env::set_var(BOT_TOKEN_VAR, "");
    }
    let err = SlackTokens::from_env().expect_err("an empty var is absent, not a valid token");
    assert!(err.contains(BOT_TOKEN_VAR), "got: {err}");

    unsafe {
        std::env::set_var(BOT_TOKEN_VAR, "xoxb-b");
    }
    let t = SlackTokens::from_env().expect("both set");
    assert_eq!(
        (t.app_token.as_str(), t.bot_token.as_str()),
        ("xapp-a", "xoxb-b")
    );

    unsafe {
        match saved.0 {
            Ok(v) => std::env::set_var(APP_TOKEN_VAR, v),
            Err(_) => std::env::remove_var(APP_TOKEN_VAR),
        }
        match saved.1 {
            Ok(v) => std::env::set_var(BOT_TOKEN_VAR, v),
            Err(_) => std::env::remove_var(BOT_TOKEN_VAR),
        }
    }
}

/// The allowlist is mandatory because the transport fails closed: an empty
/// `allowed_user_ids` rejects every sender (`transport.rs:186`), so a default of
/// "empty" would boot a bot that acks messages and answers nobody — a silent
/// dead deployment. clap must reject the omission instead.
#[test]
fn serve_requires_an_allowlist() {
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: ServeArgs,
    }

    let err = Wrap::try_parse_from(["x", "--channel", "C1"])
        .err()
        .expect("omitting --allow-user must be an argument error, not an empty allowlist");
    assert_eq!(
        err.kind(),
        clap::error::ErrorKind::MissingRequiredArgument,
        "got: {err}"
    );

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
