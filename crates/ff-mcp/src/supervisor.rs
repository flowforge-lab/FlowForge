//! Lifecycle supervisor for MCP servers (RFC 0003 §5).
//!
//! A single async actor task owns a map of [`ServerHandle`]s keyed by server id and
//! drives each through the [`McpServerState`] transitions:
//!
//! ```text
//!   (none) ──Start──▶ Starting ──ok──▶ Running ──crash──▶ Restarting ──retry──▶ Starting
//!                          │                                  │
//!                          └──connect-err / health-fail──▶ Restarting ──`max_failures`──▶ Failed
//! ```
//!
//! Why an actor instead of `Arc<Mutex<…>>`: a tool call (M4.3) and a hot-reload swap
//! (M4.1) can land in the same instant, and both want to touch the same handle. Funnel
//! everything through one task and there's no lock-ordering question — the run loop is
//! the serialization point. The handle the rest of the app holds is a cheap
//! `mpsc::Sender` plus a read-only `SharedStatus`.
//!
//! Liveness / health (RFC 0003 §5): every `health_interval` the supervisor calls
//! `list_tools` on each `Running` server. A server that has exited — cleanly on idle
//! (e.g. codegraph) or by crash — has a dead transport, so the probe fails; that failure
//! is treated like a crash: the connection is dropped, `failures` is bumped, and the
//! server moves to `Restarting`. A server that had been Running for at least
//! `min_healthy_uptime` then has its `failures`/`backoff` cleared on the next tick (it
//! has *proven* healthy), so an idle-exiting server reconnects and is never parked in
//! `Failed`; one that keeps exiting before `min_healthy_uptime` is flapping — its
//! `failures` accumulate and, once `failures >= max_failures`, it is parked in `Failed`
//! and not retried until the config is reloaded (saving CPU on a permanently-broken
//! server, e.g. a wrong command).
//!
//! Recovery latency is bounded by `health_interval`: rmcp 1.7 exposes no non-consuming
//! child-exit signal to detect a close sooner (`RunningService::is_closed` does not flip
//! on a stdio child exit, and `waiting()` consumes the service the supervisor keeps for
//! tool calls).
//!
//! Backoff: capped exponential ([`Backoff`]). A successful connect resets it.
//!
//! Shutdown: [`SupervisorHandle::stop_all`] cancels every live `McpClient` and drops
//! the map. `rmcp`'s `ChildWithCleanup` kills any straggler on drop, but only if the
//! Tokio runtime is still alive — the desktop wrapper invokes `stop_all` from the
//! Tauri `RunEvent::ExitRequested` hook so reaping completes before exit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use ff_core::{McpScope, McpServerConfig, McpServerState, McpServerStatus, McpToolInfo};
use tokio::sync::{mpsc, oneshot, watch};

use crate::backoff::Backoff;
use crate::client::McpClient;
use crate::key::{InstanceKey, ScopeKey};
use crate::reconcile::{reconcile, ReconcileAction};
use crate::watch::SharedConfig;

/// Read-only snapshot the UI subscribes to (M4.4). The supervisor swaps a freshly
/// rebuilt vec on every state change; readers never block on the actor.
pub type SharedStatus = Arc<RwLock<Vec<McpServerStatus>>>;

/// One advertised tool stamped with the [`InstanceKey`] of the instance that serves
/// it, so the per-turn bridge can route a call to the right instance under concurrent
/// workspace sessions (RFC 0018 §4.6).
#[derive(Clone, Debug)]
pub struct PublishedTool {
    pub key: InstanceKey,
    pub info: McpToolInfo,
}

/// The flat list of every `Running` server's tools, shared with the desktop shell so
/// it can compose a per-turn [`ToolRegistry`](ff_tools::ToolRegistry) (M4.3 bridge).
/// Each entry carries its serving instance's key for per-turn routing (RFC 0018 §4.6).
/// Rebuilt by the actor whenever the running tool set changes; readers never block.
pub type SharedTools = Arc<RwLock<Vec<PublishedTool>>>;

