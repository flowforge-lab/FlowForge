//! `observer` tool — single agent-facing entry point for the observer
//! framework. Dispatches on `action` (start | stop | list) and is structured
//! to match the issue's CLI surface:
//!
//! ```text
//! observer start --source file    --target ./src/ [--filter "*.rs"]
//! observer start --source http    --target <url>   [--filter "ready"] [--interval 60]
//! observer start --source process --target <pid>   [--filter "error|panic"]
//! observer list
//! observer stop  <id>
//! ```
//!
//! Session-scoped lifecycle is owned by the [`ObserverSupervisor`] the host
//! hands in at construction; the tool is a thin adapter that parses JSON,
//! calls the supervisor, and renders the result.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use ff_tools::{Safety, Tool, ToolOutcome};

use crate::event::{ObserverError, ObserverId, ObserverKind, ObserverSpec};
use crate::supervisor::ObserverSupervisor;

/// The single tool name. Stable wire value: changing it is a breaking
/// change to the model schema.
pub const OBSERVER_TOOL_NAME: &str = "observer";

pub struct ObserverTool {
    supervisor: Arc<ObserverSupervisor>,
}

impl ObserverTool {
    pub fn new(supervisor: Arc<ObserverSupervisor>) -> Self {
        Self { supervisor }
    }

    fn kind_arg(args: &Value) -> Result<ObserverKind, ToolOutcome> {
        let s = args
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolOutcome::error("observer: missing `source` (file|http|process)"))?;
        match s {
            "file" => Ok(ObserverKind::File),
            "http" => Ok(ObserverKind::Http),
            "process" => Ok(ObserverKind::Process),
            other => Err(ToolOutcome::error(format!(
                "observer: unknown source '{other}'; expected file|http|process"
            ))),
        }
    }

    fn target_arg(args: &Value) -> Result<String, ToolOutcome> {
        let s = args
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolOutcome::error("observer: missing `target` (path, url, or pid)"))?;
        if s.trim().is_empty() {
            return Err(ToolOutcome::error("observer: `target` must not be empty"));
        }
        Ok(s.to_string())
    }

    fn filter_arg(args: &Value) -> Option<String> {
        args.get("filter")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    }

    fn interval_arg(args: &Value) -> Result<Option<std::time::Duration>, ToolOutcome> {
        match args.get("interval") {
            None => Ok(None),
            Some(Value::Number(n)) => match n.as_u64() {
                Some(secs) => Ok(Some(std::time::Duration::from_secs(secs))),
                None => Err(ToolOutcome::error(
                    "observer: `interval` must be a non-negative integer (seconds)",
                )),
            },
            Some(Value::String(s)) => match s.trim().parse::<u64>() {
                Ok(secs) => Ok(Some(std::time::Duration::from_secs(secs))),
                Err(_) => Err(ToolOutcome::error(
                    "observer: `interval` must be a non-negative integer (seconds)",
                )),
            },
            Some(_) => Err(ToolOutcome::error(
                "observer: `interval` must be a number of seconds",
            )),
        }
    }

    /// Render an [`ObserverError`] for the model. Keeps the wire shape
    /// human-readable; the model reads the message verbatim.
    fn render_error(e: ObserverError) -> ToolOutcome {
        ToolOutcome::error(format!("observer: {e}"))
    }
}

