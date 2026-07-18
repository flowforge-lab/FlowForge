//! `web_search` — query a configured web-search backend and return ranked results.
//!
//! Network egress, so it is `Safety::Sensitive` (approval-gated by the agent loop). The
//! tool reads the user's persisted [`SearchConfig`](ff_core::SearchConfig) at call
//! time. The default is keyless [`Tavily`](ff_core::SearchBackend::Tavily), which
//! works with zero setup; the keyless, self-hosted SearXNG JSON API is also wired.
//! The hosted `Brave` / `OpenAiCompatible` backends are recognized but refused with
//! a clear message until API-key storage lands (Issue #8).
//!
//! A configured SearXNG `base_url` is itself an SSRF vector (it could point at
//! internal infra), so every request — including Tavily's fixed endpoint — is
//! validated by [`SsrfPolicy`](crate::SsrfPolicy): both the literal URL and, for
//! named hosts, the resolved IP, before connecting.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Safety, SsrfPolicy, Tool, ToolOutcome};
use async_trait::async_trait;
use ff_core::{SearchBackend, SearchConfig};
use reqwest::header::USER_AGENT;
use reqwest::redirect::Policy;
use serde_json::Value;
use url::Url;

/// Per-request timeout.
const TIMEOUT_SECS: u64 = 15;
const UA: &str = "FlowForge/0.1 (+web_search)";
/// Tavily's search endpoint. A fixed, vetted HTTPS host (not user-supplied).
const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";
/// Default number of results when the caller omits `limit`.
const DEFAULT_LIMIT: usize = 5;
/// Hard cap on results regardless of the requested `limit`.
const MAX_LIMIT: usize = 10;
/// Per-result snippet cap (chars) so one verbose result can't dominate the output.
const SNIPPET_CHARS: usize = 300;

/// A single search hit, backend-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// A web-search backend. Abstracted so the tool can be tested without network
/// access (see `MockSearchProvider` in tests).
#[async_trait]
trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String>;
}

/// Keyless SearXNG backend, queried via its JSON API (`/search?format=json`).
/// Validates its endpoint with an [`SsrfPolicy`] before each request.
struct SearxngProvider {
    base_url: String,
    policy: SsrfPolicy,
}

impl SearxngProvider {
    fn new(base_url: String) -> Self {
        Self {
            base_url,
            policy: SsrfPolicy::strict(),
        }
    }

    /// Build the `{base}/search?q=..&format=json` request URL.
    fn request_url(&self, query: &str) -> Result<Url, String> {
        let trimmed = self.base_url.trim_end_matches('/');
        let mut url = Url::parse(&format!("{trimmed}/search"))
            .map_err(|e| format!("invalid SearXNG base_url `{}`: {e}", self.base_url))?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("format", "json");
        Ok(url)
    }
}

#[async_trait]
impl SearchProvider for SearxngProvider {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        let url = self.request_url(query)?;
        // SSRF guard: validate the literal URL and the resolved host before connect.
        let checked = self.policy.check_url(url.as_str())?;
        self.policy.check_host(&checked).await?;

        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let resp = client
            .get(checked)
            .header(USER_AGENT, UA)
            .send()
            .await
            .map_err(|e| format!("search request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(format!("search endpoint returned HTTP {status}"));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("failed to read search response: {e}"))?;
        parse_results(&body, limit)
    }
}

/// Keyless Tavily backend, queried via its JSON API (`POST /search`). The endpoint
/// is fixed and vetted, but is still SSRF-checked for defense in depth and to keep
/// the request path uniform with SearXNG.
struct TavilyProvider {
    endpoint: String,
    policy: SsrfPolicy,
    /// Optional API key (raises the rate limit). Keyless when `None`.
    key: Option<String>,
}

impl TavilyProvider {
    fn new(key: Option<String>) -> Self {
        Self {
            endpoint: TAVILY_ENDPOINT.to_string(),
            policy: SsrfPolicy::strict(),
            key: key.filter(|k| !k.trim().is_empty()),
        }
    }
}

#[async_trait]
impl SearchProvider for TavilyProvider {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        // SSRF guard: validate the literal URL and the resolved host before connect.
        let checked = self.policy.check_url(&self.endpoint)?;
        self.policy.check_host(&checked).await?;

        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let mut req = client.post(checked).header(USER_AGENT, UA);
        // An API key raises the rate limit via Bearer auth; without one, the keyless
        // access-mode header is mandatory (Tavily returns 401 otherwise).
        req = match &self.key {
            Some(k) => req.bearer_auth(k),
            None => req.header("X-Tavily-Access-Mode", "keyless"),
        };
        let resp = req
            .json(&serde_json::json!({ "query": query, "max_results": limit }))
            .send()
            .await
            .map_err(|e| format!("search request failed: {e}"))?;

