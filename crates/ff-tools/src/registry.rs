//! The tool abstraction and the registry the agent loop dispatches through.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

// Safety is defined in ff-core (needed by PermissionMatrix without circular deps)
// and re-exported from this crate for backward compatibility.
pub use ff_core::Safety;
use ff_core::{Mode, PermissionMatrix};

/// The wake source an [`ObserverIntent`] requests. M3 (#1039) only emits
/// [`ObserverIntentKind::Process`]; `File`/`Http` are reserved so the shape is
/// stable when future tools declare those. Mirrors `ff_observer::ObserverKind`
/// as a plain enum so `ff-tools` need not depend on `ff-observer` (the dependency
/// runs the other way — `ff-observer` uses `ff_tools::process`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserverIntentKind {
    File,
    Http,
    Process,
}

/// A tool's declaration that the host should attach a background observer after
/// this call (#1039, epic #954 M3). The tool cannot start the observer itself —
/// `ff-observer` depends on `ff-tools`, so a direct call would be circular — so it
/// declares intent here and the host (which owns both supervisors) translates this
/// into an `ObserverSpec` and calls `ObserverSupervisor::start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserverIntent {
    pub kind: ObserverIntentKind,
    /// Source-specific target. For `Process` this is the process id as a string
    /// (the `process_manager` id the observer subscribes to).
    pub target: String,
    /// Human-readable name echoed in wake messages and shown by the observer list.
    pub label: String,
    /// Source-specific filter. For `Process` it is the stdout wake pattern (regex /
    /// substring); `None` means wake on any output.
    pub filter: Option<String>,
    /// Source-specific cadence (http/file). `None` for `Process`.
    pub interval_secs: Option<u64>,
}

/// The result of running a tool. `content` is fed back to the model verbatim as the
/// tool message; `success` lets the host render pass/fail without parsing `content`.
/// `observer_intent`, when set, asks the host to attach a background observer after
/// the call returns (#1039) — used by long-running tools to wake the agent on
/// ongoing output without a second `observer` tool call. It is `Box`ed because it is
/// `None` on virtually every call and is large enough to otherwise bloat every
/// `Result<_, ToolOutcome>` past clippy's `result_large_err` threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub success: bool,
    pub content: String,
    pub observer_intent: Option<Box<ObserverIntent>>,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
            observer_intent: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            success: false,
            content: content.into(),
            observer_intent: None,
        }
    }

    /// Attach an observer intent to this outcome (builder-style), so the host
    /// starts a background observer after the tool returns.
    pub fn with_observer(mut self, intent: ObserverIntent) -> Self {
        self.observer_intent = Some(Box::new(intent));
        self
    }
}

/// Sentinel session id meaning "no owning session" — the call has no session
/// affinity. Used by [`Tool::run`] and [`ToolRegistry::run`] when the caller
/// has no session to thread (external/test entry points). Tools that bucket by
/// session (e.g. [`crate::process::ProcessManager`]) treat all such calls as
/// sharing one anonymous bucket, which is fine for one-off calls but would
/// collide if a *real* session id were ever empty. Real session ids are UUIDs
/// assigned by the host and are never empty.
pub const NO_SESSION: &str = "";