/// How long a single graceful `shutdown` may take before we give up and drop the
/// client, letting `process_wrap`'s kill-on-drop reap the child. Bounds app-exit
/// latency so one wedged server can't stall the whole quit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound on one bridged tool call. A call is routed through the actor (which
/// owns the client), so an unbounded call would also stall supervision and app exit;
/// the timeout caps both. Generous because legitimate MCP tools (network, subprocess)
/// can be slow, but finite so a wedged server can't hang the actor forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Supervisor tunables. [`Default`] picks production-friendly values; integration tests
/// override with shorter timings.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Resolution of the periodic state-machine pass (retry due, health probe due).
    pub tick: Duration,
    /// Minimum interval between `list_tools` health probes per server.
    pub health_interval: Duration,
    /// Initial restart delay; doubles on each consecutive failure.
    pub backoff_base: Duration,
    /// Cap on the restart delay.
    pub backoff_max: Duration,
    /// Consecutive failures before a server is parked in `Failed`.
    pub max_failures: u32,
    /// How long a server must stay `Running` before a transport close is treated as a
    /// recoverable idle/clean exit (restart without penalty) rather than flapping
    /// (count toward `max_failures`). Guards against a hot restart loop on a server
    /// that exits immediately on every start.
    pub min_healthy_uptime: Duration,
    /// Host environment variables passed through to children. Anything outside this
    /// list (and the server's declared `env`) is stripped — see [`McpClient::connect`].
    pub env_allowlist: Vec<String>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        // Minimum so a bare command name resolves and `~`-style configs work.
        #[cfg(unix)]
        let allow = vec!["PATH".into(), "HOME".into()];
        #[cfg(windows)]
        let allow = vec![
            "PATH".into(),
            "SystemRoot".into(),
            "USERPROFILE".into(),
            "APPDATA".into(),
        ];
        Self {
            tick: Duration::from_secs(1),
            health_interval: Duration::from_secs(30),
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            max_failures: 5,
            min_healthy_uptime: Duration::from_secs(10),
            env_allowlist: allow,
        }
    }
}

/// Caller-side handle. Cheap to clone (the channel is) and `Send + Sync`.
#[derive(Clone)]
pub struct SupervisorHandle {
    cmd_tx: mpsc::Sender<Cmd>,
    /// The latest status snapshot, kept up to date by the actor.
    pub status: SharedStatus,
    /// The flat tool list across all `Running` servers, kept current by the actor.
    pub tools: SharedTools,
    /// Ticked (coalescing) by the actor on every `publish`, so the desktop shell can
    /// forward a `mcp:status-changed` event without polling. Carries no data — readers
    /// re-snapshot via [`status_snapshot`](Self::status_snapshot).
    status_rx: watch::Receiver<()>,
    /// Sticky `true`-latch flipped by [`stop_all`](Self::stop_all) before it queues
    /// the `StopAll` command, so an in-flight `do_call_tool` await is preempted and the
    /// actor reaches the stop quickly instead of stalling up to `CALL_TIMEOUT` (#119).
    cancel_tx: Arc<watch::Sender<bool>>,
}