        let status = resp.status();
        // Fail loud on the keyless rate cap so the remedy is obvious rather than the
        // tool silently returning nothing.
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(
                "Tavily keyless hourly rate limit reached. Wait a few minutes \
                 and retry, or add a free Tavily API key in Settings → Search to \
                 raise the limit."
                    .to_string(),
            );
        }
        if !status.is_success() {
            return Err(format!("search endpoint returned HTTP {status}"));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("failed to read search response: {e}"))?;
        parse_results(&body, limit)
    }
}

/// Brave Search API (`GET /res/v1/web/search?q=`), authenticated with an
/// `X-Subscription-Token` header (#1010). Fixed vetted HTTPS host, still SSRF-checked
/// for defense in depth. Its response shape differs from Tavily/SearXNG
/// (`web.results[]` with `title`/`url`/`description`), so it parses separately.
struct BraveProvider {
    key: String,
    policy: SsrfPolicy,
}

impl BraveProvider {
    fn new(key: String) -> Self {
        Self {
            key,
            policy: SsrfPolicy::strict(),
        }
    }
}

#[async_trait]
impl SearchProvider for BraveProvider {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        let url = url::Url::parse_with_params(
            "https://api.search.brave.com/res/v1/web/search",
            &[("q", query), ("count", &limit.to_string())],
        )
        .map_err(|e| format!("failed to build Brave URL: {e}"))?;
        let checked = self.policy.check_url(url.as_str())?;
        self.policy.check_host(&checked).await?;

        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let resp = client
            .get(checked)
            .header(USER_AGENT, UA)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.key)
            .send()
            .await
            .map_err(|e| format!("search request failed: {e}"))?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err("Brave Search rate limit reached. Wait and retry.".to_string());
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(
                "Brave Search rejected the API key (401/403). Check the key in \
                 Settings → Search."
                    .to_string(),
            );
        }
        if !status.is_success() {
            return Err(format!("search endpoint returned HTTP {status}"));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("failed to read search response: {e}"))?;
        parse_brave_results(&body, limit)
    }
}

/// An OpenAI-compatible search endpoint (`POST {base_url}`), authenticated with a
/// Bearer key (#1010). The `base_url` is user-supplied, so it is SSRF-checked like
/// SearXNG. Assumes the common `results[]` (`title`/`url`/`content`) response shape.
struct OpenAiCompatibleProvider {
    base_url: String,
    key: String,
    policy: SsrfPolicy,
}

impl OpenAiCompatibleProvider {
    fn new(base_url: String, key: String) -> Self {
        Self {
            base_url,
            key,
            policy: SsrfPolicy::strict(),
        }
    }
}

#[async_trait]
impl SearchProvider for OpenAiCompatibleProvider {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        let checked = self.policy.check_url(&self.base_url)?;
        self.policy.check_host(&checked).await?;

        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let resp = client
            .post(checked)
            .header(USER_AGENT, UA)
            .bearer_auth(&self.key)
            .json(&serde_json::json!({ "query": query, "max_results": limit }))
            .send()
            .await
            .map_err(|e| format!("search request failed: {e}"))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(
                "The search endpoint rejected the API key (401/403). Check the key in \
                 Settings → Search."
                    .to_string(),
            );
        }
        if !status.is_success() {
            return Err(format!("search endpoint returned HTTP {status}"));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("failed to read search response: {e}"))?;
        parse_results(&body, limit)
    }
}

/// Parse Brave's `web.results[]` (`title`/`url`/`description`) into capped results.
fn parse_brave_results(body: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| format!("invalid search JSON: {e}"))?;
    let results = json
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(Value::as_array)
        .ok_or_else(|| "Brave response has no `web.results` array".to_string())?;

    let out = results
        .iter()
        .filter_map(|r| {
            let url = r.get("url").and_then(Value::as_str)?.to_string();
            let title = r
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let snippet = cap_snippet(r.get("description").and_then(Value::as_str).unwrap_or(""));
            Some(SearchResult {
                title,
                url,
                snippet,
            })
        })
        .take(limit)
        .collect();
    Ok(out)
}