/// A callable the model can invoke. Implementors describe themselves as an
/// OpenAI-style function schema and execute against a jailed workspace `root`.
///
/// `run` must never panic or propagate transport errors to the caller — failures
/// are returned as [`ToolOutcome::error`] so the model can read and react to them.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema for the arguments object (the `parameters` field of the function).
    fn parameters(&self) -> Value;
    /// Classify a concrete invocation. Defaults to [`Safety::Write`] — implementors
    /// override when they can prove read-only or flag a destructive call.
    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }
    /// Worst-case safety this tool can ever reach, independent of arguments. Used to
    /// decide whether the tool is advertised at all in capability-restricted modes
    /// (Plan, RFC 0011): only tools whose ceiling is [`Safety::ReadOnly`] are shown.
    /// Defaults to the same conservative [`Safety::Write`] as [`Tool::safety`];
    /// tools with dynamic per-call safety (e.g. `bash`) override to their true ceiling.
    fn max_safety(&self) -> Safety {
        Safety::Write
    }
    /// Best-case safety this tool can ever reach — the *floor*, independent of
    /// arguments. A tool whose floor is [`Safety::ReadOnly`] has a genuine
    /// read-only path (e.g. `bash ls`, `gh pr_list`) and is worth advertising in
    /// capability-restricted modes (Plan, RFC 0011) even when its ceiling is
    /// higher; the per-call [`Tool::safety`] then gates each concrete invocation.
    /// Defaults to [`max_safety`](Self::max_safety) — a tool with a single fixed
    /// safety has floor == ceiling; only tools with dynamic per-call safety
    /// override this to their true floor.
    fn min_safety(&self) -> Safety {
        self.max_safety()
    }
    /// Interactive tools don't execute against the workspace — they pause the turn to
    /// ask the user something and resume with the answer (e.g. `ask_user`, #44). The
    /// agent loop routes them through [`Approver::ask`] instead of [`Tool::run`].
    ///
    /// Invariant: interactive tools MUST be side-effect-free ([`Safety::ReadOnly`]).
    /// The agent loop resolves them *before* the approval gate, so an interactive
    /// tool that performed `Write` work would bypass approval entirely.
    fn interactive(&self) -> bool {
        false
    }
    /// Whether this tool can send data over the network (RFC 0013 egress policy).
    /// Used by the advertised-toolset filter to strip network-capable tools under
    /// a `LocalOnly` phenotype (e.g. `enclave`), the privacy analogue of how Plan
    /// mode strips non-ReadOnly tools.
    ///
    /// **Fail-safe default is `true`**: a tool is assumed network-capable unless it
    /// proves otherwise. Only tools with no plausible egress path (pure local file
    /// / process-introspection / interactive) override to `false`. A tool that
    /// `exec`s arbitrary user code or shells out (`bash`, `python`, `process_manager`,
    /// MCP-bridged tools) MUST keep the `true` default — it could `curl` regardless
    /// of its nominal purpose.
    fn reaches_network(&self) -> bool {
        true
    }
    /// A stable identity for a *content read*, used by the agent's per-turn semantic
    /// read-dedupe (#458 RC5). A read tool (e.g. `view`) returns a key — typically
    /// the path it reads — so the loop can detect a re-read of the same target this
    /// turn and, when the content is unchanged, return a sentinel instead of
    /// re-injecting the bytes. Non-read tools keep the `None` default and are never
    /// deduped. Pure: it must not perform I/O — keying is by reference, not content.
    fn dedupe_key(&self, _args: &Value) -> Option<String> {
        None
    }
    /// Which search **corpus** this tool queries, if it is a search tool (#552 / #1011).
    ///
    /// Returns the [`SearchSource::id`](crate::web_search::SearchSource::id) of the
    /// backing source (`"web"`, `"pubmed"`, …). Used by the advertised-toolset filter
    /// to scope search corpora per phenotype, the same way [`Self::reaches_network`]
    /// scopes egress: the registry asks each tool what it is rather than consulting a
    /// hardcoded name list, so adding a source needs no change here or in the agent
    /// loop.
    ///
    /// **Default is `None`** — a non-search tool is never affected by search scoping.
    fn search_source_id(&self) -> Option<&str> {
        None
    }
    /// Whether this tool is *deferred*: discoverable via `tool_search` but kept out
    /// of the standing `tools` block (RFC 0024 Layer 1).
    ///
    /// Every advertised tool's full definition — name, description and complete
    /// parameter schema — sits in the provider's cached prompt prefix on *every*
    /// turn, whether or not the task needs it. Measured on a live workstation, the
    /// 31 built-ins cost 7,503 tokens while 77 MCP-bridged tools cost 33,890 — 81%
    /// of a ~41,400-token standing block spent keeping tools on standby. Beyond
    /// ~30-50 simultaneously-offered tools, selection accuracy also degrades.
    ///
    /// A deferred tool is still registered and still dispatchable; it is simply not
    /// advertised until `tool_search` re-admits it into the turn's allow-set.
    ///
    /// **Default is `false`** — a tool is advertised every turn unless it opts out.
    /// The built-in distribution is healthy (median 121 tokens) and these are
    /// high-frequency general-purpose tools, so they stay resident; MCP-bridged
    /// tools (long descriptions, narrow applicability, unbounded count) are the
    /// ones that opt in.
    ///
    /// Deferral is a *context-budget* mechanism, never a security one: re-admitted
    /// tools are still filtered by the mode/egress passes, so deferring a tool
    /// cannot be used to sneak it past a capability restriction.
    fn defer(&self) -> bool {
        false
    }
    /// Which argument properties each `action` of a dispatch-style tool actually
    /// reads (RFC 0024 Phase 2B, #1162).
    ///
    /// A dispatch tool (`github`, `git`, `process_manager`, `notebook_runner`)
    /// takes a required `action` discriminant and a union of every property any
    /// action might need. `github` advertises 17 properties for 17 actions, but
    /// `push` reads exactly one of them. Admission is keyed by tool name and
    /// injects [`Tool::parameters`] wholesale, so every turn pays for all 17.
    ///
    /// Returning `Some` lets the registry emit a per-action-scoped schema instead.
    /// Measured across the four dispatch tools: 5,896 → 1,753 bytes (70.3%), and
    /// because none of them defer, that is a cut to the *resident* prefix paid on
    /// every request.
    ///
    /// **Default is `None`** — no scoping, the full schema is emitted. Non-dispatch
    /// tools and un-migrated dispatch tools are unaffected.
    ///
    /// # Contract
    ///
    /// The returned map MUST list, for every action, every property that action's
    /// dispatch path reads — including properties read by helpers it forwards to.
    /// A missing entry is not a cosmetic error: the property is removed from the
    /// schema the model sees, so the capability silently disappears. No error, no
    /// warning, a normal-looking transcript. `assert_action_params_cover_dispatch`
    /// pins this per tool.
    ///
    /// Derive the map by reading the dispatch code, **never** by copying the
    /// action names mentioned in property descriptions — those are prose and are
    /// wrong in at least four places today (#1161).
    fn action_params(&self) -> Option<BTreeMap<&'static str, &'static [&'static str]>> {
        None
    }
    async fn run(&self, args: Value, root: &Path) -> ToolOutcome;

    /// Session-aware dispatch point. Tools that need per-session affinity
    /// (e.g. `process_manager`, which scopes its live-process table to the
    /// owning session and auto-reaps on close) override this. The default
    /// delegates to [`run`](Self::run), ignoring `session_id`. Callers without
    /// a session pass [`NO_SESSION`] as the sentinel; a real session id is a
    /// non-empty UUID threaded from the host.
    async fn run_with_session(&self, args: Value, root: &Path, session_id: &str) -> ToolOutcome {
        let _ = session_id;
        self.run(args, root).await
    }

    /// Streaming dispatch point (#680). Tools that buffer output until the process
    /// exits (e.g. `bash`) override this to push chunks to `sink` *as they are
    /// produced*, in addition to the full capture they still return in the final
    /// [`ToolOutcome`]. The live stream is additive: the returned result is
    /// byte-for-byte identical to [`run_with_session`](Self::run_with_session). The
    /// default ignores `sink` and delegates, so a non-streaming tool needs no change
    /// and a caller can always pass a sink safely.
    async fn run_streaming(
        &self,
        args: Value,
        root: &Path,
        session_id: &str,
        sink: Option<crate::OutputSink>,
    ) -> ToolOutcome {
        let _ = sink;
        self.run_with_session(args, root, session_id).await
    }
}

