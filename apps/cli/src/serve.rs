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
    /// (e.g. `--allow-user U123,U456`). Overrides `allowed_users` under
    /// `[slack]` in `transports.toml`.
    ///
    /// Not a clap `required`, because the file is an equally valid source — but
    /// it still cannot be skipped in *both* places: the transport fails closed,
    /// and an empty allowlist rejects *every* sender (`transport.rs:186`), so a
    /// bot configured with neither acks messages and silently answers nobody.
    /// [`serve`] rejects that combination at startup rather than letting it boot.
    #[arg(long, value_delimiter = ',')]
    pub allow_user: Vec<String>,
}

/// The two Slack tokens, resolved from `transports.toml` with the environment
/// as an override.
///
/// Split out from [`serve`] so the resolution rule and its failure message can be
/// asserted without a Slack connection.
#[derive(Debug, Clone)]
pub struct SlackTokens {
    pub app_token: String,
    pub bot_token: String,
}

impl SlackTokens {
    /// Resolve both tokens from `transports.toml`, with the environment as an
    /// override (#1060 scope bullet 2).
    ///
    /// The file is the documented source; the env vars stay because a container
    /// or CI deployment must be able to supply a credential without baking it
    /// into a file on disk. This is the same layering `host.rs` uses for the
    /// provider config, where the per-connection value wins and the env var is
    /// the global override.
    ///
    /// An empty value is treated as absent from *either* source: an empty
    /// string in a checked-in file is a half-finished edit, not a credential,
    /// and silently booting with it would fail later at the Slack handshake
    /// with a far less obvious error.
    /// Resolve from an already-parsed `[slack]` section.
    ///
    /// Takes the section rather than reading the file itself so [`serve`] does
    /// not parse `transports.toml` twice — it needs `allowed_users` from the
    /// same read.
    pub fn from_parts(slack: &SlackConfig) -> Result<Self, String> {
        Ok(Self {
            app_token: resolve_token(APP_TOKEN_VAR, "app_token", slack.app_token.clone())?,
            bot_token: resolve_token(BOT_TOKEN_VAR, "bot_token", slack.bot_token.clone())?,
        })
    }
}

/// The `[slack]` section of `transports.toml`. Unknown keys are rejected so a
/// typo in a credential key surfaces as an error rather than as a silently
/// missing token.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackConfig {
    pub app_token: Option<String>,
    pub bot_token: Option<String>,
    /// Slack user IDs allowed to drive the session. `--allow-user` overrides
    /// this; the transport fails closed when the result is empty.
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportsConfig {
    pub slack: Option<SlackConfig>,
}

