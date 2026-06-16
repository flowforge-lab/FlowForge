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
//! Health (RFC 0003 §5): every `health_interval`, the supervisor calls `list_tools` on
//! each `Running` server. A failure is treated like a crash: the connection is dropped,
//! `failures` is bumped, and the server moves to `Restarting`. Once `failures >=
//! max_failures` the server is parked in `Failed` and not retried until the config is
//! reloaded — saving CPU on a permanently-broken server (e.g. wrong command).
//!
//! Backoff: capped exponential ([`Backoff`]). A successful connect resets it.
//!
//! Shutdown: [`SupervisorHandle::stop_all`] cancels every live `McpClient` and drops
//! the map. `rmcp`'s `ChildWithCleanup` kills any straggler on drop, but only if the
//! Tokio runtime is still alive — the desktop wrapper invokes `stop_all` from the
//! Tauri `RunEvent::ExitRequested` hook so reaping completes before exit.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use ff_core::{McpServerConfig, McpServerState, McpServerStatus};
use tokio::sync::{mpsc, oneshot};

use crate::backoff::Backoff;
use crate::client::McpClient;
use crate::reconcile::{reconcile, ReconcileAction};
use crate::watch::SharedConfig;

/// Read-only snapshot the UI subscribes to (M4.4). The supervisor swaps a freshly
/// rebuilt vec on every state change; readers never block on the actor.
pub type SharedStatus = Arc<RwLock<Vec<McpServerStatus>>>;

/// How long a single graceful close may take before we give up and drop the
/// client, letting `process_wrap`'s kill-on-drop reap the child. Bounds app-exit
/// latency so one wedged server can't stall the whole quit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

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
}

impl SupervisorHandle {
    /// Ask the supervisor to re-snapshot the shared config and apply any deltas. The
    /// watcher already pings on file change; this is for callers that mutate
    /// `SharedConfig` programmatically (tests).
    pub async fn reconcile_now(&self) {
        let _ = self.cmd_tx.send(Cmd::Reconcile).await;
    }

    /// Stop every server and exit the actor. Returns once all `shutdown` calls have
    /// completed (or timed out) so the caller can let the Tokio runtime wind down with
    /// no children still waiting to be reaped.
    pub async fn stop_all(&self) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::StopAll(ack_tx)).await.is_err() {
            return;
        }
        let _ = ack_rx.await;
    }
}

enum Cmd {
    Reconcile,
    StopAll(oneshot::Sender<()>),
}

struct ServerHandle {
    /// The config the server was *started with*. `reconcile` compares against this so
    /// an unchanged definition produces no action.
    config: McpServerConfig,
    client: Option<McpClient>,
    state: McpServerState,
    tool_count: usize,
    pid: Option<u32>,
    last_error: Option<String>,
    restarts: u32,
    failures: u32,
    backoff: Backoff,
    /// When the supervisor should next attempt a connect. `None` while running or
    /// while parked in `Failed`.
    next_retry_at: Option<Instant>,
    last_health_check: Option<Instant>,
}

impl ServerHandle {
    fn new(config: McpServerConfig, sup: &SupervisorConfig) -> Self {
        let disabled = config.disabled;
        Self {
            config,
            client: None,
            state: if disabled {
                McpServerState::Disabled
            } else {
                McpServerState::Starting
            },
            tool_count: 0,
            pid: None,
            last_error: None,
            restarts: 0,
            failures: 0,
            backoff: Backoff::new(sup.backoff_base, sup.backoff_max),
            next_retry_at: None,
            last_health_check: None,
        }
    }