/// Parse a `results[]` JSON body (SearXNG `format=json` or Tavily — same field
/// names: `title`, `url`, `content`) into capped [`SearchResult`]s.
fn parse_results(body: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| format!("invalid search JSON: {e}"))?;
    let results = json
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "search response has no `results` array".to_string())?;

    let out = results
        .iter()
        .filter_map(|r| {
            let url = r.get("url").and_then(Value::as_str)?.to_string();
            let title = r
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let snippet = cap_snippet(r.get("content").and_then(Value::as_str).unwrap_or(""));
            Some(SearchResult {
                title,
                url,
                snippet,
            })
        })
        .take(limit)
        .collect();
    Ok(out)
}

/// Trim and char-cap a snippet (never splits a multibyte char).
fn cap_snippet(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= SNIPPET_CHARS {
        return s.to_string();
    }
    let capped: String = s.chars().take(SNIPPET_CHARS).collect();
    format!("{capped}…")
}

/// Format results as a numbered list: `N. {title}\n   {url}\n   {snippet}`.
fn format_results(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("No results for `{query}`.");
    }
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let title = if r.title.is_empty() { &r.url } else { &r.title };
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, title, r.url));
        if !r.snippet.is_empty() {
            out.push_str(&format!("   {}\n", r.snippet));
        }
    }
    out.trim_end().to_string()
}

/// Agent-callable web search. Reads the live [`SearchConfig`] (shared with the
/// settings command) on each call so a runtime backend switch takes effect.
/// Supplies API keys for key-gated search backends (#1010). Implemented by the host
/// over its OS keychain (`SecretStore`); `ff-tools` cannot depend on the host's
/// `secrets` module (wrong dependency direction), so the key is *injected* rather
/// than fetched here. `backend` is the [`SearchBackend`] the key is wanted for;
/// `None` means no key is stored (the tool then errors actionably for a required
/// backend, or falls back to keyless mode for an optional one like Tavily).
pub trait SearchKeyProvider: Send + Sync {
    fn key_for(&self, backend: SearchBackend) -> Option<String>;
}

/// A [`SearchKeyProvider`] that never has a key — the default when no host keychain
/// is wired (tests, or a keyless-only deployment). Keeps keyless backends working.
pub struct NoSearchKeys;

impl SearchKeyProvider for NoSearchKeys {
    fn key_for(&self, _backend: SearchBackend) -> Option<String> {
        None
    }
}

/// A search **corpus** the agent can query (#552 / #1011). Each source becomes its
/// own agent tool (`web_search`, later `pubmed_search`, …) so the model knows which
/// index it is hitting. Adding a source = implement this trait + register a
/// [`SearchTool`] over it — no agent-loop or registry-logic change.
///
/// A source may make multiple internal HTTP requests and return a flat result list
/// (`search`'s signature), which fits multi-step APIs like PubMed's esearch→esummary.
#[async_trait]
pub trait SearchSource: Send + Sync {
    /// Stable id (e.g. `"web"`, `"pubmed"`), for config/persona references.
    fn id(&self) -> &str;
    /// The agent tool name this source is exposed as (e.g. `"web_search"`).
    fn tool_name(&self) -> &str;
    /// The tool description shown to the model.
    fn description(&self) -> &str;
    /// Run the search, returning ranked results (or an actionable error).
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String>;
}

/// The default web-search source (#552): one corpus, backed by an interchangeable
/// web backend (Tavily / SearXNG / Brave / OpenAI-compatible) chosen from the live
/// [`SearchConfig`]. The backend enum is an *implementation detail* of this source,
/// not a separate source — they all query "the web".
pub struct WebSource {
    config: Arc<Mutex<SearchConfig>>,
    keys: Arc<dyn SearchKeyProvider>,
}

impl WebSource {
    pub fn new(config: Arc<Mutex<SearchConfig>>) -> Self {
        Self {
            config,
            keys: Arc::new(NoSearchKeys),
        }
    }

    pub fn with_keys(config: Arc<Mutex<SearchConfig>>, keys: Arc<dyn SearchKeyProvider>) -> Self {
        Self { config, keys }
    }

