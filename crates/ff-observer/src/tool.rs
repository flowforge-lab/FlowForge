//! The `observer` tool — the single agent-facing surface for the
//! observer framework. Dispatches on an `action` discriminator
//! (`start` / `stop` / `list`) and threads the caller's `session_id`
//! to the supervisor for cross-session isolation. Mirrors
//! `ProcessManagerTool` (`crates/ff-tools/src/process.rs:550`) so a
//! model that already knows `process_manager` immediately knows
//! `observer`.

use super::source::{HttpMode, ObserverInfo, ObserverKind, ObserverSpec};
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
         external state changes. Sources: `file` (a file or directory path with \
         an optional glob filter; uses kqueue/inotify), `http` (a URL polled on \
         an interval with an optional substring `filter`; wakes when the body \
         changes — or, with `filter`, when the new body contains the substring), \
         and `process` (a numeric process_id returned by `process_manager start`; \
         wakes when new stdout/stderr bytes match a regex `filter`, with the \
         matched line in the wake). Observers are session-scoped: each one \
         belongs to the session that started it and is reaped when that \
         session is deleted. Actions: `start` (begin watching a target; returns \
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
                    "enum": ["file", "http", "process"],
                    "description": "start: source kind. `file` watches a path; `http` polls a URL; `process` observes a running background process (see `process_manager`)."
                },
                "target": {
                    "type": "string",
                    "description": "start: file/directory path (file), http(s) URL (http), or numeric process_id (process). File targets are relative to the workspace root; http URLs must be absolute; process ids are integers returned by `process_manager start`."
                },
                "filter": {
                    "type": "string",
                    "description": "start: glob for file directory targets, a plain substring the http body must contain (http, `change` mode only — ignored in `ready` mode), or a regex applied to each new stdout/stderr chunk (process). Multi-line patterns should be passed without `(?m)` — the source enables it."
                },
                "mode": {
                    "type": "string",
                    "enum": ["change", "ready"],
                    "description": "start (http, optional): `change` (default) wakes whenever the response body changes; `ready` wakes ONCE the moment the URL first responds 2xx, then completes — the dev-server readiness probe (start a server, then `observer --kind http --target http://localhost:3000/health --mode ready`). Point `ready` at a dedicated health endpoint (e.g. `/health`), not the root URL: a root or catch-all route often returns 200 with a landing or framework error page before the app is truly serving, which would fire a false 'ready'. Ignored for file/process."
                },
                "interval_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "start (http, optional): seconds between polls. In `change` mode, clamped to >= 30 and defaults to 60. In `ready` mode, clamped to >= 1 and defaults to 2 (a readiness probe must poll near-instantly)."
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

    /// Per RFC 0013: the `http` source reaches the network, so the
    /// tool stays on the network-capable set. (`file` would be
    /// `LocalOnly`, but the same tool surface starts both kinds, and
    /// mixing per-kind phenotypes on a single tool isn't worth the
    /// complexity for the user — keep the fail-safe default.)
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
                            "unknown kind '{other}'; expected one of: file, http, process"
                        ));
                    }
                };
                // `target` semantics differ per kind: file sources take
                // a path that may be relative to the session root,
                // http sources take a URL that must already be absolute
                // — passing an http URL through `resolve_target` would
                // join it with the session root and silently corrupt it.
                // `process` takes the *string* form of a u64 process id
                // returned by `process_manager start`; passed through
                // as-is so leading/trailing whitespace is preserved for
                // the parse in the supervisor.
                let target_str = if kind == ObserverKind::Http {
                    let Some(t) = args
                        .get("target")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                    else {
                        return ToolOutcome::error("start requires a `target` URL for kind=http");
                    };
                    t
                } else if kind == ObserverKind::Process {
                    let Some(t) = args
                        .get("target")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                    else {
                        return ToolOutcome::error(
                            "start requires a numeric `target` process_id for kind=process",
                        );
                    };
                    t
                } else {
                    let target = Self::resolve_target(&args, root);
                    target.to_string_lossy().into_owned()
                };
                let filter = args
                    .get("filter")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string());
                let interval_secs = args.get("interval_secs").and_then(Value::as_u64);
                // `mode` applies to http only: "ready" (fire once on first 2xx)
                // vs "change" (default, diff the body). #954 item 4.
                let http_mode = match args.get("mode").and_then(Value::as_str) {
                    Some("ready") => HttpMode::Ready,
                    _ => HttpMode::Change,
                };
                let spec = ObserverSpec {
                    label: label.to_string(),
                    kind,
                    target: target_str,
                    filter,
                    interval_secs,
                    http_mode,
                };
                match self.supervisor.start(spec, session_id) {
                    Ok(id) => {
                        let suffix = match (kind, http_mode) {
                            (ObserverKind::Http, HttpMode::Ready) => {
                                "\n(fires once when the target first responds 2xx, then completes.)"
                            }
                            (ObserverKind::Http, HttpMode::Change) => {
                                "\n(first poll is silent; the next change wakes the agent.)"
                            }
                            _ => "",
                        };
                        ToolOutcome::ok(format!(
                            "started observer {id}: kind={kind_str}, label=\"{label}\"{suffix}\n\
                             stop with action=stop, observer_id={id}"
                        ))
                    }
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
    async fn tool_start_is_listed_under_the_callers_session() {
        // Regression (#1038 M2): the `👁 Observers` panel reads
        // `list_observers(session_id)` with the FE session id, and the agent
        // runs the `observer` tool with that same id via `run_with_session`.
        // Guard the contract the panel depends on — an observer started under a
        // session id is visible in *that* session's list and invisible to
        // others. (A `tool.run`/`NO_SESSION_TOOL` start, as the other tests use,
        // would land in the anonymous bucket the panel never queries — which is
        // exactly the mismatch class this asserts against.)
        let (dir, tool) = tool_with_supervisor();
        let target = dir.path().to_string_lossy().into_owned();
        let started = tool
            .run_with_session(
                json!({"action": "start", "label": "watch", "kind": "file", "target": target}),
                dir.path(),
                "session-a",
            )
            .await;
        assert!(started.success, "{}", started.content);

        // Visible in its own session — this is the exact call the desktop
        // `list_observers` command forwards to, so a non-empty result here is
        // what makes the panel render.
        let mine = tool.supervisor.list("session-a");
        assert_eq!(mine.len(), 1, "observer must be listed for its own session");
        assert_eq!(mine[0].label, "watch");

        // Isolated: another session sees nothing (no cross-session leakage).
        assert!(
            tool.supervisor.list("session-b").is_empty(),
            "observer must not leak into another session's list"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_unknown_source_errors() {
        let (_dir, tool) = tool_with_supervisor();
        let res = tool
            .run(
                json!({
                    "action": "start",
                    "label": "x",
                    "kind": "totally-not-a-kind",
                    "target": "1",
                }),
                _dir.path(),
            )
            .await;
        assert!(!res.success, "{}", res.content);
        // The error message should mention the bad kind so the model
        // can self-correct.
        assert!(
            res.content.contains("unknown kind") || res.content.contains("expected one of"),
            "{}",
            res.content
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_http_kind_is_accepted() {
        // Phase 2: http is a real source. The supervisor's start will
        // kick off polling against a (likely-unreachable) URL; the
        // tool call itself must succeed and list the observer.
        let (dir, tool) = tool_with_supervisor();
        let res = tool
            .run(
                json!({
                    "action": "start",
                    "label": "poll",
                    "kind": "http",
                    "target": "https://example.com/",
                    "interval_secs": 60,
                }),
                dir.path(),
            )
            .await;
        assert!(res.success, "{}", res.content);
        assert!(
            res.content.contains("first poll is silent"),
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
