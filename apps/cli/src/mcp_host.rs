//! MCP host stand-up for the CLI (`docs/rfcs/0003-mcp-host.md`).
//!
//! The desktop shell has hosted MCP servers since M4.2; `run`, `goal`, and `serve`
//! could not reach them, so a server configured in the GUI was invisible to every
//! headless entry point (#1207). This module is the CLI's equivalent stand-up.
//!
//! Two things differ from `apps/desktop/src-tauri/src/state.rs`:
//!
//! 1. **No runtime guard.** The desktop enters `tauri::async_runtime` explicitly
//!    because `init_mcp` can be called from a non-reactor thread. Every CLI caller is
//!    already inside `#[tokio::main]`, so entering a second handle would be wrong.
//! 2. **Deferred servers are skipped, not bridged.** See [`bridge_into`].

use std::path::Path;
use std::time::Duration;

use ff_core::McpServerState;
use ff_mcp::{McpConfigWatcher, SupervisorHandle};

/// How long [`bridge_into`] waits for servers to finish starting before bridging.
///
/// Servers are connected asynchronously, so a bridge that runs the instant the
/// supervisor spawns sees an empty tool snapshot and registers nothing — silently, with
/// no error anywhere. The desktop never hits this because it rebuilds its registry per
/// turn, long after startup; the CLI builds one registry per process, so it has to wait.
///
/// A ceiling rather than a target: the wait ends as soon as no awaited server is still
/// starting. Measured on a live config, a heavyweight server (37 tools) needed ~9s to
/// connect and publish, so a tighter ceiling would routinely drop it. It is rarely reached
/// because only servers whose tools would actually be *kept* are waited for — see
/// [`init`]. A server slower than this is named in a warning and skipped rather than
/// allowed to stall the run indefinitely.
const STARTUP_BUDGET: Duration = Duration::from_secs(15);

/// Classify a status snapshot into "keep waiting" vs "settled".
///
/// Split out from [`await_startup`] because it is the whole substance of the startup race
/// and the only part testable without a live supervisor: `spawn_supervisor` hands back a
/// handle whose status list is empty and fills it from a spawned task, so
/// `snapshot.is_empty()` means *"has not reconciled yet"* — not *"nothing to wait for"*.
/// Conflating the two makes the wait return instantly, the bridge see zero tools, and MCP
/// silently do nothing on every CLI run.
///
/// `Starting`/`Restarting` are transient; `Running`/`Failed`/`Disabled` are settled —
/// waiting on a `Failed` server would burn the whole budget for no gain.
fn settle_state(statuses: &[ff_core::McpServerStatus]) -> Settle {
    if statuses.is_empty() {
        return Settle::NotYetPublished;
    }
    let pending: Vec<String> = statuses
        .iter()
        .filter(|s| {
            matches!(
                s.state,
                McpServerState::Starting | McpServerState::Restarting
            )
        })
        .map(|s| s.id.clone())
        .collect();
    if pending.is_empty() {
        Settle::Settled
    } else {
        Settle::Pending(pending)
    }
}

/// Outcome of [`settle_state`].
#[derive(Debug, PartialEq, Eq)]
enum Settle {
    /// The supervisor has not published any status yet. Distinct from [`Self::Settled`]:
    /// treating this as settled is the startup race.
    NotYetPublished,
    /// Every server has reached a terminal state (`Running`/`Failed`/`Disabled`).
    Settled,
    /// These servers are still `Starting`/`Restarting`.
    Pending(Vec<String>),
}

/// Block until every awaited server has settled, or [`STARTUP_BUDGET`] elapses.
///
/// Returns the ids still starting when the budget ran out, so the caller can name the
/// servers whose tools are missing rather than leaving the user to guess. `expected` is
/// the count from [`init`]; zero means there is nothing worth waiting for and the wait is
/// skipped entirely.
async fn await_startup(handle: &SupervisorHandle, expected: usize) -> Vec<String> {
    if expected == 0 {
        return Vec::new();
    }
    let mut rx = handle.status_changed_rx();
    let deadline = tokio::time::Instant::now() + STARTUP_BUDGET;
    loop {
        let statuses = handle.status_snapshot();
        let settle = settle_state(&statuses);
        if settle == Settle::Settled {
            return Vec::new();
        }
        // `changed()` resolves only on a *new* publish, so a transition that already
        // happened cannot be observed — hence re-reading the snapshot each pass.
        if tokio::time::timeout_at(deadline, rx.changed())
            .await
            .is_err()
        {
            return match settle {
                Settle::Settled => Vec::new(),
                Settle::NotYetPublished => {
                    vec![format!("<{expected} server(s) never reported status>")]
                }
                Settle::Pending(pending) => pending,
            };
        }
    }
}