impl SupervisorHandle {
    /// A minimal, inert handle for unit tests that only need a `SupervisorHandle`
    /// value (e.g. constructing a `McpBridgedTool` to assert its safety
    /// classification). The channels/watches are live but never driven.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let (_status_tx, status_rx) = watch::channel(());
        let (cancel_tx, _cancel_rx) = watch::channel(false);
        Self {
            cmd_tx,
            status: SharedStatus::default(),
            tools: SharedTools::default(),
            status_rx,
            cancel_tx: Arc::new(cancel_tx),
        }
    }

    /// Ask the supervisor to re-snapshot the shared config and apply any deltas. The
    /// watcher already pings on file change; this is for callers that mutate
    /// `SharedConfig` programmatically (tests).
    pub async fn reconcile_now(&self) {
        let _ = self.cmd_tx.send(Cmd::Reconcile).await;
    }

    /// A snapshot of the currently advertised tools across all `Running` servers.
    /// Cheap read (clone of the shared vec under a read lock).
    pub fn tools_snapshot(&self) -> Vec<PublishedTool> {
        self.tools.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A snapshot of every server's status, id-sorted. Cheap read (clone of the shared
    /// vec under a read lock); poison-tolerant like [`tools_snapshot`](Self::tools_snapshot).
    pub fn status_snapshot(&self) -> Vec<McpServerStatus> {
        self.status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// A receiver that fires whenever the supervisor republishes status (start/stop/
    /// restart/health change, or a reconcile). Coalescing: a burst of changes may wake
    /// the receiver once — re-read via [`status_snapshot`](Self::status_snapshot). The
    /// desktop shell drives a `mcp:status-changed` event off this.
    pub fn status_changed_rx(&self) -> watch::Receiver<()> {
        self.status_rx.clone()
    }

    /// Route a tool call through the supervisor actor to the specified server.
    /// Returns the text content the model sees, or an error if the server is not
    /// running / the call failed / timed out.
    pub async fn call_tool(
        &self,
        key: &InstanceKey,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<String, crate::McpError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = Cmd::CallTool {
            key: key.clone(),
            tool: tool.to_string(),
            args,
            reply: reply_tx,
        };
        if self.cmd_tx.send(cmd).await.is_err() {
            return Err(crate::McpError::Protocol(
                "supervisor actor has exited".into(),
            ));
        }
        reply_rx.await.unwrap_or_else(|_| {
            Err(crate::McpError::Protocol(
                "supervisor dropped the reply channel".into(),
            ))
        })
    }

    /// Drive an immediate restart of one server, bypassing the backoff schedule.
    /// Unlike auto-recovery this also revives a server parked in `Failed`, so it backs
    /// a UI "Restart" button. Fire-and-forget: the new status arrives via the shared
    /// snapshot. Unknown ids are a no-op.
    pub async fn restart(&self, id: &str) {
        let _ = self.cmd_tx.send(Cmd::Restart { id: id.to_string() }).await;
    }

    /// Align the supervisor's live instance set for `session_id`, now rooted at
    /// `root`: the session references one workspace instance per `Workspace`-scoped
    /// server in the watched config (RFC 0018 §4.3). The supervisor adds the session to
    /// those instances' ref-lists, drops it from any it no longer references (evicting a
    /// workspace instance whose ref-list empties), and proactively (re)starts each
    /// referenced instance that is not `Running` -- the one place a codegraph parked in
    /// `Failed` is revived for a new turn (RFC 0018 §4.5, #557 Finding 2). Awaits until
    /// applied so the caller can snapshot tools right after.
    pub async fn align_session(
        &self,
        session_id: &str,
        root: PathBuf,
        servers: Vec<McpServerConfig>,
    ) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(Cmd::SetSessionRoot {
                session_id: session_id.to_string(),
                root,
                servers,
                ack: ack_tx,
            })
            .await
            .is_err()
        {
            return;
        }
        let _ = ack_rx.await;
    }

    /// Drop `session_id` from every workspace instance's ref-list, evicting any whose
    /// ref-list empties (RFC 0018 §4.3). Called when a session is closed/deleted so a
    /// per-workspace codegraph is reaped once no live session references its path.
    pub async fn release_session(&self, session_id: &str) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(Cmd::ReleaseSession {
                session_id: session_id.to_string(),
                ack: ack_tx,
            })
            .await
            .is_err()
        {
            return;
        }
        let _ = ack_rx.await;
    }

    /// Stop every server and exit the actor. Returns once all graceful-close calls
    /// have completed (or timed out) so the caller can let the Tokio runtime wind
    /// down with no children still waiting to be reaped.
    pub async fn stop_all(&self) {
        // Preempt any in-flight tool call before queuing StopAll: `do_call_tool`
        // races this latch, so the actor abandons the call and reaches the stop in
        // ~SHUTDOWN_TIMEOUT instead of stalling up to CALL_TIMEOUT (#119).
        let _ = self.cancel_tx.send(true);
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::StopAll(ack_tx)).await.is_err() {
            return;
        }
        let _ = ack_rx.await;
    }
}

enum Cmd {
    Reconcile,
    Restart {
        id: String,
    },
    SetSessionRoot {
        session_id: String,
        root: PathBuf,
        servers: Vec<McpServerConfig>,
        ack: oneshot::Sender<()>,
    },
    ReleaseSession {
        session_id: String,
        ack: oneshot::Sender<()>,
    },
    StopAll(oneshot::Sender<()>),
    CallTool {
        key: InstanceKey,
        tool: String,
        args: serde_json::Value,
        reply: oneshot::Sender<Result<String, crate::McpError>>,
    },
}

struct ServerHandle {
    /// The instance key this handle is filed under (RFC 0018 §4.2): `(id, scope)`. A
    /// workspace instance carries its canonical root here, used at connect for the MCP
    /// root and the child cwd.
    key: InstanceKey,
    /// The config the server was *started with*. `reconcile` compares against this so
    /// an unchanged definition produces no action.
    config: McpServerConfig,
    client: Option<McpClient>,
    state: McpServerState,
    /// Full tool list from the last successful `list_tools` call. Empty while the
    /// server is not Running.
    tools: Vec<McpToolInfo>,
    pid: Option<u32>,
    last_error: Option<String>,
    restarts: u32,
    failures: u32,
    backoff: Backoff,
    /// When the supervisor should next attempt a connect. `None` while running or
    /// while parked in `Failed`.
    next_retry_at: Option<Instant>,
    last_health_check: Option<Instant>,
    /// When the server last entered `Running`. Used to decide whether a transport
    /// close is a recoverable idle/clean exit (healthy long enough) or flapping.
    running_since: Option<Instant>,
}

