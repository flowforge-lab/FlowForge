//! The `observer` tool — the single agent-facing surface for the
//! observer framework. Dispatches on an `action` discriminator
//! (`start` / `stop` / `list`) and threads the caller's `session_id`
//! to the supervisor for cross-session isolation. Mirrors
//! `ProcessManagerTool` (`crates/ff-tools/src/process.rs:550`) so a
//! model that already knows `process_manager` immediately knows
//! `observer`.

use super::source::{ObserverInfo, ObserverKind, ObserverSpec};
use super::supervisor::ObserverSupervisor;
use ff_tools::{Safety, Tool, ToolOutcome};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ObserverTool {
    supervisor: Arc<ObserverSupervisor>,
}

impl ObserverTool {
    pub fn new(supervisor: Arc<ObserverSupervisor>) -> Self {
        Self { supervisor }
    }

    /// Resolve `target` against the session root. Mirrors
    /// `ProcessManagerTool::resolve_dir` so the two tools have
    /// identical path semantics — a relative target joins the root;
    /// an absolute target is used as-is.
    fn resolve_target(args: &Value, root: &Path) -> PathBuf {
        match args.get("target").and_then(Value::as_str) {
            Some(t) if !t.trim().is_empty() => {
                let p = Path::new(t);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                }
            }
            _ => root.to_path_buf(),
        }
    }

    /// Parse `observer_id` as a JSON number or numeric string. Same
    /// shape as `ProcessManagerTool::id_arg` so a model that passed a
    /// string id in `start`'s output is accepted by `stop`.
    fn id_arg(args: &Value) -> Option<u64> {
        match args.get("observer_id") {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ObserverTool {
    fn name(&self) -> &str {
        "observer"
    }

    fn description(&self) -> &str {
        "Start, list, and stop background observers that wake the agent when \
         external state changes. Phase 1 supports the `file` source (a file or \
         directory path with an optional glob filter); `http` and `process` \
         sources ship in later releases. Observers are session-scoped: each one \
         belongs to the session that started it and is reaped when that session \
         is deleted. Actions: `start` (begin watching a target; returns \
         observer_id), `list` (this session's observers), `stop` (end a watcher)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "stop", "list"],
                    "description": "What to do."
                },
                "label": {
                    "type": "string",
                    "description": "start: human-readable name shown in wake messages (`[Observer \"<label>\"]: ...`)."
                },
                "kind": {
                    "type": "string",
                    "enum": ["file"],
                    "description": "start: source kind. Phase 1 only ships `file`; `http` and `process` will be added in subsequent releases."
                },
                "target": {
                    "type": "string",
                    "description": "start: file or directory path. Absolute, or relative to the workspace root."
                },
                "filter": {
                    "type": "string",
                    "description": "start (file, optional): glob that limits which children of a directory target trigger a wake."
                },
                "observer_id": {
                    "type": "integer",
                    "description": "stop: the id returned by `start`."
                }
            },
            "required": ["action"]
        })
    }

    fn safety(&self, args: &Value) -> Safety {
        // ReadOnly for `list` (always safe); Write for `start`/`stop`
        // (start can have side effects on a wake; stop mutates
        // supervisor state).
        match args.get("action").and_then(Value::as_str) {
            Some("list") => Safety::ReadOnly,
            _ => Safety::Write,
        }
    }

    /// Fail-safe per RFC 0013: until Phase 2 lands and the
    /// `reaches_network() = false` override is justified, the tool
    /// stays on the network-capable set (LocalOnly phenotype hides
    /// it). This matches the fail-safe default on
    /// `ProcessManagerTool`.
    fn reaches_network(&self) -> bool {
        true
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        self.run_with_session(args, root, crate::NO_SESSION_TOOL)
            .await
    }

    async fn run_with_session(&self, args: Value, root: &Path, session_id: &str) -> ToolOutcome {
        match args.get("action").and_then(Value::as_str) {
            Some("start") => {
                let Some(label) = args
                    .get("label")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                else {
                    return ToolOutcome::error("start requires a non-empty `label`");
                };
                let kind_str = args.get("kind").and_then(Value::as_str).unwrap_or("file");
                let kind = match kind_str {
                    "file" => ObserverKind::File,
                    "http" => ObserverKind::Http,
                    "process" => ObserverKind::Process,
                    other => {
                        return ToolOutcome::error(format!(
                            "unknown kind '{other}'; only 'file' is implemented in Phase 1"
                        ));
                    }
                };
                let target = Self::resolve_target(&args, root);
                let target_str = target.to_string_lossy().into_owned();
                let filter = args
                    .get("filter")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string());
                let spec = ObserverSpec {
                    label: label.to_string(),
                    kind,
                    target: target_str,
                    filter,
                };
                match self.supervisor.start(spec, session_id) {
                    Ok(id) => ToolOutcome::ok(format!(
                        "started observer {id}: kind={kind_str}, label=\"{label}\"\n\
                         stop with action=stop, observer_id={id}"
                    )),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("stop") => {
                let Some(id) = Self::id_arg(&args) else {
                    return ToolOutcome::error("stop requires a numeric `observer_id`");
                };
                match self.supervisor.stop(id, session_id).await {
                    Ok(body) => ToolOutcome::ok(body),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("list") => ToolOutcome::ok(format_infos(&self.supervisor.list(session_id))),
            Some(other) => ToolOutcome::error(format!(
                "unknown action '{other}'; expected start|stop|list"
            )),
            None => ToolOutcome::error("missing required argument: action (start|stop|list)"),
        }
    }
}

fn format_infos(infos: &[ObserverInfo]) -> String {
    if infos.is_empty() {
        return "No observers.".to_string();
    }
    let mut s = String::new();
    for info in infos {
        s.push_str(&format!(
            "#{} [{}] label=\"{}\" target={} (started {})\n",
            info.id,
            kind_label(info.kind),
            info.label,
            info.target,
            info.started_at.to_rfc3339(),
        ));
    }
    s.trim_end().to_string()
}

fn kind_label(k: ObserverKind) -> &'static str {
    match k {
        ObserverKind::File => "file",
        ObserverKind::Http => "http",
        ObserverKind::Process => "process",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_with_supervisor() -> (tempfile::TempDir, ObserverTool) {
        let dir = tempfile::tempdir().unwrap();
        let (sup, _rx) = ObserverSupervisor::new();
        (dir, ObserverTool::new(Arc::new(sup)))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tool_validates_args_and_classifies_safety() {
        let (dir, tool) = tool_with_supervisor();

        // Missing action.
        assert!(!tool.run(json!({}), dir.path()).await.success);
        // Unknown action.
        assert!(
            !tool
                .run(json!({"action": "nope"}), dir.path())
                .await
                .success
        );
        // start without label.
        assert!(
            !tool
                .run(json!({"action": "start", "kind": "file", "target": dir.path().to_string_lossy()}), dir.path())
                .await
                .success
        );
        // start with unknown kind.
        assert!(
            !tool
                .run(
                    json!({"action": "start", "label": "x", "kind": "warp", "target": dir.path().to_string_lossy()}),
                    dir.path()
                )
                .await
                .success
        );
        // list alone is OK.
        assert!(
            tool.run(json!({"action": "list"}), dir.path())
                .await
                .success
        );
        // stop without id.
        assert!(
            !tool
                .run(json!({"action": "stop"}), dir.path())
                .await
                .success
        );

        // Safety classification.
        assert_eq!(tool.safety(&json!({"action": "list"})), Safety::ReadOnly);
        assert_eq!(tool.safety(&json!({"action": "start"})), Safety::Write);
        assert_eq!(tool.safety(&json!({"action": "stop"})), Safety::Write);
        // reaches_network is fail-safe true.
        assert!(tool.reaches_network());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_then_stop_round_trip() {
        let (dir, tool) = tool_with_supervisor();
        let target = dir.path().to_string_lossy().into_owned();
        let started = tool
            .run(
                json!({"action": "start", "label": "watch", "kind": "file", "target": target}),
                dir.path(),
            )
            .await;
        assert!(started.success, "{}", started.content);
        let id: u64 = started
            .content
            .split_whitespace()
            .find_map(|w| w.trim_end_matches(':').parse().ok())
            .expect("observer id in start output");

        // list shows the new observer.
        let listed = tool.run(json!({"action": "list"}), dir.path()).await;
        assert!(listed.success);
        assert!(listed.content.contains("watch"), "{}", listed.content);

        // stop returns Ok.
        let stopped = tool
            .run(json!({"action": "stop", "observer_id": id}), dir.path())
            .await;
        assert!(stopped.success, "{}", stopped.content);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_unknown_source_errors() {
        let (dir, tool) = tool_with_supervisor();
        let target = dir.path().to_string_lossy().into_owned();
        let res = tool
            .run(
                json!({
                    "action": "start",
                    "label": "x",
                    "kind": "http",
                    "target": target,
                }),
                dir.path(),
            )
            .await;
        assert!(!res.success, "{}", res.content);
        // The error message should mention Phase 2 so the model
        // can self-correct.
        assert!(
            res.content.contains("Phase 2") || res.content.contains("not yet implemented"),
            "{}",
            res.content
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn id_arg_accepts_string_and_number() {
        // Numbers parse.
        assert_eq!(ObserverTool::id_arg(&json!({"observer_id": 42})), Some(42));
        // Numeric strings parse.
        assert_eq!(
            ObserverTool::id_arg(&json!({"observer_id": "42"})),
            Some(42)
        );
        // Non-numeric strings don't.
        assert_eq!(ObserverTool::id_arg(&json!({"observer_id": "x"})), None);
        // Missing doesn't.
        assert_eq!(ObserverTool::id_arg(&json!({})), None);
    }
}
