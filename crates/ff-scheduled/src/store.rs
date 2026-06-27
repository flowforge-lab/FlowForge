//! Durable SQLite store for scheduled tasks, mirroring `ff-session`'s
//! `SessionStore` (RFC 0017 §5): a `Mutex<Connection>`, version-gated
//! migrations, and the same PRAGMAs. `cadence_label`/`next_run` are derived on
//! read (never columns); firing belongs to the runner (#542), so this store
//! only persists tasks + run records and answers the due query.

use std::path::Path;
use std::sync::Mutex;

use chrono::Local;
use ff_core::{
    BuiltinAction, CreateScheduledTaskInput, RunStatus, SafetyCeiling, ScheduledTask, TaskKind,
};
use rusqlite::{params, Connection, OptionalExtension};

use crate::cron;

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_ms() -> i64 {
    Local::now().timestamp_millis()
}

/// Error deleting a built-in task (they ship with the app; RFC 0017 §6.3).
#[derive(Debug, PartialEq, Eq)]
pub struct DeleteBuiltinError;

impl std::fmt::Display for DeleteBuiltinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "built-in tasks cannot be deleted")
    }
}

impl std::error::Error for DeleteBuiltinError {}

/// App-wide durable table of scheduled tasks + their fire history.
pub struct ScheduledStore {
    conn: Mutex<Connection>,
}

impl ScheduledStore {
    /// Open or create the store at `path`.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        Self::from_conn(Connection::open(path)?)
    }

    /// In-memory store, for tests.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a new task. Validates the cron expression first.
    pub fn create(&self, input: CreateScheduledTaskInput) -> Result<ScheduledTask, String> {
        cron::parse(&input.cron)?;
        let id = new_id();
        let (kind_tag, kind_value) = encode_kind(&input.kind);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO scheduled_tasks
                 (id, name, cron, kind, kind_value, workspace, profile,
                  safety_ceiling, paused, last_run_ms, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, NULL, ?9)",
            params![
                id,
                input.name,
                input.cron,
                kind_tag,
                kind_value,
                input.workspace,
                input.profile,
                ceiling_to_text(input.safety_ceiling),
                now_ms(),
            ],
        )
        .map_err(|e| format!("insert scheduled task: {e}"))?;
        drop(conn);
        self.get(&id)
            .ok_or_else(|| "task vanished after insert".into())
    }

    /// All tasks, newest first, with derived `cadence_label`/`next_run`.
    pub fn list(&self) -> Vec<ScheduledTask> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, cron, kind, kind_value, workspace, profile,
                        safety_ceiling, paused, last_run_ms
                 FROM scheduled_tasks ORDER BY created_ms DESC",
            )
            .expect("prepare list");
        let rows = stmt
            .query_map([], row_to_task)
            .expect("query list")
            .filter_map(Result::ok)
            .collect();
        rows
    }

    /// One task by id, with derived fields.
    pub fn get(&self, id: &str) -> Option<ScheduledTask> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name, cron, kind, kind_value, workspace, profile,
                    safety_ceiling, paused, last_run_ms
             FROM scheduled_tasks WHERE id = ?1",
            params![id],
            row_to_task,
        )
        .optional()
        .expect("query get")
    }

    /// Flip `paused`; returns the updated task.
    pub fn toggle_paused(&self, id: &str) -> Option<ScheduledTask> {
        {
            let conn = self.conn.lock().unwrap();
            let updated = conn
                .execute(
                    "UPDATE scheduled_tasks SET paused = NOT paused WHERE id = ?1",
                    params![id],
                )
                .expect("toggle paused");
            if updated == 0 {
                return None;
            }
        }
        self.get(id)
    }

    /// Delete a task and (via cascade) its run records. Rejects built-ins.
    pub fn delete(&self, id: &str) -> Result<bool, DeleteBuiltinError> {
        let Some(task) = self.get(id) else {
            return Ok(false);
        };
        if task.kind.is_builtin() {
            return Err(DeleteBuiltinError);
        }
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute("DELETE FROM scheduled_tasks WHERE id = ?1", params![id])
            .expect("delete task");
        Ok(n > 0)
    }

    /// Stamp `last_run_ms` after a fire (called by the runner, #542).
    pub fn stamp_last_run(&self, id: &str, fired_ms: i64) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE scheduled_tasks SET last_run_ms = ?2 WHERE id = ?1",
            params![id, fired_ms],
        )
        .expect("stamp last_run");
    }

    /// Append a fire record; returns its row id.
    pub fn append_run(&self, task_id: &str, session_id: Option<&str>, status: RunStatus) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO scheduled_runs (task_id, session_id, fired_ms, status)
             VALUES (?1, ?2, ?3, ?4)",
            params![task_id, session_id, now_ms(), status_to_text(status)],
        )
        .expect("append run");
        conn.last_insert_rowid()
    }

    /// Tasks that are due to fire at `now` (local time): not paused, and the
    /// most recent occurrence at/before `now` is strictly newer than the last
    /// fire (RFC 0017 §4). A `None` `last_run` means "never fired", so any past
    /// occurrence makes it due. `now` is taken from the local clock so the
    /// predicate matches the local-time cadence the user set.
    pub fn due_tasks(&self) -> Vec<ScheduledTask> {
        let now = Local::now();
        self.list()
            .into_iter()
            .filter(|t| !t.paused)
            .filter(|t| {
                let Ok(sched) = cron::parse(&t.cron) else {
                    return false;
                };
                match cron::prev_occurrence(&sched, now) {
                    Some(prev) => t.last_run.is_none_or(|last| prev > last),
                    None => false,
                }
            })
            .collect()
    }
}

