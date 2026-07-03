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
}

impl TavilyProvider {
    fn new() -> Self {
        Self {
            endpoint: TAVILY_ENDPOINT.to_string(),
            policy: SsrfPolicy::strict(),
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

        let resp = client
            .post(checked)
            .header(USER_AGENT, UA)
            // Keyless access mode is mandatory when no API key is sent; without it
            // Tavily returns 401. An optional key (later phase) replaces this header
            // with `Authorization: Bearer <key>` to raise the rate limit.
            .header("X-Tavily-Access-Mode", "keyless")
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
pub struct WebSearchTool {
    config: Arc<Mutex<SearchConfig>>,
}

impl WebSearchTool {
    pub fn new(config: Arc<Mutex<SearchConfig>>) -> Self {
        Self { config }
    }

    /// Resolve the configured backend into a provider, or an error explaining why
    /// search is unavailable (unconfigured endpoint, or a key-gated backend).
    fn provider(&self) -> Result<Box<dyn SearchProvider>, String> {
        let config = self.config.lock().unwrap().clone();
        if config.backend.requires_key() && !config.has_key {
            return Err(format!(
                "the {:?} search backend needs an API key, which isn't supported yet \
                 (tracked with the keychain work, #8); switch to Tavily or SearXNG in Settings",
                config.backend
            ));
        }
        match config.backend {
            SearchBackend::Tavily => Ok(Box::new(TavilyProvider::new())),
            SearchBackend::SearxNg => {
                let base = config.resolved_base_url().ok_or_else(|| {
                    "no SearXNG endpoint configured — set one in Settings → Search".to_string()
                })?;
                Ok(Box::new(SearxngProvider::new(base.to_string())))
            }
            // Recognized but gated above (requires_key + has_key=false always trips).
            SearchBackend::Brave | SearchBackend::OpenAiCompatible => Err(format!(
                "the {:?} search backend is not available yet (#8)",
                config.backend
            )),
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web and return ranked results (title, URL, snippet). Uses the \
         configured search backend (Tavily by default). Requires approval (network \
         access)."
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
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolOutcome::error("web_search requires a string `query`");
        };
        let query = query.trim();
        if query.is_empty() {
            return ToolOutcome::error("web_search requires a non-empty `query`");
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        let provider = match self.provider() {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };
        match provider.search(query, limit).await {
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
        };
        let err = provider.search("rust", 5).await.unwrap_err();
        assert!(err.contains("SSRF guard"), "{err}");
    }

    #[test]
    fn default_backend_resolves_to_a_provider() {
        // Default (Tavily keyless) must yield a working provider with no config.
        let tool = WebSearchTool::new(shared(SearchConfig::default()));
        assert!(tool.provider().is_ok());
    }
}
