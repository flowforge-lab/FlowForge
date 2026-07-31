//! `flowforge serve` — boot the Slack transport and its approver into the
//! [`Router`] (#1060 T5, RFC 0021 §6).
//!
//! T3 (#1058) built the transport and T4 (#1059) the approver, but nothing ever
//! constructed them together: `Router::run`'s only caller was a test, so Slack
//! approval was unreachable in a real process. This is that assembly.
//!
//! Single channel by design. [`SlackApprover`] binds one [`ChannelId`] at
//! construction (one approver per session, mirroring `UiApprover`), while
//! `Router::run` takes one `&dyn Approver` for every channel it serves. Serving
//! several channels interactively needs the channel on `Approver::approve` — a
//! trait change touching `UiApprover` and `CliApprover` — so it is deliberately
//! out of scope here (#912 T6).

use std::sync::Arc;

use clap::Args;
use ff_core::PermissionMatrix;
use ff_transport::{ChannelId, ChannelMap, MessageTransport, Router, RouterConfig};
use ff_transport_slack::{SlackApi, SlackApprover, SlackTransport, TRANSPORT_NAME};

use crate::host;
use crate::ModeArg;

/// Env var holding the app-level token (`xapp-…`) used for Socket Mode.
pub const APP_TOKEN_VAR: &str = "SLACK_APP_TOKEN";
/// Env var holding the bot token (`xoxb-…`) used for Web API calls.
pub const BOT_TOKEN_VAR: &str = "SLACK_BOT_TOKEN";

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Slack channel ID to serve (e.g. C0123456789).
    #[arg(long)]
    pub channel: String,

    /// Permission mode for the served session.
    #[arg(long, value_enum, default_value_t = ModeArg::Auto)]
    pub mode: ModeArg,

    /// Slack user IDs allowed to drive the session, comma-separated
    /// (e.g. `--allow-user U123,U456`).
    ///
    /// Required, and required for a reason: the transport fails closed — an empty
    /// allowlist rejects *every* sender (`transport.rs:186`), so defaulting it to
    /// empty would start a bot that acks messages and silently answers nobody.
    /// Making it mandatory turns that into an argument error at startup.
    #[arg(long, value_delimiter = ',', required = true)]
    pub allow_user: Vec<String>,
}

/// The two Slack tokens, read from the environment.
///
/// Split out from [`serve`] so the resolution rule and its failure message can be
/// asserted without a Slack connection.
#[derive(Debug, Clone)]
pub struct SlackTokens {
    pub app_token: String,
    pub bot_token: String,
}

impl SlackTokens {
    /// Read both tokens from the environment, treating an empty var as absent —
    /// the same rule `host.rs` applies to provider keys.
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            app_token: require_var(APP_TOKEN_VAR)?,
            bot_token: require_var(BOT_TOKEN_VAR)?,
        })
    }
}

fn require_var(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!("{name} is not set; a Slack app-level and bot token are both required")
        })
}

/// A transport that has completed `connect`.
///
/// This exists to make the assembly order a *compile-time* property rather than a
/// convention. `take_interaction_rx` returns `None` until `connect` creates the
/// channel (`transport.rs:265`), and taking it early is neither a compile error
/// nor a runtime error in the transport — it silently yields an approver that
/// never observes a click and therefore times out, i.e. denies, every prompt.
/// A comment asking the next reader to keep two statements in order is not a
/// guard; a type they cannot obtain out of order is.
///
/// The inner field is wrapped in a private module so it cannot be built by
/// tuple-construction even from inside this file — without that, `Connected(&mut
/// t)` right here would defeat the whole guarantee, which is exactly the mistake
/// this type is meant to prevent.
pub use connected::Connected;

mod connected {
    use super::*;

    pub struct Connected<'t> {
        transport: &'t mut SlackTransport,
    }

    impl<'t> Connected<'t> {
        /// Connect `transport`, then hand back the proof that it is connected.
        pub async fn open(transport: &'t mut SlackTransport) -> Result<Self, String> {
            transport
                .connect()
                .await
                .map_err(|e| format!("Slack connect failed: {e}"))?;
            Ok(Self { transport })
        }

        /// Wrap a transport without connecting it, so [`approver`](Self::approver)
        /// is reachable in tests without dialling a real WebSocket.
        ///
        /// `cfg(test)` on purpose: this is the one way to defeat the ordering
        /// guarantee, and it must not exist in a production build.
        #[cfg(test)]
        pub fn assume(transport: &'t mut SlackTransport) -> Self {
            Self { transport }
        }

        /// Build the approver for `channel`, consuming the transport's interaction
        /// stream.
        ///
        /// Still fallible: the receiver is single-consumer, so a second call has
        /// to fail rather than silently split clicks between two approvers, each
        /// resolving an arbitrary half of the prompts.
        pub fn approver(
            &mut self,
            api: SlackApi,
            channel: ChannelId,
            mode: ff_core::Mode,
            matrix: PermissionMatrix,
        ) -> Result<SlackApprover, String> {
            let interactions = self.transport.take_interaction_rx().ok_or_else(|| {
                "interaction receiver already taken: one approver drains it, and a second \
                 would see an arbitrary half of the clicks"
                    .to_string()
            })?;
            Ok(SlackApprover::new(api, channel, mode, matrix, interactions))
        }
    }
}

/// Run the router over Slack until the transport closes.
pub async fn run(args: ServeArgs) -> std::process::ExitCode {
    match serve(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The fallible core of [`run`], kept separate so assembly failures are
/// assertable without a process exit.
async fn serve(args: ServeArgs) -> Result<(), String> {
    let tokens = SlackTokens::from_env()?;
    let channel = ChannelId::new(TRANSPORT_NAME, &args.channel);
    let mode: ff_core::Mode = args.mode.into();

    let mut transport = SlackTransport::new(&tokens.app_token, &tokens.bot_token)
        .with_allowed_user_ids(args.allow_user.clone());

    // The approver can only be built from a `Connected`, so this cannot be
    // reordered without a compile error.
    let mut connected = Connected::open(&mut transport).await?;
    let approver = connected.approver(
        SlackApi::new(&tokens.bot_token),
        channel,
        mode,
        PermissionMatrix::default(),
    )?;

    let mut router = build_router(mode);

    eprintln!(
        "serving Slack channel {} in {mode:?} mode for {} allowlisted user(s)",
        args.channel,
        args.allow_user.len()
    );

    router.run(&mut transport, &approver).await;
    Ok(())
}

/// The channel→session map, persisted next to the session DB when a config dir
/// resolves. Falling back to an in-memory map mirrors `build_session_store`'s
/// ephemeral branch: the bot still works, it just re-creates the session next
/// boot rather than crashing on an unwritable home.
fn channel_map() -> ChannelMap {
    match host::channel_map_path() {
        Some(p) => ChannelMap::open(p),
        None => ChannelMap::new(),
    }
}

/// Assemble the [`Router`] from the same seams the other headless entry points
/// use, so `serve` cannot drift from `flowforge run` on provider, tools or
/// session storage.
fn build_router(mode: ff_core::Mode) -> Router {
    let (provider, model) = host::load_provider();
    let store = host::build_session_store(false);
    let (registry, _memory, _keys) = crate::build_registry_with_memory();

    Router::new(
        RouterConfig {
            mode,
            workspace: host::workspace_root(),
            model,
            ..RouterConfig::default()
        },
        channel_map(),
        Arc::new(store),
        Arc::new(registry),
        Arc::from(provider),
    )
}

#[cfg(test)]
mod tests;
