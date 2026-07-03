//! `web_fetch` — HTTP GET a URL and return its readable content as markdown.
//!
//! Network egress, so it is `Safety::Sensitive` (approval-gated by the agent loop). An
//! [`SsrfPolicy`](crate::url_safety::SsrfPolicy) rejects internal / loopback /
//! link-local / cloud-metadata targets before connecting and on every redirect hop.
//! No JavaScript is executed (plain GET), matching the deterministic-tool ethos.

use std::path::Path;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::Response;
use serde_json::Value;
use url::Url;

use crate::html_text::{self, MAX_BYTES, TRUNCATE_BYTES};
use crate::registry::{Safety, Tool, ToolOutcome};
use crate::url_safety::SsrfPolicy;

/// Cap on redirect hops we follow manually (each re-checked by the SSRF policy).
const MAX_REDIRECTS: usize = 5;
/// Per-request timeout.
const TIMEOUT_SECS: u64 = 15;
/// Hard ceiling on raw bytes read from the network (defense in depth). Output is
/// capped separately by [`MAX_BYTES`] / [`TRUNCATE_BYTES`].
const MAX_DOWNLOAD_BYTES: u64 = 524_288; // 512 KiB
const UA: &str = "FlowForge/0.1 (+web_fetch)";

/// How much of a fetched document to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FetchMode {
    /// First [`TRUNCATE_BYTES`] of the markdown — a cheap preview.
    Truncated,
    /// Readability main-content extraction: strips nav / header / footer / ads and
    /// returns just the article body as markdown, falling back to `full` when no main
    /// content is detected. The default — the biggest context-economy win for the model.
    #[default]
    Distilled,
    /// Full document as markdown, capped only by [`MAX_BYTES`].
    Full,
}

impl FetchMode {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "truncated" => Ok(Self::Truncated),
            "full" => Ok(Self::Full),
            "distilled" => Ok(Self::Distilled),
            other => Err(format!(
                "invalid mode `{other}` (expected `distilled`, `full`, or `truncated`)"
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
         refused. `mode`: `distilled` (default — main article content, boilerplate \
         stripped), `full` (whole page), or `truncated` (first ~8 KB preview). Requires \
         approval (network access)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The http(s) URL to fetch." },
                "mode": {
                    "type": "string",
                    "enum": ["distilled", "full", "truncated"],
                    "description": "How much to return: `distilled` (default — main article content, boilerplate stripped), `full` (whole page), or `truncated` (~8 KB preview)."
                }
            },
            "required": ["url"]
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

            let body = read_body_capped(resp).await?;

            return Ok(render(&content_type, &body, mode, Some(current.as_str())));
        }

        Err(format!("too many redirects (>{MAX_REDIRECTS})"))
    }
}

/// Read the response body with a hard byte ceiling. Rejects up front when
/// `Content-Length` exceeds the limit; otherwise accumulates streamed chunks until
/// the cap is reached.
async fn read_body_capped(resp: Response) -> Result<String, String> {
    if let Some(len) = resp.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(format!(
                "response too large ({len} bytes exceeds {MAX_DOWNLOAD_BYTES} byte download limit)"
            ));
        }
    }

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("failed to read response body: {e}"))?;
        let new_len = buf.len().saturating_add(chunk.len());
        if new_len as u64 > MAX_DOWNLOAD_BYTES {
            return Err(format!(
                "response exceeded {MAX_DOWNLOAD_BYTES} byte download limit"
            ));
        }
        buf.extend_from_slice(&chunk);
    }

    // Lossy decode: a page declaring iso-8859-1 / windows-1252 / UTF-16, or
    // carrying a stray invalid byte, degrades to U+FFFD instead of failing the
    // whole fetch — restoring the graceful behavior of reqwest's old `.text()`
    // (which an LLM-facing tool wants). The byte ceiling above still holds.
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Resolve a (possibly relative) `Location` against the current URL.
fn resolve_redirect(base: &Url, location: &str) -> Result<Url, String> {
    base.join(location)
        .map_err(|e| format!("invalid redirect target `{location}`: {e}"))
}

/// Turn a response body into the tool's output string, honoring the content type
/// and the requested mode.
fn render(content_type: &str, body: &str, mode: FetchMode, url: Option<&str>) -> String {
    let is_html = content_type.contains("text/html") || content_type.contains("application/xhtml");
    let text = if is_html {
        // `distilled` extracts the main article body, falling back to whole-page
        // conversion when no main content is found. Other modes convert the full page.
        match mode {
            FetchMode::Distilled => {
                html_text::distill(body, url).unwrap_or_else(|| html_text::html_to_markdown(body))
            }
            FetchMode::Full | FetchMode::Truncated => html_text::html_to_markdown(body),
        }
    } else if is_text_passthrough(content_type) {
        // Plain text, JSON, XML: return as-is (still capped below).
        body.trim().to_string()
    } else {
        return format!(
            "(unsupported content type `{content_type}` — web_fetch returns text only)"
        );
    };

    let limit = match mode {
        FetchMode::Truncated => TRUNCATE_BYTES,
        FetchMode::Distilled | FetchMode::Full => MAX_BYTES,
    };
    let (capped, truncated) = html_text::cap(&text, limit);
    if truncated {
        format!("{capped}\n\n(truncated at {limit} bytes)")
    } else {
        capped
    }
}

