//! `web_fetch` — HTTP GET a URL and return its readable content as markdown.
//!
//! Network egress, so it is `Safety::Write` (approval-gated by the agent loop). An
//! [`SsrfPolicy`](crate::url_safety::SsrfPolicy) rejects internal / loopback /
//! link-local / cloud-metadata targets before connecting and on every redirect hop.
//! No JavaScript is executed (plain GET), matching the deterministic-tool ethos.

use std::path::Path;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use serde_json::Value;
use url::Url;

use crate::html_text::{self, MAX_BYTES, TRUNCATE_BYTES};
use crate::registry::{Tool, ToolOutcome};
use crate::url_safety::SsrfPolicy;

/// Cap on redirect hops we follow manually (each re-checked by the SSRF policy).
const MAX_REDIRECTS: usize = 5;
/// Per-request timeout.
const TIMEOUT_SECS: u64 = 15;
const UA: &str = "FlowForge/0.1 (+web_fetch)";

/// How much of a fetched document to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FetchMode {
    /// First [`TRUNCATE_BYTES`] of the markdown — a cheap preview.
    Truncated,
    /// Readability main-content extraction. NOT yet implemented and NOT advertised
    /// in the tool schema (Option A) — reserved for the `dom_smoothie` follow-up
    /// (TODO #70). If a caller forces it, `run` returns a clear error.
    #[allow(dead_code)] // reserved; constructed once #70 lands
    Distilled,
    /// Full document as markdown, capped only by [`MAX_BYTES`]. Default mode.
    #[default]
    Full,
}

impl FetchMode {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "truncated" => Ok(Self::Truncated),
            "full" => Ok(Self::Full),
            // Reserved but unadvertised (TODO #70).
            "distilled" => Err(
                "mode `distilled` is not available yet (tracked in #70); use `full` or `truncated`"
                    .to_string(),
            ),
            other => Err(format!(
                "invalid mode `{other}` (expected `truncated` or `full`)"
            )),
        }
    }
}

/// Fetches and extracts a web page. Constructed with a strict SSRF policy in prod.
pub struct WebFetchTool {
    policy: SsrfPolicy,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            policy: SsrfPolicy::strict(),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL over HTTP(S) and return its readable content as Markdown. Does \
         not run JavaScript. Internal, loopback, and cloud-metadata addresses are \
         refused. `mode`: `full` (default, whole page) or `truncated` (first ~8 KB \
         preview). Requires approval (network access)."
    }

    fn parameters(&self) -> Value {
        // `distilled` is intentionally absent from this enum until #70 implements it.
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The http(s) URL to fetch." },
                "mode": {
                    "type": "string",
                    "enum": ["full", "truncated"],
                    "description": "How much to return: `full` (default) or `truncated` (~8 KB preview)."
                }
            },
            "required": ["url"]
        })
    }

    // Defaults to `Safety::Write` (network egress) -> approval-gated. No override.

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        let Some(url) = args.get("url").and_then(Value::as_str) else {
            return ToolOutcome::error("web_fetch requires a string `url`");
        };
        let mode = match args.get("mode").and_then(Value::as_str) {
            Some(s) => match FetchMode::parse(s) {
                Ok(m) => m,
                Err(e) => return ToolOutcome::error(e),
            },
            None => FetchMode::default(),
        };

        match self.fetch(url, mode).await {
            Ok(body) => ToolOutcome::ok(body),
            Err(e) => ToolOutcome::error(e),
        }
    }
}

impl WebFetchTool {
    async fn fetch(&self, url: &str, mode: FetchMode) -> Result<String, String> {
        let client = reqwest::Client::builder()
            .redirect(Policy::none()) // we follow manually, re-checking each hop
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let mut current = self.policy.check_url(url)?;

        for _ in 0..=MAX_REDIRECTS {
            // Re-validate the host on every hop (SSRF-via-redirect): literal IPs
            // directly, named hosts via DNS + per-answer check.
            self.policy.check_host(&current).await?;

            let resp = client
                .get(current.clone())
                .header(USER_AGENT, UA)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;

            let status = resp.status();
            if status.is_redirection() {
                let location = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| format!("redirect ({status}) without a Location header"))?;
                let next = resolve_redirect(&current, location)?;
                current = self.policy.check_url(next.as_str())?;
                continue;
            }

            if !status.is_success() {
                return Err(format!("HTTP {status}"));
            }

            let content_type = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();

            let body = resp
                .text()
                .await
                .map_err(|e| format!("failed to read response body: {e}"))?;

            return Ok(render(&content_type, &body, mode));
        }

