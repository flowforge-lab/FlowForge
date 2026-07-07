//! Data-driven model capability specs (#457).
//!
//! A single ordered rule set describes per-model properties — today the context
//! window, with room for vision support and other quirks — so adding or
//! correcting one is a data edit, not a code change. A flat per-vendor rule
//! cannot track reality: served context windows span 96K–1M *within* a vendor
//! family (e.g. `GLM-4.5-Air` 98,304 vs `GLM-5.2` 1,048,576) and drift with
//! every point release, so that churn belongs in data.
//!
//! This module owns the **schema, the bundled defaults, and the pure lookups**.
//! It does no I/O: the optional user-override layer (reading a config-dir file
//! and merging it ahead of the bundled rules) lives in `ff-llm`, which feeds the
//! merged rule slice back into [`context_window_in`]. Keeping the pure layer here
//! lets capability lookups in *other* crates (e.g. the vision gate in
//! [`crate::provider`]) share one schema and one bundled file without a reverse
//! dependency on `ff-llm`.
//!
//! ## Matching
//! `match` is tested against the model id per the rule's `match_kind`
//! (case-insensitive; substring by default, `prefix`-anchored on request, #473)
//! through the single shared [`ModelSpec::matches`] so the window and vision
//! lookups cannot drift. Rules are evaluated
//! **first-match-wins**, so the list is ordered most-specific-first (e.g.
//! `glm-4.5-air` before `glm`). Per-field absent semantics differ by capability
//! and are documented on each lookup:
//! - [`context_window_in`]: the first matching rule that carries a
//!   `context_window` wins; unknowns fall through to
//!   [`DEFAULT_CONTEXT_WINDOW_TOKENS`]. Provider-agnostic — the window is a
//!   property of the model, not the transport, so `provider` is ignored here.
//! - [`supports_vision_in`]: provider-scoped (the `provider` field is consulted;
//!   `None` matches any) and fail-closed -- `false` unless some matching rule
//!   carries `supports_vision: true`. Unlike the window lookup it is an OR over
//!   all matching rules, not first-match-wins (#466).

use serde::Deserialize;

use crate::ProviderKind;

/// Conservative fallback window for a model with no matching rule. Small on
/// purpose: undersizing throttles a capable model but never overflows it, while
/// oversizing lets a request blow past the real cap before compaction engages.
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 32_000;

/// Bundled defaults, seeded from live SiliconFlow probes (2026-06-24) plus
/// official Anthropic/OpenAI windows. Trusted input: a parse failure here is a
/// build/CI defect, caught by [`tests::bundled_rules_parse_and_lookup`].
const DEFAULT_JSON: &str = include_str!("model-specs.default.json");

/// How a rule's `match` pattern is tested against a (lowercased) model id (#473).
/// Defaults to [`Contains`](MatchKind::Contains) so existing rules and any
/// user-override file written against the older schema keep their behavior; a
/// short pattern that would over-match as a substring (e.g. OpenAI `o1`/`o3`,
/// which must not match `proto1`) sets `prefix` to anchor at the start.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    /// `match` may appear anywhere in the id (the historical default).
    #[default]
    Contains,
    /// The id must start with `match`. Mirrors the legacy `starts_with` arm.
    Prefix,
}

/// One capability rule. `match` is tested against the model id per `match_kind`
/// (case-insensitive, substring by default). Capability fields are optional so a
/// rule can describe only what it knows; new fields (`max_output`, pricing, …)
/// can be added later with `#[serde(default)]` without breaking files written
/// against an older schema (serde also ignores unknown fields on the way in).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    #[serde(rename = "match")]
    pattern: String,
    /// How `pattern` is tested against the model id (#473). Defaults to
    /// `contains`; set `prefix` to anchor a short pattern that would otherwise
    /// over-match. Both capability lookups route through [`ModelSpec::matches`],
    /// so the choice applies consistently to the window and vision lookups.
    #[serde(default)]
    match_kind: MatchKind,
    /// Context window in tokens. Absent on a rule that describes only other
    /// capabilities (e.g. a future vision-only rule).
    #[serde(default)]
    context_window: Option<u64>,
    /// Provider scope for the rule (#466). The same model substring can mean
    /// different things per provider (`-vl` is vision on SiliconFlow, nothing on
    /// Bedrock), so vision rules are keyed on this; `None` means "any provider".
    /// Consulted by [`supports_vision_in`]; ignored by the provider-agnostic
    /// [`context_window_in`].
    #[serde(default)]
    provider: Option<ProviderKind>,
    /// Whether the model accepts image/document input (#466). Backs the vision
    /// gate via [`supports_vision_in`], fail-closed: absent/`None` means "unknown"
    /// and is treated as `false`. A rule must set this to `true` to grant vision.
    #[serde(default)]
    supports_vision: Option<bool>,
}

/// A parsed rule set (bundled defaults or a user override).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpecs {
    pub rules: Vec<ModelSpec>,
}

/// Parse a rule set from JSON. Used by the `ff-llm` override layer to read the
/// user's on-disk file into the shared schema.
pub fn parse_specs(json: &str) -> Result<ModelSpecs, serde_json::Error> {
    serde_json::from_str(json)
}

impl ModelSpec {
    /// The single matcher both capability lookups consult, so substring/anchor
    /// semantics cannot drift between the window and vision paths (#473).
    /// `model_lower` must already be lowercased (each lookup lowercases once).
    fn matches(&self, model_lower: &str) -> bool {
        let pattern = self.pattern.to_lowercase();
        match self.match_kind {
            MatchKind::Contains => model_lower.contains(&pattern),
            MatchKind::Prefix => model_lower.starts_with(&pattern),
        }
    }
}

/// The compiled-in default rules, parsed once.
pub fn bundled_rules() -> &'static [ModelSpec] {
    use std::sync::OnceLock;
    static BUNDLED: OnceLock<Vec<ModelSpec>> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        parse_specs(DEFAULT_JSON)
            .expect("bundled model-specs.default.json is malformed")
            .rules
    })
}

/// First-match-wins context-window lookup over an ordered rule slice. Returns the
/// `context_window` of the first matching rule that carries one; rules without a
/// window (capability-only) are skipped so they cannot shadow a later windowed
/// rule. Provider-agnostic by design. Unknowns fall through to
/// [`DEFAULT_CONTEXT_WINDOW_TOKENS`].
pub fn context_window_in(rules: &[ModelSpec], model: &str) -> u64 {
    let m = model.to_lowercase();
    rules
        .iter()
        .filter(|r| r.matches(&m))
        .find_map(|r| r.context_window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
}

/// Provider-scoped, fail-closed vision capability lookup over a rule slice (#466).
/// Returns `true` only when some rule matches *and* carries `supports_vision:
/// true`; a rule's `provider` must be `None` (any) or equal `kind`. This is an OR
/// over all matching rules (not first-match-wins): the question is whether *any*
/// rule grants vision for `(kind, model)`. Window-only rules (`supports_vision:
/// None`) never grant or block vision, so they cannot interfere. Conservative by
/// design -- an unknown model returns `false`, and the FE attach gate and provider
/// safety strip both fail closed on `false`.
pub fn supports_vision_in(rules: &[ModelSpec], kind: ProviderKind, model: &str) -> bool {
    let m = model.to_lowercase();
    rules.iter().any(|r| {
        r.supports_vision == Some(true) && r.provider.is_none_or(|p| p == kind) && r.matches(&m)
    })
}

#[cfg(test)]
mod tests;