impl ServerHandle {
    fn new(key: InstanceKey, config: McpServerConfig, sup: &SupervisorConfig) -> Self {
        let disabled = config.disabled;
        Self {
            key,
            config,
            client: None,
            state: if disabled {
                McpServerState::Disabled
            } else {
                McpServerState::Starting
            },
            tools: Vec::new(),
            pid: None,
            last_error: None,
            restarts: 0,
            failures: 0,
            backoff: Backoff::new(sup.backoff_base, sup.backoff_max),
            next_retry_at: None,
            last_health_check: None,
            running_since: None,
        }
    }

    fn snapshot(&self) -> McpServerStatus {
        McpServerStatus {
            id: self.config.id.clone(),
            state: self.state,
            tool_count: self.tools.len(),
            last_error: self.last_error.clone(),
            restarts: self.restarts,
            pid: self.pid,
            scope_key: self.key.scope.display(),
        }
    }
}

/// Spawn the supervisor actor and return a handle. The shared config is the one the
/// watcher keeps current; the change receiver is its post-reload signal.
pub fn spawn(
    shared_config: SharedConfig,
    change_rx: mpsc::UnboundedReceiver<()>,
    config: SupervisorConfig,
) -> SupervisorHandle {
    let status: SharedStatus = Arc::new(RwLock::new(Vec::new()));
    let tools: SharedTools = Arc::new(RwLock::new(Vec::new()));
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(16);
    let (status_tx, status_rx) = watch::channel(());
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let actor = Supervisor {
        config,
        handles: BTreeMap::new(),
        ws_refs: BTreeMap::new(),
        shared_config,
        status: Arc::clone(&status),
        tools: Arc::clone(&tools),
        status_tx,
        cmd_rx,
        change_rx,
        cancel_rx,
    };
    tokio::spawn(actor.run());
    SupervisorHandle {
        cmd_tx,
        status,
        tools,
        status_rx,
        cancel_tx: Arc::new(cancel_tx),
    }
}

struct Supervisor {
    config: SupervisorConfig,
    handles: BTreeMap<InstanceKey, ServerHandle>,
    /// Per workspace-scoped instance, the set of live session ids referencing it
    /// (RFC 0018 §4.3). A workspace instance is evicted when its set empties. Global
    /// instances are not tracked here -- they are always-on, driven by `reconcile`.
    ws_refs: BTreeMap<InstanceKey, BTreeSet<String>>,
    shared_config: SharedConfig,
    status: SharedStatus,
    tools: SharedTools,
    /// Coalescing change tick: sent on every `publish` so the desktop shell can forward
    /// a `mcp:status-changed` event without polling.
    status_tx: watch::Sender<()>,
    cmd_rx: mpsc::Receiver<Cmd>,
    change_rx: mpsc::UnboundedReceiver<()>,
    /// Receiver side of the quit latch. `do_call_tool` races this so an in-flight
    /// call is abandoned the moment `stop_all` flips it (#119).
    cancel_rx: watch::Receiver<bool>,
}