impl TransportsConfig {
    /// Read and parse `transports.toml`, treating an absent file as an empty
    /// config — the env-var-only deployment is legitimate, so a missing file is
    /// not itself an error. A *malformed* file is, since silently ignoring it
    /// would present as "token not set" and send the reader to the wrong place.
    pub fn read() -> Result<Self, String> {
        let Some(path) = transports_path() else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => toml::from_str(&raw)
                .map_err(|e| format!("{} is not valid TOML: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }
}

/// Path to `transports.toml`, resolved through the same `config_dir()`
/// indirection `host.rs:177` uses so the `TestEnv` override applies here too.
fn transports_path() -> Option<std::path::PathBuf> {
    let config_dir = {
        #[cfg(test)]
        {
            crate::test_support::config_dir_override().or_else(dirs::config_dir)
        }
        #[cfg(not(test))]
        {
            dirs::config_dir()
        }
    }?;
    // The override is the *config dir*, not its `flowforge/` subdir, so the
    // subdir is appended here exactly as `registry_path()` does.
    Some(config_dir.join("flowforge").join("transports.toml"))
}

fn resolve_token(var: &str, key: &str, from_file: Option<String>) -> Result<String, String> {
    std::env::var(var)
        .ok()
        .or(from_file)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "no Slack {key}: set `{key}` under [slack] in transports.toml, \
                 or export {var}"
            )
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
    let config = TransportsConfig::read()?;
    let slack = config.slack.unwrap_or_default();
    let tokens = SlackTokens::from_parts(&slack)?;
    let channel = ChannelId::new(TRANSPORT_NAME, &args.channel);
    let mode: ff_core::Mode = args.mode.into();

    // `--allow-user` wins; the file supplies the default. Either way the
    // transport fails closed on an empty result, so an operator who configures
    // neither is refused rather than exposed.
    let allowed_users = if args.allow_user.is_empty() {
        slack.allowed_users.clone()
    } else {
        args.allow_user.clone()
    };
    if allowed_users.is_empty() {
        return Err(
            "no allowed users: pass --allow-user U123,U456 or set `allowed_users` \
             under [slack] in transports.toml. The transport fails closed, so \
             booting with an empty allowlist would ack every message and answer \
             nobody."
                .to_string(),
        );
    }
    let allowlist_size = allowed_users.len();
    let mut transport = SlackTransport::new(&tokens.app_token, &tokens.bot_token)
        .with_allowed_user_ids(allowed_users);
    #[cfg(test)]
    if let Some(base) = test_seams::api_base() {
        transport = transport.with_api_base(base);
    }

    // The approver can only be built from a `Connected`, so this cannot be
    // reordered without a compile error.
    let mut connected = Connected::open(&mut transport).await?;
    let api = SlackApi::new(&tokens.bot_token);
    #[cfg(test)]
    let api = if let Some(base) = test_seams::api_base() {
        api.with_base(base)
    } else {
        api
    };
    let approver = connected.approver(api, channel, mode, PermissionMatrix::default())?;

    // The teardown guard is held for the whole serve loop: dropping it earlier would
    // stop the MCP servers out from under in-flight turns (#1207).
    let (mut router, _mcp_teardown) = build_router(mode).await;

    eprintln!(
        "serving Slack channel {} in {mode:?} mode for {} allowlisted user(s)",
        args.channel, allowlist_size
    );

    // Ctrl-C stops the loop gracefully: the handle closes the inbound side, so an
    // in-flight turn finishes and `run` then returns on its own. `ctrl_c()` is
    // portable across Unix and Windows, matching `main.rs:589`.
    //
    // A second Ctrl-C is left to the default handler on purpose — if the first
    // one is waiting on a wedged turn, the operator needs a way out that does not
    // depend on this process behaving.
    let shutdown = transport.shutdown_handle();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("shutting down: finishing the current turn, then stopping");
            shutdown.shutdown();
        }
    });

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
async fn build_router(mode: ff_core::Mode) -> (Router, Option<crate::mcp_host::McpTeardown>) {
    #[cfg(test)]
    if let Some((provider, model, registry)) = test_seams::take_host() {
        return assemble_router(
            mode,
            provider,
            model,
            registry,
            Arc::new(host::build_session_store(false)),
            None,
        );
    }
    let (provider, model) = host::load_provider();
    let store = host::build_session_store(false);
    let (registry, _memory, _keys, _guidance, mcp_teardown) =
        crate::build_registry_with_mcp().await;
    assemble_router(
        mode,
        Arc::from(provider),
        model,
        Arc::new(registry),
        Arc::new(store),
        mcp_teardown,
    )
}

/// The shared `Router::new` invocation, split out so the `cfg(test)` seam can
/// inject provider + registry without duplicating the wiring.
fn assemble_router(
    mode: ff_core::Mode,
    provider: Arc<dyn ff_llm::Provider>,
    model: String,
    registry: Arc<ff_tools::ToolRegistry>,
    store: Arc<ff_session::SessionStore>,
    mcp_teardown: Option<crate::mcp_host::McpTeardown>,
) -> (Router, Option<crate::mcp_host::McpTeardown>) {
    let router = Router::new(
        RouterConfig {
            mode,
            workspace: host::workspace_root(),
            model,
            ..RouterConfig::default()
        },
        channel_map(),
        store,
        registry,
        provider,
    );
    (router, mcp_teardown)
}

#[cfg(test)]
mod test_seams {
    //! Test-only knobs (T6, #1061) for booting `serve` against a mock Slack and
    //! a scripted model. Each booted test sets its own seam and the assembly
    //! below consumes it; there is no production path onto these.
    //!
    //! `nextest` isolates each test in its own process, so a shared static is
    //! safe here — the "serialise the tests" concern that motivated
    //! `test_support::MEM_STORE_LOCK` does not arise.

    use std::sync::{Arc, Mutex};

    use ff_llm::Provider;
    use ff_tools::ToolRegistry;

    /// Web API + `apps.connections.open` base for the Slack transport/approver
    /// to dial (the mock's HTTP server).
    static API_BASE: Mutex<Option<String>> = Mutex::new(None);

    /// The provider + model + registry the Router would otherwise build from the
    /// real provider registry / `~/.flowforge` MCP config.
    type HostSeam = (Arc<dyn Provider>, String, Arc<ToolRegistry>);
    static HOST: Mutex<Option<HostSeam>> = Mutex::new(None);

    pub(crate) fn set_api_base(base: impl Into<String>) {
        *API_BASE.lock().unwrap() = Some(base.into());
    }

    pub(crate) fn api_base() -> Option<String> {
        API_BASE.lock().unwrap().clone()
    }

    pub(crate) fn set_host(
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
        registry: Arc<ToolRegistry>,
    ) {
        *HOST.lock().unwrap() = Some((provider, model.into(), registry));
    }

    pub(crate) fn take_host() -> Option<(Arc<dyn Provider>, String, Arc<ToolRegistry>)> {
        HOST.lock().unwrap().take()
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_t6;