/// Name -> tool. Built with the M2 defaults (bash, view, edit) and queried by the
/// agent loop to (a) advertise schemas to the model and (b) dispatch calls.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in M2 toolset.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(Box::new(crate::bash::BashTool));
        r.register(Box::new(crate::python::PythonTool));
        r.register(Box::new(crate::view::ViewTool));
        r.register(Box::new(crate::edit::EditTool));
        r.register(Box::new(crate::write::WriteTool));
        r.register(Box::new(crate::apply_patch::ApplyPatchTool));
        r.register(Box::new(crate::grep::GrepTool));
        r.register(Box::new(crate::glob::GlobTool));
        r.register(Box::new(crate::tree::TreeTool));
        r.register(Box::new(crate::todo::TodoTool));
        r.register(Box::new(crate::web_fetch::WebFetchTool::new()));
        r.register(Box::new(crate::ask_user::AskUserTool));
        r.register(Box::new(crate::diagnostics::DiagnosticsTool));
        r.register(Box::new(crate::test_runner::TestRunnerTool::new()));
        r.register(Box::new(crate::git::GitTool));
        r.register(Box::new(crate::github::GithubTool));
        r.register(Box::new(crate::agent_tool::AgentTool));
        r
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Iterate over all registered tools (for permission-matrix filtering).
    pub fn iter_tools(&self) -> impl Iterator<Item = &dyn Tool> {
        self.tools.values().map(|b| b.as_ref())
    }

    /// All tools as OpenAI `tools` request entries.
    pub fn openai_tools(&self) -> Vec<Value> {
        self.openai_tools_for(None, true, None)
    }

    /// OpenAI `tools` entries, optionally restricted to a sub-agent's allowlist and
    /// with the `agent` delegation tool suppressed once the depth cap is reached
    /// (so a sub-agent at max depth is never even offered a spawn it cannot make).
    ///
    /// Tools are emitted in a **stable, name-sorted order**. The registry stores
    /// tools in a `HashMap` (random per-instance iteration order) and is rebuilt
    /// every turn, so an unsorted array would reorder the serialized `tools`
    /// block each turn. Since that block sits in the provider's cached prompt
    /// prefix (before messages), reordering busts the prefix on every turn —
    /// Bedrock writes a fresh cache entry but never reads one back
    /// (`cacheRead == 0`), forcing a full cold prefill and dominating TTBF
    /// (#947). Sorting keeps the prefix byte-identical across turns so the cache
    /// actually hits.
    pub fn openai_tools_for(
        &self,
        allowed: Option<&HashSet<String>>,
        allow_subagent: bool,
        scope: Option<&ActionScope>,
    ) -> Vec<Value> {
        let mut tools: Vec<&dyn Tool> = self
            .tools
            .values()
            .map(|t| t.as_ref())
            .filter(|t| allowed.is_none_or(|set| set.contains(t.name())))
            .filter(|t| allow_subagent || !is_subagent(t.name()))
            .collect();
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": scoped_parameters(t, scope.and_then(|m| m.get(t.name()))),
                    }
                })
            })
            .collect()
    }

    /// OpenAI `tools` entries for exactly `names`, name-sorted among themselves.
    ///
    /// Used to append just-in-time discovered tools to a turn's tools block
    /// (RFC 0024 §6). The append has to be a separate call rather than a widened
    /// [`openai_tools_for`] because that method sorts the *whole* array: re-running it
    /// with extra names would interleave them into the middle of the block, shifting
    /// the bytes that the provider's cached prefix depends on and triggering exactly
    /// the full cold prefill #947 exists to avoid. Sorting only within the appended
    /// batch keeps the block deterministic while leaving everything before it
    /// byte-identical, so growth is strictly append-only.
    pub fn openai_tools_named(
        &self,
        names: &HashSet<String>,
        scope: Option<&ActionScope>,
    ) -> Vec<Value> {
        let mut tools: Vec<&dyn Tool> = self
            .tools
            .values()
            .map(|t| t.as_ref())
            .filter(|t| names.contains(t.name()))
            .collect();
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": scoped_parameters(t, scope.and_then(|m| m.get(t.name()))),
                    }
                })
            })
            .collect()
    }

    /// Names of every tool whose worst-case safety is [`Safety::ReadOnly`] — tools
    /// that can *never* mutate regardless of arguments.
    pub fn readonly_tool_names(&self) -> HashSet<String> {
        self.tools
            .values()
            .filter(|t| t.max_safety() == Safety::ReadOnly)
            .map(|t| t.name().to_string())
            .collect()
    }

    /// Names of every tool with a read-only *floor* ([`Tool::min_safety`] is
    /// [`Safety::ReadOnly`]) — i.e. tools that have a genuine read-only path even
    /// if their ceiling is higher (`bash ls`, `gh pr_list`). Superset of
    /// [`readonly_tool_names`](Self::readonly_tool_names); the base of the Plan-mode
    /// advertised set (#793). The per-call [`Tool::safety`] gate then rejects any
    /// concrete invocation that exceeds what the Plan matrix row permits.
    pub fn readonly_capable_names(&self) -> HashSet<String> {
        self.tools
            .values()
            .filter(|t| t.min_safety() == Safety::ReadOnly)
            .map(|t| t.name().to_string())
            .collect()
    }

    /// Names of tools with no network-egress path ([`Tool::reaches_network`] is
    /// `false`). The base of a `LocalOnly` phenotype's advertised set (RFC 0013):
    /// the egress filter intersects the mode-visible set with this. Fail-safe —
    /// anything not proven local (default `true`) is excluded.
    pub fn local_tool_names(&self) -> HashSet<String> {
        self.tools
            .values()
            .filter(|t| !t.reaches_network())
            .map(|t| t.name().to_string())
            .collect()
    }

    /// Names of every registered **search** tool ([`Tool::search_source_id`] is
    /// `Some`), regardless of which corpus it queries (#552 / #1011).
    ///
    /// The subtrahend of the search-scoping filter: the advertised set drops every
    /// search tool, then re-admits only those whose source id the phenotype named.
    /// Derived by asking each tool, so a newly registered source is scoped correctly
    /// with no change here.
    pub fn search_tool_names(&self) -> HashSet<String> {
        self.tools
            .values()
            .filter(|t| t.search_source_id().is_some())
            .map(|t| t.name().to_string())
            .collect()
    }

    /// Names of the search tools whose corpus is one of `ids` (#552 / #1011).
    ///
    /// The counterpart to [`Self::search_tool_names`]: what a phenotype's
    /// `search_sources` list resolves to. An unknown id contributes nothing rather
    /// than erroring — a phenotype naming a source this build does not carry loses
    /// that corpus, it does not lose the whole toolset.
    pub fn search_tool_names_for(&self, ids: &[String]) -> HashSet<String> {
        self.tools
            .values()
            .filter(|t| {
                t.search_source_id()
                    .is_some_and(|id| ids.iter().any(|want| want == id))
            })
            .map(|t| t.name().to_string())
            .collect()
    }

    /// Names of tools that opt out of the standing `tools` block
    /// ([`Tool::defer`] is `true`). Subtracted from the advertised set and used as
    /// the corpus `tool_search` indexes (RFC 0024 Layer 1). Fail-open in the
    /// budget sense — anything not explicitly deferred stays advertised.
    pub fn deferred_tool_names(&self) -> HashSet<String> {
        self.tools
            .values()
            .filter(|t| t.defer())
            .map(|t| t.name().to_string())
            .collect()
    }

    /// Dispatch a call by name with an anonymous session ([`NO_SESSION`]). No
    /// session affinity — equivalent to
    /// [`run_with_session`](Self::run_with_session) with [`NO_SESSION`].
    pub async fn run(&self, name: &str, args: Value, root: &Path) -> ToolOutcome {
        self.run_with_session(name, args, root, NO_SESSION).await
    }

    /// Dispatch a call by name, threading the owning `session_id` to tools
    /// that implement [`Tool::run_with_session`]. Unknown tools and malformed
    /// arguments return an error outcome rather than failing the turn.
    pub async fn run_with_session(
        &self,
        name: &str,
        args: Value,
        root: &Path,
        session_id: &str,
    ) -> ToolOutcome {
        match self.get(name) {
            Some(tool) => tool.run_with_session(args, root, session_id).await,
            // Name the registered tools so a model that hallucinated a tool name
            // (e.g. `codegraph_explore`, #646) can self-correct in one turn instead
            // of guessing again. The list is sorted for a stable, diff-friendly hint.
            None => ToolOutcome::error(format!(
                "unknown tool: {name}. Available tools: {}",
                self.sorted_names().join(", ")
            )),
        }
    }

    /// Dispatch a call by name with an optional live-output `sink` (#680), threading
    /// the owning `session_id`. Streaming tools (e.g. `bash`) push chunks to `sink`
    /// as they are produced; non-streaming tools ignore it. The returned outcome is
    /// identical to [`run_with_session`](Self::run_with_session).
    pub async fn run_streaming(
        &self,
        name: &str,
        args: Value,
        root: &Path,
        session_id: &str,
        sink: Option<crate::OutputSink>,
    ) -> ToolOutcome {
        match self.get(name) {
            Some(tool) => tool.run_streaming(args, root, session_id, sink).await,
            None => ToolOutcome::error(format!(
                "unknown tool: {name}. Available tools: {}",
                self.sorted_names().join(", ")
            )),
        }
    }

    /// The registered tool names, sorted. Used to make an unknown-tool error
    /// actionable (#646).
    fn sorted_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Safety of a concrete call (defaults to [`Safety::Dangerous`] for unknown
    /// tools so an unrecognized name can never be auto-approved).
    pub fn safety(&self, name: &str, args: &Value) -> Safety {
        match self.get(name) {
            Some(tool) => tool.safety(args),
            None => Safety::Dangerous,
        }
    }

    /// Whether a tool pauses the turn for user input rather than executing (#44).
    /// Unknown tools are never interactive.
    pub fn is_interactive(&self, name: &str) -> bool {
        self.get(name).is_some_and(Tool::interactive)
    }

    /// The per-turn read-dedupe key for a call (#458 RC5), or `None` for an unknown
    /// tool or one that isn't a content read.
    pub fn dedupe_key(&self, name: &str, args: &Value) -> Option<String> {
        self.get(name).and_then(|tool| tool.dedupe_key(args))
    }
}