/// Stand up the config watcher and supervisor against `~/.flowforge/mcp.json` — the
/// same file the desktop watches, so servers configured in the GUI are picked up here
/// with no extra setup.
///
/// Fail-soft by contract (RFC 0003 §3, §5): a missing, unreadable, or malformed config
/// leaves MCP disabled and the CLI fully functional, exactly as it behaves today. The
/// `warn` carries the resolved path, because "my server isn't showing up" is otherwise
/// indistinguishable from "the file isn't where I think it is" (#1060).
///
/// Returns `None` when MCP could not be started; callers treat that as "no MCP tools".
/// On success, also returns how many servers [`bridge_into`] should wait for: those that
/// are enabled *and* non-deferred. The count is needed because an empty status snapshot
/// cannot distinguish "no servers" from "the supervisor has not reconciled yet".
pub fn init() -> Option<(SupervisorHandle, usize)> {
    let path = match ff_mcp::config_path() {
        Some(p) => p,
        None => {
            tracing::warn!("no home directory; MCP host disabled");
            return None;
        }
    };
    if !path.exists() {
        tracing::debug!(path = %path.display(), "no MCP config; MCP host disabled");
        return None;
    }
    let (watcher, shared, change_rx) = match McpConfigWatcher::spawn(path.clone()) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "MCP config watcher unavailable; MCP host disabled"
            );
            return None;
        }
    };
    let shared_for_count = shared.clone();
    let handle = ff_mcp::spawn_supervisor(shared, change_rx, ff_mcp::SupervisorConfig::default());
    // Only servers that are enabled *and* opted out of deferral are worth waiting for: a
    // deferred server's tools are discarded by `bridge_into` regardless, so blocking on one
    // would trade real startup latency for nothing. `defer` is a config field, known before
    // any process is spawned, which is what makes this decidable up front.
    let awaited = shared_for_count
        .read()
        .map(|c| {
            c.iter()
                .filter(|s| !s.disabled && s.defer == Some(false))
                .count()
        })
        .unwrap_or(0);
    // The watcher stops on drop, which would silently end hot-reload while leaving the
    // supervisor running. The CLI has no long-lived struct to park it in, so it is
    // leaked deliberately: its lifetime is the process, and `run`/`serve` both exit by
    // process teardown.
    std::mem::forget(watcher);
    tracing::info!(path = %path.display(), awaited, "MCP host started");
    Some((handle, awaited))
}

/// Bridge this session's MCP tools into `registry`.
///
/// **Deferred servers are skipped.** `defer` is `Option<bool>` where `None` means
/// deferred (`ff-mcp/src/config.rs:51`), so *unconfigured servers default to deferred*.
/// A deferred tool is only advertised once `tool_search` admits it
/// (`ff-agent/src/lib.rs`, RFC 0024 Layer 1) — and the CLI has no `tool_search`. Bridging
/// a deferred tool here would therefore register something the model can never see or
/// call: the user configures a server, the supervisor starts it, the bridge succeeds,
/// and nothing works, with no error anywhere. Skipping it and saying so is the honest
/// failure mode, and `defer: false` is a one-line opt-in.
///
/// Wiring `tool_search` into the CLI would lift this restriction and is deliberately
/// left out of scope here.
pub async fn bridge_into(
    handle: &SupervisorHandle,
    registry: &mut ff_tools::ToolRegistry,
    session_root: &Path,
    expected_servers: usize,
) {
    let still_starting = await_startup(handle, expected_servers).await;
    if !still_starting.is_empty() {
        tracing::warn!(
            servers = %still_starting.join(", "),
            budget_secs = STARTUP_BUDGET.as_secs(),
            "MCP servers did not finish starting within the budget; their tools are \
             missing from this run"
        );
    }
    // The count is deliberately dropped: it is already logged by `partition_and_register`,
    // and bridging is fail-soft, so no caller can act on it. Tests assert it on
    // `partition_and_register` directly.
    let _ = partition_and_register(ff_mcp::build_bridged_tools(handle, session_root), registry);
}

/// The policy half of [`bridge_into`], split out so it can be tested without a live
/// supervisor: `SupervisorHandle::for_test` is `#[cfg(test)]`-private to `ff-mcp`, so a
/// CLI test cannot construct a real bridged tool.
fn partition_and_register(
    tools: Vec<Box<dyn ff_tools::Tool>>,
    registry: &mut ff_tools::ToolRegistry,
) -> usize {
    let mut registered = 0usize;
    let mut deferred: Vec<String> = Vec::new();
    for tool in tools {
        if tool.defer() {
            deferred.push(tool.name().to_string());
            continue;
        }
        registry.register(tool);
        registered += 1;
    }
    if !deferred.is_empty() {
        deferred.sort();
        tracing::warn!(
            count = deferred.len(),
            tools = %deferred.join(", "),
            "MCP tools skipped: their server is deferred and the CLI has no tool_search \
             to admit them. Set \"defer\": false on the server in mcp.json to use them here."
        );
    }
    if registered > 0 {
        tracing::info!(count = registered, "MCP tools bridged");
    }
    registered
}