    /// Resolve the configured backend into a provider, or an error explaining why
    /// search is unavailable (unconfigured endpoint, or a missing API key).
    fn provider(&self) -> Result<Box<dyn SearchProvider>, String> {
        let config = self.config.lock().unwrap().clone();
        // Treat an empty/whitespace stored key as absent, so the local gate fires an
        // actionable "add a key in Settings" instead of sending a blank credential and
        // surfacing the backend's remote 401/403 after a wasted round-trip (#1013).
        let key = self
            .keys
            .key_for(config.backend)
            .filter(|k| !k.trim().is_empty());
        // A key-gated backend with no stored key errors *actionably* (#1010).
        if config.backend.requires_key() && key.is_none() {
            return Err(format!(
                "the {:?} search backend needs an API key — add one in Settings → Search \
                 (or switch to Tavily / SearXNG, which need no key)",
                config.backend
            ));
        }
        match config.backend {
            // Tavily's key is optional (raises the rate limit); pass it when present.
            SearchBackend::Tavily => Ok(Box::new(TavilyProvider::new(key))),
            SearchBackend::SearxNg => {
                let base = config.resolved_base_url().ok_or_else(|| {
                    "no SearXNG endpoint configured — set one in Settings → Search".to_string()
                })?;
                Ok(Box::new(SearxngProvider::new(base.to_string())))
            }
            SearchBackend::Brave => {
                // `key` is Some here (the requires_key gate above guarantees it).
                Ok(Box::new(BraveProvider::new(key.unwrap_or_default())))
            }
            SearchBackend::OpenAiCompatible => {
                let base = config.resolved_base_url().ok_or_else(|| {
                    "no OpenAI-compatible search endpoint configured — set one in \
                     Settings → Search"
                        .to_string()
                })?;
                Ok(Box::new(OpenAiCompatibleProvider::new(
                    base.to_string(),
                    key.unwrap_or_default(),
                )))
            }
        }
    }
}

#[async_trait]
impl SearchSource for WebSource {
    fn id(&self) -> &str {
        "web"
    }
    fn tool_name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> &str {
        "Search the web and return ranked results (title, URL, snippet). Uses the \
         configured search backend (Tavily by default). Requires approval (network \
         access)."
    }
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        self.provider()?.search(query, limit).await
    }
}

/// Keyless PubMed / biomedical-literature search via NCBI E-utilities (#1012).
/// A `search` is two serialized requests to `eutils.ncbi.nlm.nih.gov` (no key):
/// `esearch` → PMIDs, then `esummary` → title / authors / journal / year. Fixed,
/// vetted host, still SSRF-checked for defense in depth. Reversibility/format match
/// the web sources so it plugs into [`SearchTool`] like any other corpus.
///
/// NCBI compliance: every request carries `tool=flowforge` and the keyless rate cap
/// is 3 req/s — the two calls are issued serially, well within it.
pub struct PubMedSource {
    policy: SsrfPolicy,
}

impl Default for PubMedSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PubMedSource {
    pub fn new() -> Self {
        Self {
            policy: SsrfPolicy::strict(),
        }
    }

    async fn get_json(&self, url: &str) -> Result<Value, String> {
        let checked = self.policy.check_url(url)?;
        self.policy.check_host(&checked).await?;
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        let resp = client
            .get(checked)
            .header(USER_AGENT, UA)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("PubMed request failed: {e}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err("PubMed (NCBI) rate limit reached. Wait a moment and retry.".to_string());
        }
        if !status.is_success() {
            return Err(format!("PubMed endpoint returned HTTP {status}"));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("failed to read PubMed response: {e}"))?;
        serde_json::from_str(&body).map_err(|e| format!("invalid PubMed JSON: {e}"))
    }
}

