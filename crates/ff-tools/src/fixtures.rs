//! Tool stubs for tests in *other* crates, behind the `test-fixtures` feature so
//! they never reach a production build — the same shape as the `tauri` crate's
//! unstable `test` module, which the desktop enables only as a dev-dependency
//! (see `apps/desktop/src-tauri/Cargo.toml`).
//!
//! Exported because of a measured trap (#1107): no built-in tool overrides
//! [`Tool::defer`], so `ToolRegistry::new().deferred_tool_names()` is **empty**.
//! Any test that exercises a deferral- or preheat-gated path against a default
//! registry therefore passes while asserting nothing — the gate rejects every
//! name for want of a candidate, and a no-op reads as a pass. Injecting a
//! deferred tool is what makes those paths observable at all.

use crate::registry::{Tool, ToolOutcome};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

/// A deferred tool stub with a caller-chosen name. Mirrors the private
/// `Deferred` stub in `tool_search::tests`; kept in sync by construction, since
/// what matters to a caller is only that `defer()` is `true`.
pub struct DeferredStub {
    name: String,
    desc: String,
    schema: Value,
}

impl DeferredStub {
    /// A deferred stub carrying a minimal object schema.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            desc: "A deferred stub tool.".into(),
            schema: json!({"type": "object", "properties": {}}),
        }
    }

    /// A deferred stub whose spec is padded to approximately `bytes` when
    /// serialised, for exercising a byte-budget gate.
    #[must_use]
    pub fn with_spec_bytes(name: impl Into<String>, bytes: usize) -> Self {
        Self {
            name: name.into(),
            desc: "x".repeat(bytes),
            schema: json!({"type": "object", "properties": {}}),
        }
    }
}

/// A *resident* (non-deferred) stub, for asserting that the deferral gate
/// rejects it. Distinct from simply omitting a tool: a name absent from the
/// registry is rejected for being absent, which proves nothing about the gate.
pub struct ResidentStub {
    name: String,
}

impl ResidentStub {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait]
impl Tool for ResidentStub {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "A resident stub tool."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::ok("ran")
    }
}

#[async_trait]
impl Tool for DeferredStub {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.desc
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    fn defer(&self) -> bool {
        true
    }
    async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::ok("ran")
    }
}