impl Supervisor {
    async fn run(mut self) {
        // Initial reconcile picks up whatever was loaded at boot.
        self.reconcile().await;

        let mut ticker = tokio::time::interval(self.config.tick);
        // Defensive: if a tick is missed (e.g. a slow connect held the loop), don't
        // burst-fire to catch up.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ticker.tick() => self.on_tick().await,
                Some(()) = self.change_rx.recv() => self.reconcile().await,
                Some(cmd) = self.cmd_rx.recv() => match cmd {
                    Cmd::Reconcile => self.reconcile().await,
                    Cmd::Restart { id } => self.restart(&id).await,
                    Cmd::SetSessionRoot { session_id, root, servers, ack } => {
                        self.set_session_root(session_id, root, servers).await;
                        let _ = ack.send(());
                    }
                    Cmd::ReleaseSession { session_id, ack } => {
                        self.release_session(session_id).await;
                        let _ = ack.send(());
                    }
                    Cmd::StopAll(ack) => {
                        self.stop_all().await;
                        let _ = ack.send(());
                        return;
                    }
                    Cmd::CallTool { key, tool, args, reply } => {
                        let result = self.do_call_tool(&key, &tool, args).await;
                        let _ = reply.send(result);
                    }
                },
                else => return,
            }
        }
    }

    /// Reconcile the **global-tier** instances against the watched config (RFC 0018
    /// §4.1). Global servers are always-on as before; `Workspace`-scoped servers are
    /// ref-driven ([`set_session_root`](Self::set_session_root)) and intentionally left
    /// untouched here -- they have no root until a session references them. A workspace
    /// server whose definition changed in the file is re-aligned on the next turn (its
    /// config is re-read then).
    async fn reconcile(&mut self) {
        let all: Vec<McpServerConfig> = match self.shared_config.read() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let desired: Vec<McpServerConfig> = all
            .into_iter()
            .filter(|c| c.scope == McpScope::Global)
            .collect();
        let running: Vec<McpServerConfig> = self
            .handles
            .values()
            .filter(|h| h.key.scope == ScopeKey::Global)
            .map(|h| h.config.clone())
            .collect();
        let actions = reconcile(&desired, &running);

        for action in actions {
            match action {
                ReconcileAction::Stop(id) => {
                    self.stop(&InstanceKey::global(&id)).await;
                }
                ReconcileAction::Restart(cfg) => {
                    let key = InstanceKey::global(&cfg.id);
                    self.stop(&key).await;
                    self.start(key, cfg).await;
                }
                ReconcileAction::Start(cfg) => {
                    let key = InstanceKey::global(&cfg.id);
                    self.start(key, cfg).await;
                }
            }
        }
        self.publish();
    }

    /// Manual restart of a server by id (from [`SupervisorHandle::restart`]). Restarts
    /// **every** live instance with that id -- the global one and any per-workspace ones
    /// -- reusing each instance's key and config, so even one parked in `Failed`
    /// (auto-retry exhausted) is revived. If no instance is live, a global-scoped config
    /// entry is started fresh. Bypasses the backoff timer. Unknown ids are a no-op.
    async fn restart(&mut self, id: &str) {
        let targets: Vec<(InstanceKey, McpServerConfig)> = self
            .handles
            .iter()
            .filter(|(k, _)| k.id == id)
            .map(|(k, h)| (k.clone(), h.config.clone()))
            .collect();
        if targets.is_empty() {
            let cfg = self.shared_config.read().ok().and_then(|g| {
                g.iter()
                    .find(|c| c.id == id && c.scope == McpScope::Global)
                    .cloned()
            });
            if let Some(cfg) = cfg {
                self.start(InstanceKey::global(id), cfg).await;
                self.publish();
            }
            return;
        }
        for (key, cfg) in targets {
            self.stop(&key).await;
            self.start(key, cfg).await;
        }
        self.publish();
    }

    /// Recompute the workspace instances `session_id` references now that it is rooted
    /// at `root`: update ref-lists, evict emptied workspace instances, and proactively
    /// (re)start each referenced instance that is not `Running` (RFC 0018 §4.3, §4.5).
    async fn set_session_root(
        &mut self,
        session_id: String,
        root: PathBuf,
        servers: Vec<McpServerConfig>,
    ) {
        // The workspace-scoped servers this session wants, keyed at its canonical root.
        // `servers` is the session's resolved tier set (RFC 0018 §3.2) computed by the
        // desktop -- global file overlaid by the phenotype + session tiers (C3). We take
        // the `scope: workspace` subset here; global-scoped servers are always-on via
        // `reconcile` against the watched file (RFC 0018 §14, the Global tier).
        let scope_key = ScopeKey::workspace(&root);
        let wanted: BTreeMap<InstanceKey, McpServerConfig> = servers
            .into_iter()
            .filter(|c| c.scope == McpScope::Workspace && !c.disabled)
            .map(|c| {
                (
                    InstanceKey {
                        id: c.id.clone(),
                        scope: scope_key.clone(),
                    },
                    c,
                )
            })
            .collect();

        // Drop this session from any workspace instance it no longer references.
        let stale: Vec<InstanceKey> = self
            .ws_refs
            .iter()
            .filter(|(k, refs)| refs.contains(&session_id) && !wanted.contains_key(k))
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale {
            self.release_ref(&key, &session_id).await;
        }

        // Add this session to each wanted instance, (re)starting it as needed.
        for (key, cfg) in wanted {
            self.ws_refs
                .entry(key.clone())
                .or_default()
                .insert(session_id.clone());
            self.ensure_running(key, cfg).await;
        }
        self.publish();
    }

    /// Drop `session_id` from every workspace instance ref-list, evicting any that
    /// empties (RFC 0018 §4.3).
    async fn release_session(&mut self, session_id: String) {
        let keys: Vec<InstanceKey> = self
            .ws_refs
            .iter()
            .filter(|(_, refs)| refs.contains(&session_id))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            self.release_ref(&key, &session_id).await;
        }
        self.publish();
    }

    /// Remove one session's reference to a workspace instance; if the ref-list empties,
    /// stop and forget the instance (RFC 0018 §4.3).
    async fn release_ref(&mut self, key: &InstanceKey, session_id: &str) {
        if let Some(refs) = self.ws_refs.get_mut(key) {
            refs.remove(session_id);
            if refs.is_empty() {
                self.ws_refs.remove(key);
                self.stop(key).await;
            }
        }
    }

    /// Ensure a referenced workspace instance exists, runs the current config, and is
    /// `Running` -- proactively (re)starting it otherwise (RFC 0018 §4.5). A changed
    /// definition (command/args/env/disabled) restarts; a `Failed`/`Restarting`/exited
    /// instance is revived for this turn.
    async fn ensure_running(&mut self, key: InstanceKey, cfg: McpServerConfig) {
        match self.handles.get(&key) {
            Some(h) if h.config != cfg => {
                self.stop(&key).await;
                self.start(key, cfg).await;
            }
            Some(h) if h.state == McpServerState::Running => {}
            Some(_) => self.try_connect(&key).await,
            None => self.start(key, cfg).await,
        }
    }

    async fn start(&mut self, key: InstanceKey, cfg: McpServerConfig) {
        if cfg.disabled {
            // Reconcile wouldn't ask for this, but be defensive.
            let mut handle = ServerHandle::new(key.clone(), cfg, &self.config);
            handle.state = McpServerState::Disabled;
            self.handles.insert(key, handle);
            return;
        }
        let handle = ServerHandle::new(key.clone(), cfg, &self.config);
        self.handles.insert(key.clone(), handle);
        self.try_connect(&key).await;
    }

    async fn stop(&mut self, key: &InstanceKey) {
        if let Some(mut handle) = self.handles.remove(key) {
            if let Some(client) = handle.client.take() {
                // Bound the graceful close: a wedged child must not stall app exit.
                // On timeout we drop the client and let kill-on-drop reap it.
                let id = &key.id;
                match tokio::time::timeout(SHUTDOWN_TIMEOUT, client.shutdown()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(server = id, error = %e, "mcp shutdown"),
                    Err(_) => {
                        tracing::warn!(server = id, "mcp shutdown timed out; dropping to reap")
                    }
                }
            }
        }
    }

    async fn stop_all(&mut self) {
        let keys: Vec<InstanceKey> = self.handles.keys().cloned().collect();
        for key in keys {
            self.stop(&key).await;
        }
        self.publish();
    }

    /// Attempt a connect for `id`. Updates state, tool_count, pid, error, and the
    /// retry schedule based on the outcome.
    async fn try_connect(&mut self, key: &InstanceKey) {
        let cfg = match self.handles.get(key) {
            Some(h) => h.config.clone(),
            None => return,
        };
        // Mark Starting + publish so the UI sees the transition.
        if let Some(h) = self.handles.get_mut(key) {
            h.state = McpServerState::Starting;
            h.pid = None;
        }
        self.publish();

        let allow_refs: Vec<&str> = self
            .config
            .env_allowlist
            .iter()
            .map(String::as_str)
            .collect();
        // A workspace instance learns its checkout from its key's root: advertised as
        // an MCP root and set as the child cwd (belt-and-braces). A global instance has
        // no root (RFC 0018 §4.4).
        let root = key.scope.root().map(Path::to_path_buf);
        let roots: Vec<&Path> = root.as_deref().into_iter().collect();
        // Resolve ${workspace}/${root} in the config against this instance's checkout
        // (#544), so a workspace-aware server (e.g. codegraph) gets an explicit
        // --path even when it ignores the advertised MCP root / child cwd.
        let cfg = crate::config::substitute_workspace(cfg, root.as_deref());
        let connect_result = McpClient::connect(&cfg, &allow_refs, root.as_deref(), &roots).await;
        let outcome = match connect_result {
            Ok(client) => match client.list_tools().await {
                Ok(tools) => Ok((client, tools)),
                Err(e) => {
                    let _ = client.shutdown().await;
                    Err(e.to_string())
                }
            },
            Err(e) => Err(e.to_string()),
        };

        let max_failures = self.config.max_failures;
        let Some(h) = self.handles.get_mut(key) else {
            return;
        };
        match outcome {
            Ok((client, tools)) => {
                let was_restart = h.restarts > 0 || h.failures > 0;
                h.pid = client.pid();
                h.client = Some(client);
                h.tools = tools;
                h.state = McpServerState::Running;
                h.last_error = None;
                // Do NOT clear `failures`/`backoff` here: a server that connects but
                // exits again before `min_healthy_uptime` is flapping, and clearing on
                // every connect would let it loop forever. The counters are cleared in
                // `on_tick` once the server has *proven* healthy (stayed up long enough).
                h.next_retry_at = None;
                h.last_health_check = Some(Instant::now());
                h.running_since = Some(Instant::now());
                if was_restart {
                    h.restarts = h.restarts.saturating_add(1);
                }
            }
            Err(msg) => {
                h.client = None;
                h.pid = None;
                h.tools.clear();
                h.last_error = Some(msg);
                h.failures = h.failures.saturating_add(1);
                if h.failures >= max_failures {
                    h.state = McpServerState::Failed;
                    h.next_retry_at = None;
                } else {
                    h.state = McpServerState::Restarting;
                    h.next_retry_at = Some(Instant::now() + h.backoff.next_delay());
                }
            }
        }
        self.publish();
    }

    /// Periodic pass: drive Restarting servers whose retry is due, and probe Running
    /// servers due for a health check.
    async fn on_tick(&mut self) {
        let now = Instant::now();
        let mut due_retry: Vec<InstanceKey> = Vec::new();
        let mut due_probe: Vec<InstanceKey> = Vec::new();
        let mut proven_healthy: Vec<InstanceKey> = Vec::new();

        for (key, h) in &self.handles {
            match h.state {
                McpServerState::Restarting => {
                    if h.failures < self.config.max_failures
                        && h.next_retry_at.map(|t| now >= t).unwrap_or(true)
                    {
                        due_retry.push(key.clone());
                    }
                }
                McpServerState::Running => {
                    // Once a server has stayed up for `min_healthy_uptime`, it has
                    // proven healthy: clear any failure debt so a later isolated exit
                    // (detected by the next health probe) recovers cleanly rather than
                    // counting toward a park.
                    let proven = h.failures > 0
                        && h.running_since
                            .map(|t| now.duration_since(t) >= self.config.min_healthy_uptime)
                            .unwrap_or(false);
                    if proven {
                        proven_healthy.push(key.clone());
                    }
                    let due = h
                        .last_health_check
                        .map(|t| now.duration_since(t) >= self.config.health_interval)
                        .unwrap_or(true);
                    if due {
                        due_probe.push(key.clone());
                    }
                }
                _ => {}
            }
        }

        for key in proven_healthy {
            if let Some(h) = self.handles.get_mut(&key) {
                h.failures = 0;
                h.backoff.reset();
            }
        }
        for key in due_retry {
            self.try_connect(&key).await;
        }
        for key in due_probe {
            self.health_probe(&key).await;
        }
    }

    /// `list_tools` against the live client; treat errors like a crash.
    async fn health_probe(&mut self, key: &InstanceKey) {
        // Take the client out so we can `await` without holding a `&mut` borrow on
        // `self.handles`. Put it back if the probe succeeds.
        let client = match self.handles.get_mut(key).and_then(|h| h.client.take()) {
            Some(c) => c,
            None => return,
        };
        let result = client.list_tools().await;
        let max_failures = self.config.max_failures;
        let Some(h) = self.handles.get_mut(key) else {
            // Server was removed mid-probe: drop the client.
            let _ = client.shutdown().await;
            return;
        };
        match result {
            Ok(tools) => {
                h.tools = tools;
                h.last_health_check = Some(Instant::now());
                h.client = Some(client);
                // Publish so a tool-set change on a healthy server reaches both the
                // UI (M4.4) and the bridge (M4.3) promptly.
                self.publish();
            }
            Err(e) => {
                // The connection looks dead — drop it and schedule a retry.
                drop(client);
                h.pid = None;
                h.tools.clear();
                h.last_error = Some(e.to_string());
                h.failures = h.failures.saturating_add(1);
                if h.failures >= max_failures {
                    h.state = McpServerState::Failed;
                    h.next_retry_at = None;
                } else {
                    h.state = McpServerState::Restarting;
                    h.next_retry_at = Some(Instant::now() + h.backoff.next_delay());
                }
                self.publish();
            }
        }
    }

    fn publish(&self) {
        // `handles` is a BTreeMap keyed by `(id, scope)`, so `values()` already yields
        // a deterministic id-then-scope order; the explicit sort keeps the published
        // contract (id-sorted, instances of one id grouped) independent of that.
        let mut snap: Vec<McpServerStatus> = self.handles.values().map(|h| h.snapshot()).collect();
        // Surface *disabled* configured servers that have no live instance as synthetic
        // rows (review #595). A server only gets a `ServerHandle` once instantiated: a
        // Global server via `reconcile`, a Workspace server via `set_session_root` (which
        // skips disabled). So a disabled Workspace server -- e.g. the seeded codegraph --
        // never produces a handle, and with the status list built purely from handles its
        // Settings -> MCP row would vanish, leaving the user no way to enable it. An
        // enabled server instead surfaces through its real handle (Global immediately;
        // Workspace once a session roots it), so we only synthesize the disabled case.
        let live_ids: std::collections::HashSet<&str> =
            self.handles.keys().map(|k| k.id.as_str()).collect();
        if let Ok(cfgs) = self.shared_config.read() {
            for c in cfgs
                .iter()
                .filter(|c| c.disabled && !live_ids.contains(c.id.as_str()))
            {
                snap.push(McpServerStatus {
                    id: c.id.clone(),
                    state: McpServerState::Disabled,
                    tool_count: 0,
                    last_error: None,
                    restarts: 0,
                    pid: None,
                    // No live instance, so no concrete scope to disambiguate.
                    scope_key: None,
                });
            }
        }
        snap.sort_by(|a, b| a.id.cmp(&b.id).then(a.scope_key.cmp(&b.scope_key)));
        // Recover from a poisoned lock rather than skipping: we fully overwrite the
        // Vec, so a previous writer's panic can't leave bad data behind, and skipping
        // would otherwise freeze the published status permanently. Matches the
        // poison-tolerant read in `tools_snapshot`.
        *self.status.write().unwrap_or_else(|p| p.into_inner()) = snap;
        // Rebuild the flat tool list for the bridge (M4.3). Only Running servers
        // contribute; Restarting/Failed ones have empty tool vecs anyway.
        let all_tools: Vec<PublishedTool> = self
            .handles
            .values()
            .filter(|h| h.state == McpServerState::Running)
            .flat_map(|h| {
                // Overlay the server's egress policy (RFC 0013) onto each published
                // tool. The client can't know it (no protocol annotation), so the
                // supervisor — which owns the config — resolves it here. Unset =
                // fail-safe network-capable.
                let reaches_network = h.config.reaches_network.unwrap_or(true);
                // RFC 0024: unset means deferred — bridged tools are the bulk of the
                // standing tools-block cost.
                let defer = h.config.defer.unwrap_or(true);
                let key = h.key.clone();
                h.tools.iter().cloned().map(move |mut info| {
                    info.reaches_network = reaches_network;
                    info.defer = defer;
                    PublishedTool {
                        key: key.clone(),
                        info,
                    }
                })
            })
            .collect();
        *self.tools.write().unwrap_or_else(|p| p.into_inner()) = all_tools;
        // Wake any status subscriber (the desktop shell's event forwarder). Coalescing
        // and lossless of intent: subscribers re-snapshot, so a missed intermediate
        // tick never matters. Ignore send errors (no subscribers is fine).
        let _ = self.status_tx.send(());
    }

    /// Execute a tool call against the named server's live client. Called inline
    /// from the actor's select loop — blocks supervision for the call's duration
    /// (bounded by `CALL_TIMEOUT`, but preempted early when `stop_all` flips the quit
    /// latch so app exit isn't stalled — #119). This keeps single-ownership of clients
    /// intact, preserving clean reaping and graceful close (see RFC 0003 §6).
    async fn do_call_tool(
        &mut self,
        key: &InstanceKey,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<String, crate::McpError> {
        let server = &key.id;
        let client = self
            .handles
            .get_mut(key)
            .and_then(|h| {
                if h.state == McpServerState::Running {
                    h.client.take()
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                crate::McpError::Protocol(format!("server '{server}' is not running"))
            })?;

        // Race the call against the quit latch so app exit can preempt a slow tool
        // instead of stalling up to CALL_TIMEOUT (#119). The inner block owns the call
        // future so it is dropped (releasing its borrow of `client`) before we restore
        // the client below. `biased` checks the latch first, so a StopAll queued ahead
        // of this call short-circuits without even starting to poll the call.
        let outcome = {
            let mut cancel = self.cancel_rx.clone();
            let call = tokio::time::timeout(CALL_TIMEOUT, client.call_tool(tool, args));
            tokio::pin!(call);
            tokio::select! {
                biased;
                _ = cancel.wait_for(|stopping| *stopping) => None,
                r = &mut call => Some(r),
            }
        };

        // Restore the client regardless of outcome (we didn't kill it). On timeout or
        // preemption the in-flight request is intentionally left to resolve-and-drop:
        // rmcp multiplexes by request id, so a late response to the abandoned call
        // won't be mismatched against the next call on the reused client.
        if let Some(h) = self.handles.get_mut(key) {
            h.client = Some(client);
        }

        match outcome {
            Some(Ok(Ok(text))) => Ok(text),
            Some(Ok(Err(e))) => Err(e),
            Some(Err(_elapsed)) => Err(crate::McpError::Protocol(format!(
                "tool call '{tool}' on server '{server}' timed out after {}s",
                CALL_TIMEOUT.as_secs()
            ))),
            None => Err(crate::McpError::Protocol(format!(
                "tool call '{tool}' on server '{server}' aborted: supervisor stopping"
            ))),
        }
    }
}