// --- row mapping & enum <-> text helpers -----------------------------------

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<ScheduledTask> {
    let cron_expr: String = row.get("cron")?;
    let kind_tag: String = row.get("kind")?;
    let kind_value: String = row.get("kind_value")?;
    let ceiling: String = row.get("safety_ceiling")?;
    let last_run: Option<i64> = row.get("last_run_ms")?;
    let next_run = cron::parse(&cron_expr)
        .ok()
        .and_then(|s| cron::next_run(&s, Local::now()));
    Ok(ScheduledTask {
        id: row.get("id")?,
        name: row.get("name")?,
        cadence_label: cron::cadence_label(&cron_expr),
        cron: cron_expr,
        kind: decode_kind(&kind_tag, &kind_value),
        workspace: row.get("workspace")?,
        profile: row.get("profile")?,
        safety_ceiling: text_to_ceiling(&ceiling),
        paused: row.get::<_, i64>("paused")? != 0,
        next_run,
        last_run,
    })
}

fn encode_kind(kind: &TaskKind) -> (&'static str, String) {
    match kind {
        TaskKind::Prompt(p) => ("prompt", p.clone()),
        TaskKind::Builtin(a) => ("builtin", builtin_to_text(*a).to_string()),
    }
}

fn decode_kind(tag: &str, value: &str) -> TaskKind {
    match tag {
        "builtin" => TaskKind::Builtin(text_to_builtin(value)),
        _ => TaskKind::Prompt(value.to_string()),
    }
}

fn builtin_to_text(a: BuiltinAction) -> &'static str {
    match a {
        BuiltinAction::MemoryConsolidate => "memory_consolidate",
    }
}

fn text_to_builtin(_s: &str) -> BuiltinAction {
    // Only one variant today; decoding is total by construction.
    BuiltinAction::MemoryConsolidate
}

fn ceiling_to_text(c: SafetyCeiling) -> &'static str {
    match c {
        SafetyCeiling::ReadOnly => "read_only",
        SafetyCeiling::Write => "write",
    }
}

fn text_to_ceiling(s: &str) -> SafetyCeiling {
    match s {
        "write" => SafetyCeiling::Write,
        _ => SafetyCeiling::ReadOnly,
    }
}

fn status_to_text(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Ok => "ok",
        RunStatus::Error => "error",
        RunStatus::Cancelled => "cancelled",
    }
}

// --- migrations ------------------------------------------------------------