/// Whether `name` is the `agent` delegation tool the loop intercepts to spawn a
/// scoped sub-agent (#234) rather than dispatching through [`Tool::run`].
pub fn is_subagent(name: &str) -> bool {
    name == crate::agent_tool::AGENT_TOOL_NAME
}

/// Per-tool action allow-lists for Phase 2B spec pruning (#1162), keyed by tool name.
///
/// Passed per call rather than stored on the registry, because the registry is
/// shared immutably through `ToolContext` and rebuilt every turn.
pub type ActionScope = HashMap<String, BTreeSet<String>>;

/// The actions of each dispatch tool that `mode` can actually invoke, for Phase 2B
/// spec pruning (#1162).
///
/// Derived from [`Tool::safety`] per action and the permission matrix — the same two
/// inputs the per-call gate uses — so a new action is classified automatically
/// instead of drifting against a hand-kept list.
///
/// # Why this does not break #947
///
/// The result varies with `mode`, so a mode switch changes a tool's advertised
/// bytes. That is safe because a mode switch **already** re-forms the tools block:
/// `advertised_tools` drops write-only tools entirely in Plan, so the block's
/// membership changes and the prefix is invalidated regardless. Pruning rides an
/// existing invalidation boundary rather than introducing a new one. Within a fixed
/// mode the output is deterministic and byte-stable.
///
/// Today a tool survives Plan on its read-only *floor* — `github` stays visible
/// because `pr_list` is ReadOnly — and its ten mutating actions are advertised
/// anyway, refused only when called. Pruning stops advertising what the mode cannot
/// run, which is a correctness gain on top of the byte saving.
pub fn action_scope_for_mode(
    registry: &ToolRegistry,
    mode: Mode,
    matrix: &PermissionMatrix,
) -> ActionScope {
    let mut scope = ActionScope::new();
    for t in registry.iter_tools() {
        let Some(declared) = t.action_params() else {
            continue;
        };
        let kept: BTreeSet<String> = declared
            .keys()
            .filter(|a| {
                let args = serde_json::json!({ "action": a });
                !matrix
                    .effective_cell(t.name(), mode, t.safety(&args))
                    .is_deny()
            })
            .map(|a| (*a).to_string())
            .collect();
        if !kept.is_empty() && kept.len() < declared.len() {
            scope.insert(t.name().to_string(), kept);
        }
    }
    scope
}

/// A tool's schema with its `action` enum and properties narrowed to `actions`
/// (RFC 0024 Phase 2B, #1162), or the full schema unchanged when the tool declares
/// no [`Tool::action_params`] or `actions` is `None`.
///
/// The output is a pure function of (`tool`, `actions`). `preserve_order` is **not**
/// enabled on `serde_json` in this workspace (verified: it pulls no `indexmap`), so
/// `Map` is a `BTreeMap` and object keys serialize in sorted order regardless of
/// insertion or removal order. Two calls with an equal `actions` set therefore
/// serialize byte-identically — the property #947 depends on, asserted by
/// `pruned_schema_is_byte_stable_across_calls`.
///
/// Unknown names in `actions` are ignored rather than rejected: the caller derives
/// them from mode/egress policy, and a policy naming an action a tool no longer has
/// should not break the turn.
pub fn scoped_parameters(tool: &dyn Tool, actions: Option<&BTreeSet<String>>) -> Value {
    let full = object_schema(tool.parameters());
    let (Some(actions), Some(declared)) = (actions, tool.action_params()) else {
        return full;
    };

    let kept: BTreeSet<&str> = declared
        .keys()
        .copied()
        .filter(|a| actions.contains(*a))
        .collect();
    if kept.is_empty() || kept.len() == declared.len() {
        return full;
    }

    let mut keep_props: BTreeSet<&str> = BTreeSet::from(["action"]);
    for a in &kept {
        keep_props.extend(declared[*a].iter().copied());
    }

    let mut out = full.clone();
    let Some(props) = out.get_mut("properties").and_then(Value::as_object_mut) else {
        return full;
    };
    props.retain(|k, _| keep_props.contains(k.as_str()));
    if let Some(e) = props
        .get_mut("action")
        .and_then(|a| a.get_mut("enum"))
        .and_then(Value::as_array_mut)
    {
        e.retain(|v| v.as_str().is_some_and(|s| kept.contains(s)));
    }
    out
}