    fn snapshot(&self) -> McpServerStatus {
        McpServerStatus {
            id: self.config.id.clone(),
            state: self.state,
            tool_count: self.tool_count,
            last_error: self.last_error.clone(),
            restarts: self.restarts,
            pid: self.pid,
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
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(16);
    let actor = Supervisor {
        config,
        handles: BTreeMap::new(),
        shared_config,
        status: Arc::clone(&status),
        cmd_rx,
        change_rx,
    };
    tokio::spawn(actor.run());
    SupervisorHandle { cmd_tx, status }
}

struct Supervisor {
    config: SupervisorConfig,
    handles: BTreeMap<String, ServerHandle>,
    shared_config: SharedConfig,
    status: SharedStatus,
    cmd_rx: mpsc::Receiver<Cmd>,
    change_rx: mpsc::UnboundedReceiver<()>,
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
                    Cmd::StopAll(ack) => {
                        self.stop_all().await;
                        let _ = ack.send(());
                        return;
                    }
                },
                else => return,
            }
        }
    }

    /// Snapshot the desired config and apply Stop / Restart / Start to close the gap.
    async fn reconcile(&mut self) {
        let desired: Vec<McpServerConfig> = match self.shared_config.read() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let running: Vec<McpServerConfig> =
            self.handles.values().map(|h| h.config.clone()).collect();
        let actions = reconcile(&desired, &running);

        for action in actions {
            match action {
                ReconcileAction::Stop(id) => self.stop(&id).await,
                ReconcileAction::Restart(cfg) => {
                    self.stop(&cfg.id).await;
                    self.start(cfg).await;
                }
                ReconcileAction::Start(cfg) => self.start(cfg).await,
            }
        }
        self.publish();
    }

    async fn start(&mut self, cfg: McpServerConfig) {
        let id = cfg.id.clone();
        if cfg.disabled {
            // Reconcile wouldn't ask for this, but be defensive.
            let mut handle = ServerHandle::new(cfg, &self.config);
            handle.state = McpServerState::Disabled;
            self.handles.insert(id, handle);
            return;
        }
        let handle = ServerHandle::new(cfg, &self.config);
        self.handles.insert(id.clone(), handle);
        self.try_connect(&id).await;
    }

    async fn stop(&mut self, id: &str) {
        if let Some(mut handle) = self.handles.remove(id) {
            if let Some(client) = handle.client.take() {
                // Bound the graceful close: a wedged child must not stall app exit.
                // On timeout we drop the client and let kill-on-drop reap it.
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
        let ids: Vec<String> = self.handles.keys().cloned().collect();
        for id in ids {
            self.stop(&id).await;
        }
        self.publish();
    }

    /// Attempt a connect for `id`. Updates state, tool_count, pid, error, and the
    /// retry schedule based on the outcome.
    async fn try_connect(&mut self, id: &str) {
        let cfg = match self.handles.get(id) {
            Some(h) => h.config.clone(),
            None => return,
        };
        // Mark Starting + publish so the UI sees the transition.
        if let Some(h) = self.handles.get_mut(id) {
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
        let connect_result = McpClient::connect(&cfg, &allow_refs).await;
        let outcome = match connect_result {
            Ok(client) => match client.list_tools().await {
                Ok(tools) => Ok((client, tools.len())),
                Err(e) => {
                    let _ = client.shutdown().await;
                    Err(e.to_string())
                }
            },
            Err(e) => Err(e.to_string()),
        };

        let max_failures = self.config.max_failures;
        let Some(h) = self.handles.get_mut(id) else {
            return;
        };
        match outcome {
            Ok((client, tool_count)) => {
                let was_restart = h.restarts > 0 || h.failures > 0;
                h.pid = client.pid();
                h.client = Some(client);
                h.tool_count = tool_count;
                h.state = McpServerState::Running;
                h.last_error = None;
                h.failures = 0;
                h.backoff.reset();
                h.next_retry_at = None;
                h.last_health_check = Some(Instant::now());
                if was_restart {
                    h.restarts = h.restarts.saturating_add(1);
                }
            }
            Err(msg) => {
                h.client = None;
                h.pid = None;
                h.tool_count = 0;
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
        let mut due_retry: Vec<String> = Vec::new();
        let mut due_probe: Vec<String> = Vec::new();

        for (id, h) in &self.handles {
            match h.state {
                McpServerState::Restarting => {
                    if h.failures < self.config.max_failures
                        && h.next_retry_at.map(|t| now >= t).unwrap_or(true)
                    {
                        due_retry.push(id.clone());
                    }
                }
                McpServerState::Running => {
                    let due = h
                        .last_health_check
                        .map(|t| now.duration_since(t) >= self.config.health_interval)
                        .unwrap_or(true);
                    if due && h.client.is_some() {
                        due_probe.push(id.clone());
                    }
                }
                _ => {}
            }
        }

        for id in due_retry {
            self.try_connect(&id).await;
        }
        for id in due_probe {
            self.health_probe(&id).await;
        }
    }

    /// `list_tools` against the live client; treat errors like a crash.
    async fn health_probe(&mut self, id: &str) {
        // Take the client out so we can `await` without holding a `&mut` borrow on
        // `self.handles`. Put it back if the probe succeeds.
        let client = match self.handles.get_mut(id).and_then(|h| h.client.take()) {
            Some(c) => c,
            None => return,
        };
        let result = client.list_tools().await;
        let max_failures = self.config.max_failures;
        let Some(h) = self.handles.get_mut(id) else {
            // Server was removed mid-probe: drop the client.
            let _ = client.shutdown().await;
            return;
        };
        match result {
            Ok(tools) => {
                h.tool_count = tools.len();
                h.last_health_check = Some(Instant::now());
                h.client = Some(client);
                // Publish so a tool-set change on a healthy server reaches the UI
                // (M4.4) promptly instead of waiting for the next state transition.
                self.publish();
            }
            Err(e) => {
                // The connection looks dead — drop it and schedule a retry.
                drop(client);
                h.pid = None;
                h.tool_count = 0;
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
        let mut snap: Vec<McpServerStatus> = self.handles.values().map(|h| h.snapshot()).collect();
        snap.sort_by(|a, b| a.id.cmp(&b.id));
        if let Ok(mut g) = self.status.write() {
            *g = snap;
        }
    }
}