#[async_trait]
impl SearchSource for PubMedSource {
    fn id(&self) -> &str {
        "pubmed"
    }
    fn tool_name(&self) -> &str {
        "pubmed_search"
    }
    fn description(&self) -> &str {
        "Search PubMed (NCBI) biomedical literature and return ranked articles \
         (title, PubMed URL, authors · journal · year). No API key required. \
         Requires approval (network access)."
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        // Step 1 — esearch: query → PMIDs (JSON).
        let esearch = url::Url::parse_with_params(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi",
            &[
                ("db", "pubmed"),
                ("term", query),
                ("retmax", &limit.to_string()),
                ("retmode", "json"),
                ("tool", "flowforge"),
            ],
        )
        .map_err(|e| format!("failed to build esearch URL: {e}"))?;
        let search_json = self.get_json(esearch.as_str()).await?;
        let ids: Vec<String> = search_json
            .get("esearchresult")
            .and_then(|r| r.get("idlist"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2 — esummary: PMIDs → title / authors / journal / year (JSON).
        let esummary = url::Url::parse_with_params(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi",
            &[
                ("db", "pubmed"),
                ("id", &ids.join(",")),
                ("retmode", "json"),
                ("tool", "flowforge"),
            ],
        )
        .map_err(|e| format!("failed to build esummary URL: {e}"))?;
        let summary_json = self.get_json(esummary.as_str()).await?;
        parse_pubmed_summary(&summary_json, &ids)
    }
}

/// Map an NCBI esummary JSON response + the PMID order from esearch into ranked
/// [`SearchResult`]s. Free function so it is unit-testable without network — the
/// esearch→esummary orchestration is thin; the value is this shape mapping.
fn parse_pubmed_summary(summary_json: &Value, ids: &[String]) -> Result<Vec<SearchResult>, String> {
    let result_obj = summary_json
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| "PubMed esummary has no `result` object".to_string())?;

    // Preserve esearch's PMID order.
    let out = ids
        .iter()
        .filter_map(|pmid| {
            let doc = result_obj.get(pmid)?;
            let title = doc
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let journal = doc
                .get("fulljournalname")
                .and_then(Value::as_str)
                .or_else(|| doc.get("source").and_then(Value::as_str));
            let year = doc
                .get("pubdate")
                .and_then(Value::as_str)
                .and_then(|d| d.split_whitespace().next());
            let authors: Vec<&str> = doc
                .get("authors")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|au| au.get("name").and_then(Value::as_str))
                        .take(3)
                        .collect()
                })
                .unwrap_or_default();
            let mut meta = Vec::new();
            if !authors.is_empty() {
                meta.push(authors.join(", "));
            }
            if let Some(j) = journal {
                meta.push(j.to_string());
            }
            if let Some(y) = year {
                meta.push(y.to_string());
            }
            Some(SearchResult {
                title,
                url: format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/"),
                snippet: cap_snippet(&meta.join(" · ")),
            })
        })
        .collect();
    Ok(out)
}

/// Generic agent tool over any [`SearchSource`] (#1011). One concrete tool type
/// serves every corpus — the registry adds a new source with a single
/// `SearchTool::new(Arc::new(MySource))` line, no bespoke tool per source.
pub struct SearchTool {
    source: Arc<dyn SearchSource>,
}

impl SearchTool {
    pub fn new(source: Arc<dyn SearchSource>) -> Self {
        Self { source }
    }
}

/// Backward-compatible constructor namespace for the web-search tool. Existing host
/// and CLI call sites (`WebSearchTool::new` / `::with_keys`) keep working; they now
/// build a [`SearchTool`] over a [`WebSource`].
pub struct WebSearchTool;

// These intentionally return `SearchTool`, not `Self` — `WebSearchTool` is a
// constructor namespace (kept so #1010's call sites don't churn), not a held type.
#[allow(clippy::new_ret_no_self)]
impl WebSearchTool {
    /// Web search with no key provider — keyless backends only (tests, keyless deploys).
    pub fn new(config: Arc<Mutex<SearchConfig>>) -> SearchTool {
        SearchTool::new(Arc::new(WebSource::new(config)))
    }

    /// Web search with a host-supplied key provider (#1010) so key-gated backends
    /// (Brave, OpenAI-compatible) resolve their key from the OS keychain.
    pub fn with_keys(
        config: Arc<Mutex<SearchConfig>>,
        keys: Arc<dyn SearchKeyProvider>,
    ) -> SearchTool {
        SearchTool::new(Arc::new(WebSource::with_keys(config, keys)))
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        self.source.tool_name()
    }