/// Version-gated migration runner (same shape as `ff-session`). v1 is the
/// initial scheduled_tasks + scheduled_runs schema (RFC 0017 §5).
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scheduled_tasks (
                 id             TEXT PRIMARY KEY,
                 name           TEXT NOT NULL,
                 cron           TEXT NOT NULL,
                 kind           TEXT NOT NULL,
                 kind_value     TEXT NOT NULL,
                 workspace      TEXT,
                 profile        TEXT,
                 safety_ceiling TEXT NOT NULL DEFAULT 'read_only',
                 paused         INTEGER NOT NULL DEFAULT 0,
                 last_run_ms    INTEGER,
                 created_ms     INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS scheduled_runs (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 task_id     TEXT NOT NULL,
                 session_id  TEXT,
                 fired_ms    INTEGER NOT NULL,
                 status      TEXT NOT NULL,
                 FOREIGN KEY (task_id) REFERENCES scheduled_tasks(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_scheduled_runs_task
                 ON scheduled_runs(task_id, fired_ms);",
        )?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn prompt(name: &str, cron: &str) -> CreateScheduledTaskInput {
        CreateScheduledTaskInput {
            name: name.to_string(),
            cron: cron.to_string(),
            kind: TaskKind::Prompt("do the thing".into()),
            workspace: None,
            profile: None,
            safety_ceiling: SafetyCeiling::ReadOnly,
        }
    }

    #[test]
    fn create_rejects_bad_cron() {
        let s = ScheduledStore::open_in_memory().unwrap();
        assert!(s.create(prompt("bad", "0 17 * * *")).is_err());
        assert!(s.create(prompt("ok", "0 0 17 * * *")).is_ok());
    }

    #[test]
    fn list_derives_cadence_and_next_run() {
        let s = ScheduledStore::open_in_memory().unwrap();
        let t = s.create(prompt("digest", &cron::daily(17, 0))).unwrap();
        assert_eq!(t.cadence_label, "Daily at 5:00 PM");
        assert!(t.next_run.is_some());
        assert!(t.last_run.is_none());
    }

    #[test]
    fn delete_rejected_for_builtin() {
        let s = ScheduledStore::open_in_memory().unwrap();
        let mut input = prompt("organizer", &cron::daily(17, 0));
        input.kind = TaskKind::Builtin(BuiltinAction::MemoryConsolidate);
        let t = s.create(input).unwrap();
        assert_eq!(s.delete(&t.id), Err(DeleteBuiltinError));
        // a prompt task deletes fine
        let p = s.create(prompt("p", &cron::daily(9, 0))).unwrap();
        assert_eq!(s.delete(&p.id), Ok(true));
    }

    #[test]
    fn toggle_flips_paused() {
        let s = ScheduledStore::open_in_memory().unwrap();
        let t = s.create(prompt("p", &cron::daily(9, 0))).unwrap();
        assert!(!t.paused);
        assert!(s.toggle_paused(&t.id).unwrap().paused);
        assert!(!s.toggle_paused(&t.id).unwrap().paused);
        assert!(s.toggle_paused("nope").is_none());
    }

    #[test]
    fn restart_durability() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sched.db");
        let id = {
            let s = ScheduledStore::open(&path).unwrap();
            s.create(prompt("survivor", &cron::daily(9, 0))).unwrap().id
        };
        let s2 = ScheduledStore::open(&path).unwrap();
        assert!(s2.get(&id).is_some(), "task must survive reopen");
    }

    #[test]
    fn due_predicate_fires_once_then_not_again() {
        // The headline regression (RFC §4/§9): a task whose slot is in the past
        // is due once; after stamping last_run it is no longer due.
        let s = ScheduledStore::open_in_memory().unwrap();
        // A daily task at a time guaranteed already past today: 00:00.
        let t = s.create(prompt("nightly", &cron::daily(0, 0))).unwrap();

        // Never fired -> due.
        assert!(
            s.due_tasks().iter().any(|x| x.id == t.id),
            "fresh past-slot task should be due"
        );

        // Stamp last_run = now -> the past slot is no longer > last_run.
        s.stamp_last_run(&t.id, now_ms());
        assert!(
            !s.due_tasks().iter().any(|x| x.id == t.id),
            "should not re-fire after stamping last_run"
        );
    }

    #[test]
    fn paused_task_never_due() {
        let s = ScheduledStore::open_in_memory().unwrap();
        let t = s.create(prompt("nightly", &cron::daily(0, 0))).unwrap();
        s.toggle_paused(&t.id);
        assert!(!s.due_tasks().iter().any(|x| x.id == t.id));
    }

    #[test]
    fn append_run_cascades_on_delete() {
        let s = ScheduledStore::open_in_memory().unwrap();
        let t = s.create(prompt("p", &cron::daily(9, 0))).unwrap();
        s.append_run(&t.id, Some("sess-1"), RunStatus::Ok);
        assert_eq!(s.delete(&t.id), Ok(true));
        // run record gone via cascade (no panic, count 0)
        let conn = s.conn.lock().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM scheduled_runs WHERE task_id = ?1",
                params![t.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }
}