/// MIME types returned as plain text (not converted to markdown).
fn is_text_passthrough(content_type: &str) -> bool {
    if content_type.is_empty() {
        return true;
    }
    let base = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    base.starts_with("text/")
        || base == "application/json"
        || base.ends_with("+json")
        || base == "application/xml"
        || base.ends_with("+xml")
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn safety_is_sensitive_so_it_is_approval_gated() {
        let tool = WebFetchTool::new();
        assert_eq!(tool.safety(&serde_json::json!({})), Safety::Sensitive);
        assert_eq!(tool.max_safety(), Safety::Sensitive);
    }

    #[test]
    fn all_three_modes_parse_and_distilled_is_default() {
        assert_eq!(FetchMode::parse("distilled").unwrap(), FetchMode::Distilled);
        assert_eq!(FetchMode::parse("full").unwrap(), FetchMode::Full);
        assert_eq!(FetchMode::parse("truncated").unwrap(), FetchMode::Truncated);
        assert!(FetchMode::parse("bogus").is_err());
        assert_eq!(FetchMode::default(), FetchMode::Distilled);
    }

    #[test]
    fn schema_advertises_all_three_modes() {
        let params = WebFetchTool::new().parameters();
        let modes = params["properties"]["mode"]["enum"].as_array().unwrap();
        let modes: Vec<_> = modes.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(modes, vec!["distilled", "full", "truncated"]);
    }

    // A page with nav/header/footer boilerplate around a real article body.
    const BOILERPLATE_PAGE: &str = r#"<html><head><title>News</title></head><body>
        <nav><a href="/">Home</a><a href="/about">About</a><a href="/login">Sign in</a></nav>
        <header><h1>SiteName</h1><p>Subscribe to our newsletter for daily updates!</p></header>
        <article>
            <h1>Rustaceans Rejoice</h1>
            <p>This is the substantive article body that the model actually wants to read,
            with several sentences of real content so the extractor has enough signal to
            identify it as the main content region of the document.</p>
            <p>A second meaningful paragraph continues the article with more prose so the
            scoring heuristics clearly prefer this region over the surrounding chrome.</p>
        </article>
        <footer><a href="/privacy">Privacy</a><a href="/terms">Terms</a>
        <p>Copyright 2026 SiteName Inc. All rights reserved.</p></footer>
        </body></html>"#;

    #[test]
    fn distilled_strips_boilerplate_vs_full() {
        let full = render("text/html", BOILERPLATE_PAGE, FetchMode::Full, None);
        let distilled = render("text/html", BOILERPLATE_PAGE, FetchMode::Distilled, None);

        // The article body survives both.
        assert!(distilled.contains("Rustaceans Rejoice"), "{distilled}");
        assert!(
            distilled.contains("substantive article body"),
            "{distilled}"
        );

        // Header chrome that survives the full HTML->markdown pass is what Readability
        // additionally strips (the markdown converter already drops <nav>/<footer>).
        assert!(
            full.contains("Subscribe to our newsletter") && full.contains("SiteName"),
            "full should keep header chrome: {full}"
        );
        assert!(
            !distilled.contains("Subscribe to our newsletter") && !distilled.contains("SiteName"),
            "distilled must strip header chrome: {distilled}"
        );
        assert!(
            distilled.len() < full.len(),
            "distilled ({}) should be shorter than full ({})",
            distilled.len(),
            full.len()
        );
    }

    #[test]
    fn distilled_falls_back_to_full_when_no_main_content() {
        // A fragment with no extractable article: distilled must not lose the content.
        let html = "<html><body><p>lone snippet</p></body></html>";
        let distilled = render("text/html", html, FetchMode::Distilled, None);
        assert!(distilled.contains("lone snippet"), "{distilled}");
    }

    #[test]
    fn render_json_and_xml_passthrough() {
        assert_eq!(
            render("application/json", r#"{"ok":true}"#, FetchMode::Full, None),
            r#"{"ok":true}"#
        );
        assert_eq!(
            render(
                "application/ld+json",
                r#"{"@type":"Thing"}"#,
                FetchMode::Full,
                None
            ),
            r#"{"@type":"Thing"}"#
        );
        assert_eq!(
            render("application/xml", "<root/>", FetchMode::Full, None),
            "<root/>"
        );
        assert_eq!(
            render("application/atom+xml", "<feed/>", FetchMode::Full, None),
            "<feed/>"
        );
    }

    #[test]
    fn render_still_rejects_binary_types() {
        let out = render("application/octet-stream", "data", FetchMode::Full, None);
        assert!(out.contains("unsupported content type"));
    }

    #[tokio::test]
    async fn fetches_json_api_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"status":"ok"}"#),
            )
            .mount(&server)
            .await;

        let url = format!("{}/api", server.uri());
        let out = loopback_tool()
            .run(serde_json::json!({ "url": url }), Path::new("."))
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains(r#""status":"ok""#), "{}", out.content);
    }

    #[tokio::test]
    async fn rejects_oversized_body() {
        let server = MockServer::start().await;
        let big = "x".repeat(MAX_DOWNLOAD_BYTES as usize + 1);
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/plain")
                    .set_body_string(big),
            )
            .mount(&server)
            .await;

        let out = loopback_tool()
            .run(serde_json::json!({ "url": server.uri() }), Path::new("."))
            .await;
        assert!(!out.success, "{}", out.content);
        assert!(out.content.contains("download limit"), "{}", out.content);
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