/// Per-server usage instructions for the servers whose tools the model can actually
/// reach this turn (#1173).
///
/// Simpler than the desktop's equivalent because there is no `tool_search` here: the
/// admitted set is always empty, so a server qualifies only by having at least one
/// standing (non-deferred) tool — which is precisely the set [`bridge_into`] registered.
/// Withholding instructions for a tool the model holds is the failure #1173 exists to
/// prevent, so these must be kept in step with each other.
pub fn guidance(handle: &SupervisorHandle) -> Vec<ff_agent::McpGuidance> {
    let standing: std::collections::HashSet<String> = handle
        .tools_snapshot()
        .into_iter()
        .filter(|t| !t.info.defer)
        .map(|t| t.key.id.clone())
        .collect();
    let no_admissions = std::collections::HashSet::new();
    let mut guidance: Vec<ff_agent::McpGuidance> = handle
        .instructions_snapshot()
        .into_iter()
        .filter(|(key, _)| {
            ff_agent::server_guidance_is_reachable(&key.id, &standing, &no_admissions)
        })
        .map(|(key, text)| ff_agent::McpGuidance {
            server: key.id.clone(),
            text,
        })
        .collect();
    guidance.sort_by(|a, b| a.server.cmp(&b.server));
    let (fitted, dropped) = ff_agent::fit_mcp_guidance(&guidance);
    if dropped > 0 {
        tracing::warn!(
            dropped,
            "MCP server guidance exceeded the injection budget; some servers' guidance \
             was omitted"
        );
    }
    fitted
}

/// Stops every MCP server when dropped, blocking until the actor acknowledges.
///
/// Without this, MCP servers **outlive the CLI**: `rmcp`'s `ChildWithCleanup::Drop` kills
/// the child via `tokio::spawn` (`rmcp-1.7.0/src/transport/child_process.rs:48`), and at
/// process exit the runtime is torn down before that task can run — so the child is never
/// signalled. Measured on a stub server: `flowforge run` exited and the child was still
/// alive afterwards. The desktop is not exposed to this because it calls `stop_all` from
/// its window-close handler (#1197) while its runtime is still live.
///
/// `Drop` rather than an explicit call at each exit point: `run` has several early
/// `return ExitCode::FAILURE` paths, and a leaked server process on the error path is
/// exactly the case a manual call would miss.
pub struct McpTeardown {
    handle: Option<SupervisorHandle>,
}

impl McpTeardown {
    pub(crate) fn new(handle: SupervisorHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }
}

impl Drop for McpTeardown {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        // `Drop` cannot be async, and the actor being awaited lives on *this* runtime, so
        // the wait cannot be moved to a thread with its own runtime. `block_in_place`
        // releases the current worker so the runtime keeps driving that actor.
        //
        // It panics on a current-thread runtime, so the flavour is checked rather than
        // assumed: `#[tokio::main]` is multi-thread today, but a future `flavor =
        // "current_thread"` would otherwise turn orderly shutdown into a panic during
        // unwind. On current-thread the servers are left to the OS — the same outcome as
        // before this guard existed, minus the panic.
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("no runtime at MCP teardown; server children may be orphaned");
            return;
        };
        if rt.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
            tracing::warn!(
                "current-thread runtime: skipping blocking MCP teardown; server children \
                 may be orphaned"
            );
            return;
        }
        let timed_out = tokio::task::block_in_place(|| {
            rt.block_on(async { tokio::time::timeout(SHUTDOWN_BUDGET, handle.stop_all()).await })
        })
        .is_err();
        if timed_out {
            tracing::warn!(
                budget_secs = SHUTDOWN_BUDGET.as_secs(),
                "MCP shutdown exceeded its budget; a server child may outlive this process"
            );
        }
    }
}

/// Ceiling on MCP teardown. `stop_all` is a *sequential* loop and each server gets its own
/// `SHUTDOWN_TIMEOUT` (2s, `ff-mcp/src/supervisor.rs:89`), so this is deliberately above a
/// single server's timeout — a budget below it would cut the actor off mid-shutdown and
/// reintroduce the orphan it exists to prevent. Measured: a healthy server stops in ~14ms.
/// With many wedged servers the budget still wins and the CLI orphans rather than hangs.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests;