        Err(format!("too many redirects (>{MAX_REDIRECTS})"))
    }
}

/// Resolve a (possibly relative) `Location` against the current URL.
fn resolve_redirect(base: &Url, location: &str) -> Result<Url, String> {
    base.join(location)
        .map_err(|e| format!("invalid redirect target `{location}`: {e}"))
}

/// Turn a response body into the tool's output string, honoring the content type
/// and the requested mode.
fn render(content_type: &str, body: &str, mode: FetchMode) -> String {
    let is_html = content_type.contains("text/html") || content_type.contains("application/xhtml");
    let text = if is_html {
        html_text::html_to_markdown(body)
    } else if content_type.is_empty() || content_type.starts_with("text/") {
        // Unknown or plain text: return as-is (still capped below).
        body.trim().to_string()
    } else {
        return format!(
            "(unsupported content type `{content_type}` — web_fetch returns text only)"
        );
    };

    let limit = match mode {
        FetchMode::Truncated => TRUNCATE_BYTES,
        // `Distilled` is rejected before reaching here; treat as Full defensively.
        FetchMode::Full | FetchMode::Distilled => MAX_BYTES,
    };
    let (capped, truncated) = html_text::cap(&text, limit);
    if truncated {
        format!("{capped}\n\n(truncated at {limit} bytes)")
    } else {
        capped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Safety;
    use crate::url_safety::SsrfPolicy;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A tool whose SSRF policy permits loopback, so it can reach a local mock
    /// server. Link-local / private / metadata ranges stay blocked.
    fn loopback_tool() -> WebFetchTool {
        WebFetchTool {
            policy: SsrfPolicy {
                allow_loopback: true,
            },
        }
    }

    #[test]
    fn safety_is_write_so_it_is_approval_gated() {
        assert_eq!(
            WebFetchTool::new().safety(&serde_json::json!({})),
            Safety::Write
        );
    }

    #[test]
    fn distilled_mode_is_rejected_until_70() {
        assert!(FetchMode::parse("distilled").is_err());
        assert_eq!(FetchMode::parse("full").unwrap(), FetchMode::Full);
        assert_eq!(FetchMode::parse("truncated").unwrap(), FetchMode::Truncated);
    }

    #[test]
    fn schema_advertises_only_full_and_truncated() {
        let params = WebFetchTool::new().parameters();
        let modes = params["properties"]["mode"]["enum"].as_array().unwrap();
        let modes: Vec<_> = modes.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(modes, vec!["full", "truncated"]);
        assert!(!modes.contains(&"distilled"));
    }

    #[tokio::test]
    async fn fetches_html_and_returns_markdown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<h1>Hello</h1><p>From <b>FlowForge</b>.</p>"),
            )
            .mount(&server)
            .await;

        let out = loopback_tool()
            .run(serde_json::json!({ "url": server.uri() }), Path::new("."))
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("Hello"), "{}", out.content);
        assert!(out.content.contains("FlowForge"), "{}", out.content);
    }

    #[tokio::test]
    async fn truncated_mode_caps_output() {
        let server = MockServer::start().await;
        let big = format!("<p>{}</p>", "x".repeat(20_000));
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string(big),
            )
            .mount(&server)
            .await;

        let out = loopback_tool()
            .run(
                serde_json::json!({ "url": server.uri(), "mode": "truncated" }),
                Path::new("."),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert!(
            out.content.contains("(truncated at 8000 bytes)"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn rejects_redirect_to_internal_metadata() {
        // First hop is loopback (allowed by the test policy); it redirects to the
        // cloud-metadata IP, which the SSRF guard must refuse on the next hop.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
            )
            .mount(&server)
            .await;

        let out = loopback_tool()
            .run(serde_json::json!({ "url": server.uri() }), Path::new("."))
            .await;
        assert!(
            !out.success,
            "redirect to metadata must fail: {}",
            out.content
        );
        assert!(out.content.contains("SSRF guard"), "{}", out.content);
    }

    #[tokio::test]
    async fn rejects_internal_target_up_front() {
        let out = WebFetchTool::new()
            .run(
                serde_json::json!({ "url": "http://169.254.169.254/" }),
                Path::new("."),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("SSRF guard"), "{}", out.content);
    }

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let out = WebFetchTool::new()
            .run(
                serde_json::json!({ "url": "file:///etc/passwd" }),
                Path::new("."),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("scheme"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_url_is_error() {
        let out = WebFetchTool::new()
            .run(serde_json::json!({}), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(out.content.contains("url"));
    }
}