#[async_trait]
impl Tool for ObserverTool {
    fn name(&self) -> &str {
        OBSERVER_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Watch a file path, HTTP URL, or background process and have a new agent turn fire \
         automatically when something changes. Observers are session-scoped and die with \
         the session. Actions: `start` (begin watching), `stop` (cancel by id), `list` \
         (this session's observers). For file targets the OS watcher (kqueue on macOS, \
         inotify on Linux) is event-driven and zero-cost when idle; for HTTP the poll \
         interval is clamped to >= 30s; for process, a regex filter matches against \
         captured stdout/stderr lines. Max 8 observers per session."
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
                "source": {
                    "type": "string",
                    "enum": ["file", "http", "process"],
                    "description": "For `start`: the kind of source to watch."
                },
                "target": {
                    "type": "string",
                    "description": "For `start`: a file path (file), URL (http), or numeric process id (process)."
                },
                "filter": {
                    "type": "string",
                    "description": "Optional regex. For `file`, matches the changed basename. \
                                    For `http`, the body must contain a match. For `process`, \
                                    per-line stdout/stderr must match."
                },
                "interval": {
                    "type": "integer",
                    "description": "For `start --source http`: poll interval in seconds. \
                                    Clamped to >= 30."
                },
                "id": {
                    "type": "integer",
                    "description": "For `stop`: the observer id returned by `start`."
                }
            },
            "required": ["action"]
        })
    }

    fn safety(&self, args: &Value) -> Safety {
        match args.get("action").and_then(Value::as_str) {
            Some("list") => Safety::ReadOnly,
            _ => Safety::Write,
        }
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        self.run_with_session(args, _root, ff_tools::NO_SESSION)
            .await
    }

    async fn run_with_session(&self, args: Value, _root: &Path, session_id: &str) -> ToolOutcome {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(a) => a,
            None => {
                return ToolOutcome::error(
                    "observer: missing required argument: action (start|stop|list)",
                )
            }
        };
        match action {
            "start" => {
                let kind = match Self::kind_arg(&args) {
                    Ok(k) => k,
                    Err(e) => return e,
                };
                let target = match Self::target_arg(&args) {
                    Ok(t) => t,
                    Err(e) => return e,
                };
                let filter = Self::filter_arg(&args);
                let interval = match Self::interval_arg(&args) {
                    Ok(i) => i,
                    Err(e) => return e,
                };
                let spec = ObserverSpec {
                    kind,
                    target,
                    filter,
                    interval,
                };
                match self.supervisor.start(session_id, spec).await {
                    Ok(id) => ToolOutcome::ok(format!(
                        "started observer {id} ({} observers in this session)",
                        self.supervisor.list(session_id).len(),
                    )),
                    Err(e) => Self::render_error(e),
                }
            }
            "stop" => {
                let id_num = match args.get("id") {
                    Some(Value::Number(n)) => n.as_u64(),
                    Some(Value::String(s)) => s.trim().parse().ok(),
                    _ => None,
                };
                let Some(id_num) = id_num else {
                    return ToolOutcome::error("observer stop: missing or non-numeric `id`");
                };
                match self.supervisor.stop(session_id, ObserverId(id_num)) {
                    Ok(()) => ToolOutcome::ok(format!("stopped observer {id_num}")),
                    Err(e) => Self::render_error(e),
                }
            }
            "list" => {
                let list = self.supervisor.list(session_id);
                if list.is_empty() {
                    return ToolOutcome::ok("No observers.");
                }
                let mut out = String::new();
                for o in list {
                    out.push_str(&format!(
                        "#{id} [{kind}] {target} (key={key}, fires={fires})\n",
                        id = o.id.0,
                        kind = o.kind.as_str(),
                        target = o.target,
                        key = o.key,
                        fires = o.fires,
                    ));
                }
                ToolOutcome::ok(out.trim_end().to_string())
            }
            other => ToolOutcome::error(format!(
                "observer: unknown action '{other}'; expected start|stop|list"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatches_actions_and_validates() {
        let sup = Arc::new(ObserverSupervisor::new());
        let tool = ObserverTool::new(sup.clone());
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("watched.txt");
        std::fs::write(&path, "x").unwrap();

        // list is read-only
        assert_eq!(tool.safety(&json!({"action": "list"})), Safety::ReadOnly);
        // start/stop are write
        assert_eq!(tool.safety(&json!({"action": "start"})), Safety::Write);
        assert_eq!(tool.safety(&json!({"action": "stop"})), Safety::Write);

        // missing action
        let out = tool.run(json!({}), tmp.path()).await;
        assert!(!out.success);

        // start with no source errors
        let out = tool
            .run(
                json!({"action": "start", "target": path.display().to_string()}),
                tmp.path(),
            )
            .await;
        assert!(!out.success, "missing source should error: {}", out.content);

        // start with bad source
        let out = tool
            .run(
                json!({"action": "start", "source": "nope", "target": path.display().to_string()}),
                tmp.path(),
            )
            .await;
        assert!(!out.success, "bad source should error: {}", out.content);

        // list when empty
        let out = tool.run(json!({"action": "list"}), tmp.path()).await;
        assert!(out.success);
        assert_eq!(out.content, "No observers.");
    }
}