/// Coerce a tool's declared schema into a well-formed JSON Schema object (#1191).
///
/// A tool that takes no arguments is tempting to declare as `{}`, and `skills` did.
/// Strict providers reject that outright — `"schema must be a JSON Schema of 'type:
/// \"object\"', got 'type: null'"` fails the *whole* request, not just that tool —
/// while Anthropic and Bedrock never see it, because their own
/// `normalize_object_schema` repairs it on the way out. One registry was therefore
/// fine on one provider and unusable on another.
///
/// Coercing here, where every provider-agnostic schema is produced, rather than in
/// each provider: adding it to `openai.rs` and `ollama.rs` would leave two call
/// sites to keep in sync and every future provider wrong by default. The existing
/// Anthropic/Bedrock normalizers stay as harmless idempotent second passes.
///
/// A schema that already declares `type` is returned **untouched**, so the
/// byte-identical guarantee `pruned_schema_is_byte_stable_across_calls` asserts is
/// unaffected for every already-correct tool.
fn object_schema(params: Value) -> Value {
    match params {
        Value::Object(map) if !map.contains_key("type") => {
            let mut m = map;
            m.insert("type".into(), Value::String("object".into()));
            m.entry("properties")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            Value::Object(m)
        }
        other => other,
    }
}

/// Assert that `tool`'s [`Tool::action_params`] declaration is coherent with its
/// own schema (RFC 0024 Phase 2B, #1162).
///
/// Three invariants, each a pure data comparison against [`Tool::parameters`] —
/// no source parsing, so there is nothing here that can silently mis-read the
/// dispatch code the way an ad-hoc probe does:
///
/// 1. The declared action set equals the schema's `action` enum. Adding an action
///    without declaring its parameters fails here rather than shipping a tool
///    whose new action advertises no arguments.
/// 2. Every declared property exists in the schema. Catches typos and properties
///    renamed in the schema but not in the declaration.
/// 3. No schema property is orphaned — every one is claimed by at least one
///    action. This is the check that catches a property dropped from *all*
///    declarations, which is the shape that silently removes a capability.
///
/// **What this cannot catch:** property `X` omitted from action `A` while another
/// action still claims it. Invariant 3 sees `X` as used and says nothing. No
/// data-only check can see that, because the ground truth lives in the dispatch
/// code. That gap is covered per tool by asserting the scoped schema for a
/// specific action contains the properties that action's documented behaviour
/// requires — see `github_action_params_cover_known_dispatch_reads`.
#[cfg(test)]
pub fn assert_action_params_coherent(tool: &dyn Tool) {
    let Some(declared) = tool.action_params() else {
        return;
    };
    let schema = tool.parameters();
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{}: parameters() has no properties object", tool.name()));

    let enum_actions: BTreeSet<&str> = props
        .get("action")
        .and_then(|a| a.get("enum"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "{}: declares action_params but has no action enum",
                tool.name()
            )
        })
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let declared_actions: BTreeSet<&str> = declared.keys().copied().collect();
    assert_eq!(
        declared_actions,
        enum_actions,
        "{}: action_params keys must match the schema's action enum exactly",
        tool.name()
    );

    let mut claimed: BTreeSet<&str> = BTreeSet::new();
    for (action, params) in &declared {
        for p in *params {
            assert!(
                props.contains_key(*p),
                "{}: action {action:?} declares property {p:?}, which is not in the schema",
                tool.name()
            );
            claimed.insert(p);
        }
    }

    let orphans: Vec<&str> = props
        .keys()
        .map(String::as_str)
        .filter(|p| *p != "action" && !claimed.contains(p))
        .collect();
    assert!(
        orphans.is_empty(),
        "{}: schema advertises {orphans:?}, which no action claims — either wire them \
         into the action that reads them, or drop them from the schema. A property no \
         action claims is pruned out of every scoped schema, so the capability \
         disappears silently.",
        tool.name()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_tool_is_error_not_panic() {
        let reg = ToolRegistry::with_defaults();
        let out = reg.run("nope", serde_json::json!({}), Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn unknown_tool_error_lists_available_tools() {
        let reg = ToolRegistry::with_defaults();
        let out = reg
            .run("codegraph_explore", serde_json::json!({}), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(out.content.contains("unknown tool: codegraph_explore"));
        assert!(
            out.content.contains("Available tools:"),
            "error should name the registered tools so the model can self-correct"
        );
        // Every registered tool is named, in sorted order.
        let mut expected: Vec<&str> = reg.tools.keys().map(String::as_str).collect();
        expected.sort_unstable();
        assert!(!expected.is_empty(), "default registry has tools");
        for name in &expected {
            assert!(
                out.content.contains(name),
                "available-tools hint should include {name}"
            );
        }
    }

    #[test]
    fn advertises_default_schemas() {
        let reg = ToolRegistry::with_defaults();
        let tools = reg.openai_tools();
        assert_eq!(tools.len(), 17);
        let names: Vec<_> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        for expected in [
            "bash",
            "python",
            "view",
            "edit",
            "write",
            "apply_patch",
            "grep",
            "glob",
            "tree",
            "todo",
            "web_fetch",
            "ask_user",
            "agent",
            "git",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn openai_tools_for_honors_allowlist_and_depth() {
        let reg = ToolRegistry::with_defaults();

        let allowed: HashSet<String> = ["view", "grep"].iter().map(|s| s.to_string()).collect();
        let restricted = reg.openai_tools_for(Some(&allowed), true, None);
        let names: Vec<_> = restricted
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"view") && names.contains(&"grep"));

        // At the depth cap the delegation tool is not advertised at all.
        let no_subagent = reg.openai_tools_for(None, false, None);
        let names: Vec<_> = no_subagent
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"agent"));
        assert_eq!(no_subagent.len(), 16);
    }

    // #947: the serialized tool order must be stable and name-sorted. The
    // registry stores tools in a `HashMap` (random per-instance iteration
    // order) and is rebuilt every turn, so an unsorted array would reorder the
    // `tools` block each turn and bust the provider's cached prompt prefix
    // (`cacheRead == 0`, full cold prefill every turn -> ~21s TTBF). Sorting
    // keeps the prefix byte-identical across turns so the cache hits.
    #[test]
    fn openai_tools_order_is_stable_and_sorted() {
        // Two independently-built registries (distinct HashMap seeds) must
        // serialize tools in the same order.
        let names_a: Vec<String> = ToolRegistry::with_defaults()
            .openai_tools()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        let names_b: Vec<String> = ToolRegistry::with_defaults()
            .openai_tools()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names_a, names_b, "tool order must be stable across builds");

        // ...and that stable order is name-sorted.
        let mut sorted = names_a.clone();
        sorted.sort();
        assert_eq!(names_a, sorted, "tool order must be name-sorted");

        // The allowlisted subset is sorted too (it feeds the same cached prefix).
        let allowed: HashSet<String> = ["write", "grep", "bash", "view"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let restricted: Vec<String> = ToolRegistry::with_defaults()
            .openai_tools_for(Some(&allowed), true, None)
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(restricted, vec!["bash", "grep", "view", "write"]);
    }

    #[test]
    fn search_and_plan_tools_are_read_only() {
        let reg = ToolRegistry::with_defaults();
        for name in ["grep", "glob", "tree", "todo"] {
            assert_eq!(
                reg.safety(name, &serde_json::json!({})),
                Safety::ReadOnly,
                "{name} should be read-only"
            );
        }
    }

    #[test]
    fn readonly_tool_names_excludes_mutating_and_dynamic_tools() {
        let reg = ToolRegistry::with_defaults();
        let ro = reg.readonly_tool_names();

        // Every ReadOnly-ceiling default tool is present.
        for name in [
            "view",
            "grep",
            "glob",
            "tree",
            "todo",
            "ask_user",
            "diagnostics",
        ] {
            assert!(ro.contains(name), "{name} should be a read-only tool");
        }
        // Tools whose *ceiling* exceeds ReadOnly are absent — even bash/github,
        // which have a read-only floor but can mutate (that read floor is exposed
        // via `readonly_capable_names`, not here). web_fetch is Sensitive (egress).
        for name in [
            "bash",
            "python",
            "edit",
            "write",
            "apply_patch",
            "web_fetch",
            "agent",
        ] {
            assert!(
                !ro.contains(name),
                "{name} must not be a read-only-ceiling tool"
            );
        }
    }

    #[test]
    fn readonly_capable_includes_read_floor_tools_but_not_pure_mutators() {
        // #793: the Plan-advertised base is tools with a ReadOnly *floor* — the
        // always-read tools plus bash/github (which have read-only calls).
        let reg = ToolRegistry::with_defaults();
        let cap = reg.readonly_capable_names();
        for name in ["view", "grep", "glob", "tree", "todo", "bash", "github"] {
            assert!(cap.contains(name), "{name} has a read-only floor");
        }
        // No read-only path -> not in the capable set.
        for name in [
            "python",
            "edit",
            "write",
            "apply_patch",
            "web_fetch",
            "agent",
        ] {
            assert!(!cap.contains(name), "{name} has no read-only floor");
        }
    }

    #[test]
    fn local_tool_names_excludes_network_capable_and_is_fail_safe() {
        // RFC 0013: the LocalOnly advertised base is tools with no egress path.
        let reg = ToolRegistry::with_defaults();
        let local = reg.local_tool_names();
        // Proven-local built-ins are present.
        for name in [
            "view",
            "edit",
            "write",
            "apply_patch",
            "grep",
            "glob",
            "tree",
            "todo",
            "diagnostics",
            "git",
            "ask_user",
            "agent",
        ] {
            assert!(local.contains(name), "{name} should be classified local");
        }
        // Network-capable / arbitrary-exec tools are excluded (fail-safe true default).
        for name in [
            "bash",
            "python",
            "web_fetch",
            "web_search",
            "github",
            "test_runner",
        ] {
            assert!(
                !local.contains(name),
                "{name} must be treated as network-capable"
            );
        }
    }

    #[test]
    fn reaches_network_default_is_fail_safe_true() {
        // A tool that doesn't override reaches_network is treated as network-capable.
        assert!(crate::bash::BashTool.reaches_network());
        assert!(crate::web_fetch::WebFetchTool::new().reaches_network());
        // Proven-local overrides return false.
        assert!(!crate::view::ViewTool.reaches_network());
        assert!(!crate::grep::GrepTool.reaches_network());
    }

    #[test]
    fn min_safety_defaults_to_ceiling_and_is_overridden_for_dynamic_tools() {
        // Fixed-safety tools have floor == ceiling; bash/github drop their floor to
        // ReadOnly (their list/read calls) while keeping a higher ceiling.
        assert_eq!(crate::bash::BashTool.min_safety(), Safety::ReadOnly);
        assert_eq!(crate::bash::BashTool.max_safety(), Safety::Dangerous);
        assert_eq!(crate::github::GithubTool.min_safety(), Safety::ReadOnly);
        assert_eq!(crate::github::GithubTool.max_safety(), Safety::Publish);
        // web_fetch has no read-only path: floor == ceiling == Sensitive.
        assert_eq!(
            crate::web_fetch::WebFetchTool::new().min_safety(),
            Safety::Sensitive
        );
    }

    #[test]
    fn unknown_tool_safety_is_dangerous() {
        let reg = ToolRegistry::with_defaults();
        assert_eq!(
            reg.safety("nope", &serde_json::json!({})),
            Safety::Dangerous
        );
    }

    #[test]
    fn dedupe_key_only_for_read_tools() {
        // #458 RC5: `view` exposes a read identity; non-read tools and unknown names
        // return None, so the per-turn dedupe is scoped to file reads.
        let reg = ToolRegistry::with_defaults();
        assert_eq!(
            reg.dedupe_key("view", &serde_json::json!({"path": "a.rs"})),
            Some("a.rs".to_string())
        );
        assert_eq!(
            reg.dedupe_key("bash", &serde_json::json!({"command": "ls"})),
            None
        );
        assert_eq!(reg.dedupe_key("nope", &serde_json::json!({})), None);
    }

    fn scope_of<const N: usize>(tool: &str, actions: [&str; N]) -> ActionScope {
        ActionScope::from([(
            tool.to_string(),
            actions.iter().map(|a| (*a).to_string()).collect(),
        )])
    }

    fn github_entry(reg: &ToolRegistry, scope: Option<&ActionScope>) -> Value {
        reg.openai_tools_for(None, true, scope)
            .into_iter()
            .find(|t| t["function"]["name"] == "github")
            .expect("github is registered")
    }

    fn github_schema_bytes(reg: &ToolRegistry, scope: Option<&ActionScope>) -> usize {
        serde_json::to_string(&github_entry(reg, scope)["function"]["parameters"])
            .expect("schema serializes")
            .len()
    }

    #[test]
    fn scoping_actions_prunes_the_advertised_schema() {
        let reg = ToolRegistry::with_defaults();
        let full = github_schema_bytes(&reg, None);

        let scope = scope_of("github", ["pr_view", "pr_checks"]);
        let pruned = github_schema_bytes(&reg, Some(&scope));

        assert!(
            pruned < full / 2,
            "scoping github to two read actions should cut the schema by more than half, \
             got {pruned} from {full}"
        );

        let entry = github_entry(&reg, Some(&scope));
        let props = entry["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties object");
        assert!(
            props.contains_key("diff"),
            "pr_view reads diff; it must survive pruning"
        );
        assert!(
            props.contains_key("number"),
            "both kept actions read number"
        );
        assert!(
            !props.contains_key("force"),
            "force belongs only to push, which was scoped out"
        );

        let actions = entry["function"]["parameters"]["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum");
        assert_eq!(
            actions.len(),
            2,
            "the enum must narrow to the scoped actions"
        );
    }

    // ---- Schema well-formedness at the advertising boundary (#1191) ----
    //
    // `skills` shipped with `parameters()` returning a bare `{}`, which strict
    // providers reject outright: "schema must be a JSON Schema of type object, got
    // type: null" 400s the whole request, not just that tool. Anthropic and Bedrock
    // never saw it -- they run `normalize_object_schema` -- while the OpenAI and
    // Ollama paths take schemas verbatim from `scoped_parameters`, so the same
    // registry was fine on one provider and unusable on another.

    struct BareSchemaTool;

    #[async_trait]
    impl Tool for BareSchemaTool {
        fn name(&self) -> &str {
            "bare"
        }
        fn description(&self) -> &str {
            "Declares an empty schema, as `skills` did."
        }
        fn parameters(&self) -> Value {
            serde_json::json!({})
        }
        async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
            ToolOutcome::ok("ran")
        }
    }

    #[test]
    fn every_advertised_tool_declares_an_object_schema() {
        // The invariant no per-tool unit test can hold: the bug was one tool
        // disagreeing with the other 31, which is only visible in aggregate.
        let reg = ToolRegistry::with_defaults();
        let offenders: Vec<String> = reg
            .openai_tools()
            .iter()
            .filter_map(|entry| {
                let f = &entry["function"];
                let params = &f["parameters"];
                let ok = params.get("type").and_then(Value::as_str) == Some("object")
                    && params.get("properties").is_some_and(Value::is_object);
                (!ok).then(|| {
                    format!(
                        "{}: {}",
                        f["name"].as_str().unwrap_or("?"),
                        serde_json::to_string(params).unwrap_or_default()
                    )
                })
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "every advertised schema must be an object schema; offenders: {offenders:?}"
        );
    }

    #[test]
    fn scoped_parameters_coerces_a_schema_that_omits_type() {
        // The class fix: normalizing where schemas are produced means a future
        // no-arg tool cannot reintroduce this, and cannot reintroduce it invisibly
        // on providers whose own normalizer would have masked it.
        let params = scoped_parameters(&BareSchemaTool, None);
        assert_eq!(params["type"], "object");
        assert!(
            params["properties"].is_object(),
            "a coerced schema needs a properties object, got {params}"
        );
    }

    #[test]
    fn coercion_leaves_a_well_formed_schema_untouched() {
        // Already-correct schemas must be byte-identical on the wire: a normalizer
        // that rewrites valid schemas would break #947's append-only guarantee
        // just as surely as a missing one breaks strict providers.
        //
        // Every tool in `with_defaults()` (17), not a hand-picked sample -- the
        // sample cannot notice a tool whose schema the coercion starts touching.
        // The desktop-only tools (`skills` and siblings) are covered by
        // `every_desktop_tool_advertises_an_object_schema`, which sees a registry
        // this one structurally cannot.
        let reg = ToolRegistry::with_defaults();
        for tool in reg.iter_tools() {
            let tool_name = tool.name();
            let raw = serde_json::to_string(&tool.parameters()).unwrap();
            let advertised = serde_json::to_string(&scoped_parameters(tool, None)).unwrap();
            assert_eq!(
                raw, advertised,
                "{tool_name}'s schema already declares an object; coercion must not touch it"
            );
        }
    }

    struct NonObjectSchemaTool;

    #[async_trait]
    impl Tool for NonObjectSchemaTool {
        fn name(&self) -> &str {
            "nonobject"
        }
        fn description(&self) -> &str {
            "Declares a non-object schema."
        }
        fn parameters(&self) -> Value {
            serde_json::json!({ "type": "string" })
        }
        async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
            ToolOutcome::ok("ran")
        }
    }

    #[test]
    fn coercion_never_overwrites_a_declared_type() {
        // Only *absence* of `type` is repaired. Rewriting a declared one would make
        // the normalizer an opinion about schemas rather than a repair for a missing
        // key, and would silently reshape any tool that legitimately declares
        // something else.
        //
        // Needed because the obvious guard is untestable against real tools: every
        // built-in already declares both `type` and `properties`, so a mutation that
        // drops the `!contains_key("type")` condition produces a byte-identical
        // result for all of them and survives. Found by mutation, then pinned with a
        // schema whose type is not `object`.
        let params = scoped_parameters(&NonObjectSchemaTool, None);
        assert_eq!(
            params["type"], "string",
            "a declared type must survive coercion untouched, got {params}"
        );
        assert!(
            params.get("properties").is_none(),
            "coercion must not graft properties onto a schema it did not repair"
        );
    }

    #[test]
    fn pruned_schema_is_byte_stable_across_calls() {
        // The #947 contract: a tool's advertised bytes must not move between turns.
        let reg = ToolRegistry::with_defaults();
        let scope = scope_of("github", ["pr_view", "pr_merge"]);

        let a = serde_json::to_string(&reg.openai_tools_for(None, true, Some(&scope)))
            .expect("serializes");
        let b = serde_json::to_string(&reg.openai_tools_for(None, true, Some(&scope)))
            .expect("serializes");
        assert_eq!(a, b, "the tools block must be byte-identical across turns");
    }

    #[test]
    fn scoping_does_not_reorder_or_drop_tools() {
        // Pruning changes one tool's schema; it must not perturb the block's
        // membership or order, which is what #947's append-only guarantee rests on.
        let reg = ToolRegistry::with_defaults();
        let names = |scope: Option<&ActionScope>| -> Vec<String> {
            reg.openai_tools_for(None, true, scope)
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap().to_string())
                .collect()
        };
        let scope = scope_of("github", ["pr_view"]);
        assert_eq!(
            names(None),
            names(Some(&scope)),
            "scoping must not add, drop, or reorder tools"
        );
    }

    #[test]
    fn unscoped_tools_are_untouched() {
        let reg = ToolRegistry::with_defaults();
        let full = github_schema_bytes(&reg, None);
        let scope = scope_of("git", ["status"]);
        assert_eq!(
            github_schema_bytes(&reg, Some(&scope)),
            full,
            "scoping git must not change what github advertises"
        );
    }

    #[test]
    fn scoping_every_action_is_a_no_op() {
        // Guards a subtle regression: if pruning rebuilt the schema instead of
        // filtering it, an all-actions scope would still shift bytes.
        let reg = ToolRegistry::with_defaults();
        let full = github_schema_bytes(&reg, None);
        let all: BTreeSet<String> = crate::github::GithubTool
            .action_params()
            .expect("github declares action_params")
            .keys()
            .map(|s| (*s).to_string())
            .collect();
        let scope = ActionScope::from([("github".to_string(), all)]);
        assert_eq!(github_schema_bytes(&reg, Some(&scope)), full);
    }

    #[test]
    fn scoping_an_unknown_action_leaves_the_schema_whole() {
        // A stale policy naming an action github no longer has must not silently
        // strip the tool down to nothing mid-session.
        let reg = ToolRegistry::with_defaults();
        let full = github_schema_bytes(&reg, None);
        let scope = scope_of("github", ["pr_teleport"]);
        assert_eq!(
            github_schema_bytes(&reg, Some(&scope)),
            full,
            "an all-unknown scope must fall back to the full schema, not an empty one"
        );
    }

    #[test]
    fn plan_mode_scope_drops_mutating_actions_and_keeps_reads() {
        // The production derivation: same two inputs as the per-call gate.
        let reg = ToolRegistry::with_defaults();
        let matrix = PermissionMatrix::default();
        let scope = action_scope_for_mode(&reg, Mode::Plan, &matrix);

        let gh = scope.get("github").expect("github is scoped in Plan");
        for kept in [
            "pr_view",
            "pr_list",
            "pr_checks",
            "issue_view",
            "issue_list",
        ] {
            assert!(gh.contains(kept), "Plan must keep the read action {kept:?}");
        }
        for dropped in ["push", "pr_merge", "pr_create", "issue_edit", "pr_comment"] {
            assert!(
                !gh.contains(dropped),
                "Plan refuses {dropped:?} when called, so it must not be advertised"
            );
        }

        // git is all-read, so every action survives and no entry is emitted at all —
        // an unpruned tool must not pay a pruning code path.
        assert!(
            !scope.contains_key("git"),
            "git's four actions are all ReadOnly; it needs no scope entry"
        );
    }

    #[test]
    fn act_mode_scope_prunes_nothing() {
        let reg = ToolRegistry::with_defaults();
        let matrix = PermissionMatrix::default();
        let scope = action_scope_for_mode(&reg, Mode::Act, &matrix);
        assert!(
            scope.is_empty(),
            "Act denies no action of a dispatch tool, so there is nothing to prune: {scope:?}"
        );
    }

    #[test]
    fn plan_scope_cuts_real_bytes() {
        let reg = ToolRegistry::with_defaults();
        let matrix = PermissionMatrix::default();
        let scope = action_scope_for_mode(&reg, Mode::Plan, &matrix);
        let full = github_schema_bytes(&reg, None);
        let pruned = github_schema_bytes(&reg, Some(&scope));
        assert!(
            pruned * 2 < full,
            "Plan keeps 7 of github's 17 actions; expected well under half the bytes, \
             got {pruned} from {full}"
        );
    }

    /// The four dispatch tools as the desktop app registers them: `with_defaults`
    /// supplies `github` and `git`; `process_manager` and `notebook_runner` are added
    /// by `AppState::build_tool_registry` because they need live supervisors.
    fn desktop_like_registry() -> ToolRegistry {
        use std::sync::Arc;
        let mut reg = ToolRegistry::with_defaults();
        reg.register(Box::new(crate::process::ProcessManagerTool::new(Arc::new(
            crate::process::ProcessSupervisor::new(),
        ))));
        reg.register(Box::new(crate::notebook::NotebookTool::new(Arc::new(
            crate::notebook::KernelSupervisor::new(),
        ))));
        reg
    }

    #[test]
    fn plan_mode_pruning_cuts_the_whole_tools_block() {
        // Pins the saving this phase exists for, measured on the tool set the app
        // actually ships. Without this the number lives only in a PR description and
        // silently rots — the mistake called out on #1107's semantic-recall figures.
        let reg = desktop_like_registry();
        let matrix = PermissionMatrix::default();
        let scope = action_scope_for_mode(&reg, Mode::Plan, &matrix);

        let full = serde_json::to_string(&reg.openai_tools_for(None, true, None))
            .expect("serializes")
            .len();
        let pruned = serde_json::to_string(&reg.openai_tools_for(None, true, Some(&scope)))
            .expect("serializes")
            .len();

        // Measured 20213 -> 16820 B (16.8%). Asserting a floor of 12% leaves room for
        // tools to be added or reworded without churn, while still failing outright if
        // pruning stops happening.
        let saved_pct = 100.0 * (1.0 - pruned as f64 / full as f64);
        assert!(
            saved_pct > 12.0,
            "Plan-mode pruning should cut the tools block by >12%, got {saved_pct:.1}% \
             ({full} -> {pruned} B)"
        );

        // Three of the four are scoped in Plan; git is all-ReadOnly so it keeps
        // everything and must not appear.
        let mut scoped: Vec<&str> = scope.keys().map(String::as_str).collect();
        scoped.sort_unstable();
        assert_eq!(scoped, ["github", "notebook_runner", "process_manager"]);
    }
}