    fn description(&self) -> &str {
        self.source.description()
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query." },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default 5, max 10)."
                }
            },
            "required": ["query"]
        })
    }

    /// Network egress is externally-visible, so it is [`Safety::Sensitive`]
    /// (#698) rather than the plain `Write` default. Treated identically to
    /// `Write` for now — still approval-gated the same way; `max_safety` matches
    /// so it stays hidden in Plan-mode advertisement.
    fn safety(&self, _args: &Value) -> Safety {
        Safety::Sensitive
    }

    fn max_safety(&self) -> Safety {
        Safety::Sensitive
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        let name = self.source.tool_name();
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolOutcome::error(format!("{name} requires a string `query`"));
        };
        let query = query.trim();
        if query.is_empty() {
            return ToolOutcome::error(format!("{name} requires a non-empty `query`"));
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        match self.source.search(query, limit).await {
            Ok(results) => ToolOutcome::ok(format_results(query, &results)),
            Err(e) => ToolOutcome::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Safety;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn shared(config: SearchConfig) -> Arc<Mutex<SearchConfig>> {
        Arc::new(Mutex::new(config))
    }

    /// A canned provider so the tool can be exercised without a network.
    struct MockSearchProvider {
        results: Vec<SearchResult>,
    }

    #[async_trait]
    impl SearchProvider for MockSearchProvider {
        async fn search(&self, _query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
            Ok(self.results.iter().take(limit).cloned().collect())
        }
    }

    #[tokio::test]
    async fn mock_provider_results_are_formatted() {
        let provider = MockSearchProvider {
            results: vec![
                SearchResult {
                    title: "Rust".into(),
                    url: "https://rust-lang.org".into(),
                    snippet: "A language empowering everyone.".into(),
                },
                SearchResult {
                    title: "Tokio".into(),
                    url: "https://tokio.rs".into(),
                    snippet: "Async runtime.".into(),
                },
            ],
        };
        let results = provider.search("rust", 10).await.unwrap();
        let out = format_results("rust", &results);
        assert!(out.contains("1. Rust"), "{out}");
        assert!(out.contains("https://rust-lang.org"), "{out}");
        assert!(out.contains("2. Tokio"), "{out}");
    }

    #[test]
    fn safety_is_sensitive_so_it_is_approval_gated() {
        let tool = WebSearchTool::new(shared(SearchConfig::default()));
        assert_eq!(tool.safety(&serde_json::json!({})), Safety::Sensitive);
        assert_eq!(tool.max_safety(), Safety::Sensitive);
    }

    #[test]
    fn parameters_require_query() {
        let tool = WebSearchTool::new(shared(SearchConfig::default()));
        let params = tool.parameters();
        let required = params["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "query"));
    }

    #[tokio::test]
    async fn searxng_without_endpoint_errors() {
        let tool = WebSearchTool::new(shared(SearchConfig {
            backend: SearchBackend::SearxNg,
            base_url: None,
            has_key: false,
        }));
        let out = tool
            .run(serde_json::json!({ "query": "rust" }), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(
            out.content.contains("no SearXNG endpoint"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn keyed_backend_is_gated() {
        let tool = WebSearchTool::new(shared(SearchConfig {
            backend: SearchBackend::Brave,
            base_url: None,
            has_key: false,
        }));
        let out = tool
            .run(serde_json::json!({ "query": "rust" }), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(out.content.contains("API key"), "{}", out.content);
    }

    #[tokio::test]
    async fn empty_query_errors() {
        let tool = WebSearchTool::new(shared(SearchConfig::default()));
        let out = tool
            .run(serde_json::json!({ "query": "   " }), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(out.content.contains("query"), "{}", out.content);
    }

    #[test]
    fn parse_results_extracts_and_caps() {
        let body = serde_json::json!({
            "results": [
                { "title": "A", "url": "https://a.test", "content": "first" },
                { "title": "B", "url": "https://b.test", "content": "second" },
                { "title": "C", "url": "https://c.test", "content": "third" },
            ]
        })
        .to_string();
        let results = parse_results(&body, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "A");
        assert_eq!(results[0].url, "https://a.test");
    }

    #[test]
    fn parse_results_rejects_missing_results() {
        let err = parse_results("{}", 5).unwrap_err();
        assert!(err.contains("results"), "{err}");
    }

    #[tokio::test]
    async fn searxng_provider_hits_endpoint_and_parses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "title": "Hit", "url": "https://example.test", "content": "snippet" }
                ]
            })))
            .mount(&server)
            .await;

        // Loopback-relaxed policy so the wiremock server is reachable; private /
        // metadata ranges stay blocked.
        let provider = SearxngProvider {
            base_url: server.uri(),
            policy: SsrfPolicy {
                allow_loopback: true,
            },
        };
        let results = provider.search("rust", 5).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Hit");
        assert_eq!(results[0].url, "https://example.test");
        assert_eq!(results[0].snippet, "snippet");
    }

    #[tokio::test]
    async fn searxng_provider_refuses_internal_endpoint() {
        let provider = SearxngProvider::new("http://169.254.169.254".into());
        let err = provider.search("rust", 5).await.unwrap_err();
        assert!(err.contains("SSRF guard"), "{err}");
    }

    #[tokio::test]
    async fn tavily_provider_posts_and_parses() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "title": "Hit", "url": "https://example.test", "content": "snippet" }
                ]
            })))
            .mount(&server)
            .await;

        // Loopback-relaxed policy so the wiremock server is reachable; private /
        // metadata ranges stay blocked.
        let provider = TavilyProvider {
            endpoint: format!("{}/search", server.uri()),
            policy: SsrfPolicy {
                allow_loopback: true,
            },
            key: None,
        };
        let results = provider.search("rust", 5).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Hit");
        assert_eq!(results[0].url, "https://example.test");
        assert_eq!(results[0].snippet, "snippet");
    }

    #[tokio::test]
    async fn tavily_provider_surfaces_rate_limit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "error": { "code": "hourly_cap_reached" }
            })))
            .mount(&server)
            .await;

        let provider = TavilyProvider {
            endpoint: format!("{}/search", server.uri()),
            policy: SsrfPolicy {
                allow_loopback: true,
            },
            key: None,
        };
        let err = provider.search("rust", 5).await.unwrap_err();
        assert!(err.contains("rate limit"), "{err}");
        assert!(err.contains("Settings"), "{err}");
    }

    #[tokio::test]
    async fn tavily_provider_refuses_internal_endpoint() {
        let provider = TavilyProvider {
            endpoint: "http://169.254.169.254/search".into(),
            policy: SsrfPolicy::strict(),
            key: None,
        };
        let err = provider.search("rust", 5).await.unwrap_err();
        assert!(err.contains("SSRF guard"), "{err}");
    }

    #[test]
    fn default_backend_resolves_to_a_provider() {
        // Default (Tavily keyless) must yield a working provider with no config.
        let source = WebSource::new(shared(SearchConfig::default()));
        assert!(source.provider().is_ok());
    }

    /// A stub key provider that returns a fixed key for every backend (#1010).
    struct StubKeys(Option<String>);
    impl SearchKeyProvider for StubKeys {
        fn key_for(&self, _backend: SearchBackend) -> Option<String> {
            self.0.clone()
        }
    }

    #[test]
    fn keyed_backend_without_key_errors_actionably() {
        // Brave needs a key; with none, the source must fail with a fix-it message,
        // not silently or with the old "#8 not supported" stub.
        let config = SearchConfig {
            backend: SearchBackend::Brave,
            ..SearchConfig::default()
        };
        let source = WebSource::with_keys(shared(config), Arc::new(NoSearchKeys));
        let err = source.provider().err().expect("must error without a key");
        assert!(err.contains("API key"), "{err}");
        assert!(
            err.contains("Settings"),
            "error should point to the fix: {err}"
        );
        assert!(!err.contains("#8"), "must not be the old stub error: {err}");
    }

    #[test]
    fn keyed_backend_with_key_resolves_to_a_provider() {
        let config = SearchConfig {
            backend: SearchBackend::Brave,
            ..SearchConfig::default()
        };
        let source =
            WebSource::with_keys(shared(config), Arc::new(StubKeys(Some("brave-key".into()))));
        assert!(source.provider().is_ok(), "Brave with a key must resolve");
    }

    #[test]
    fn keyed_backend_with_empty_key_errors_actionably() {
        // A stored but empty/whitespace key must be treated as absent (#1013): the tool
        // fails with the same local fix-it message instead of sending a blank credential
        // and surfacing the backend's remote 401/403 after a wasted round-trip.
        let config = SearchConfig {
            backend: SearchBackend::Brave,
            ..SearchConfig::default()
        };
        let source = WebSource::with_keys(shared(config), Arc::new(StubKeys(Some("   ".into()))));
        let err = source
            .provider()
            .err()
            .expect("empty key must be treated as no key");
        assert!(err.contains("API key"), "{err}");
        assert!(
            err.contains("Settings"),
            "error should point to the fix: {err}"
        );
    }

    #[test]
    fn brave_provider_parses_web_results() {
        // Brave's response shape differs from Tavily/SearXNG: web.results[] with
        // title/url/description. The provider hits a fixed api.search.brave.com URL,
        // so exercise the shape-specific parser directly.
        let body = serde_json::json!({
            "web": { "results": [
                { "title": "Rust", "url": "https://rust-lang.org", "description": "A language." },
                { "title": "Tokio", "url": "https://tokio.rs", "description": "Async runtime." }
            ]}
        })
        .to_string();
        let out = parse_brave_results(&body, 5).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://rust-lang.org");
        assert_eq!(out[0].title, "Rust");
        assert!(out[0].snippet.contains("A language"));
    }

    #[test]
    fn brave_parse_rejects_missing_web_results() {
        let err = parse_brave_results("{\"foo\":1}", 5).unwrap_err();
        assert!(err.contains("web.results"), "{err}");
    }

    /// A minimal stub source (#1011): proves `SearchTool` advertises the source's
    /// own tool name/description and runs it end-to-end — the seam #1012 (PubMed)
    /// plugs into with one `SearchTool::new(Arc::new(...))` registration.
    struct StubSource;
    #[async_trait]
    impl SearchSource for StubSource {
        fn id(&self) -> &str {
            "stub"
        }
        fn tool_name(&self) -> &str {
            "stub_search"
        }
        fn description(&self) -> &str {
            "stub corpus"
        }
        async fn search(&self, query: &str, _limit: usize) -> Result<Vec<SearchResult>, String> {
            Ok(vec![SearchResult {
                title: format!("hit for {query}"),
                url: "https://example.test/1".into(),
                snippet: "snippet".into(),
            }])
        }
    }

    #[tokio::test]
    async fn search_tool_exposes_source_identity_and_runs() {
        let tool = SearchTool::new(Arc::new(StubSource));
        assert_eq!(tool.name(), "stub_search");
        assert_eq!(tool.description(), "stub corpus");
        let out = tool
            .run(
                serde_json::json!({ "query": "rust" }),
                std::path::Path::new("."),
            )
            .await;
        let text = out.content.clone();
        assert!(text.contains("hit for rust"), "{text}");
        assert!(text.contains("example.test"), "{text}");
    }

    #[tokio::test]
    async fn search_tool_rejects_empty_query() {
        let tool = SearchTool::new(Arc::new(StubSource));
        let out = tool
            .run(
                serde_json::json!({ "query": "  " }),
                std::path::Path::new("."),
            )
            .await;
        assert!(!out.success, "empty query must error");
    }

    // ---- #1012: PubMed source ----

    #[test]
    fn pubmed_source_identity() {
        let src = PubMedSource::new();
        assert_eq!(src.tool_name(), "pubmed_search");
        assert_eq!(src.id(), "pubmed");
    }

    #[test]
    fn pubmed_summary_maps_to_results_in_pmid_order() {
        // esummary JSON shape → SearchResult, preserving esearch's PMID order.
        let json = serde_json::json!({
            "result": {
                "uids": ["222", "111"],
                "111": {
                    "title": "CRISPR gene editing",
                    "fulljournalname": "Nature",
                    "pubdate": "2023 Jun 1",
                    "authors": [ {"name": "Doe J"}, {"name": "Roe K"} ]
                },
                "222": {
                    "title": "Base editing advances",
                    "source": "Cell",
                    "pubdate": "2024 Jan"
                }
            }
        });
        let ids = vec!["222".to_string(), "111".to_string()];
        let out = parse_pubmed_summary(&json, &ids).unwrap();
        assert_eq!(out.len(), 2);
        // Order follows `ids` (222 first).
        assert_eq!(out[0].title, "Base editing advances");
        assert_eq!(out[0].url, "https://pubmed.ncbi.nlm.nih.gov/222/");
        assert!(out[0].snippet.contains("Cell"), "{}", out[0].snippet);
        assert!(out[0].snippet.contains("2024"), "{}", out[0].snippet);
        assert_eq!(out[1].url, "https://pubmed.ncbi.nlm.nih.gov/111/");
        assert!(out[1].snippet.contains("Doe J"), "{}", out[1].snippet);
        assert!(out[1].snippet.contains("Nature"), "{}", out[1].snippet);
    }

    #[test]
    fn pubmed_summary_missing_result_errors() {
        let err = parse_pubmed_summary(&serde_json::json!({"foo": 1}), &["1".into()]).unwrap_err();
        assert!(err.contains("result"), "{err}");
    }

    #[tokio::test]
    async fn pubmed_search_tool_advertises_pubmed_name() {
        let tool = SearchTool::new(Arc::new(PubMedSource::new()));
        assert_eq!(tool.name(), "pubmed_search");
        assert!(tool.description().contains("PubMed"));
    }
}
