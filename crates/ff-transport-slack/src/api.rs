//! Thin Slack Web API client for outbound messages (#912 T3, RFC 0021 §5.1).
//!
//! Socket Mode carries inbound events, but application replies go over the Web
//! API (HTTPS): `chat.postMessage` for a new message, `chat.update` to edit one
//! in place (streaming edits). This is the minimal surface T3 needs; richer
//! Block Kit rendering is deferred.

use serde::Deserialize;

const SLACK_API_BASE: &str = "https://slack.com/api";

/// Errors talking to the Slack Web API.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The HTTP request itself failed (network, TLS, timeout).
    #[error("slack api transport: {0}")]
    Http(String),
    /// Slack returned `{ "ok": false, "error": "..." }`.
    #[error("slack api error: {0}")]
    Slack(String),
    /// The response was missing a field we require (e.g. `ts` on a post).
    #[error("slack api: malformed response (missing {0})")]
    Malformed(&'static str),
}

/// A Slack Web API client bound to one bot token.
#[derive(Clone)]
pub struct SlackApi {
    http: reqwest::Client,
    token: String,
    base: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    ok: bool,
    error: Option<String>,
    /// Present on success for `chat.postMessage` (the new message's timestamp).
    ts: Option<String>,
}

impl SlackApi {
    /// Build a client for `bot_token` (a `xoxb-...` token).
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            token: bot_token.into(),
            base: SLACK_API_BASE.to_string(),
        }
    }

    /// Override the API base URL (used by tests to point at a mock server).
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// Reuse an existing HTTP client instead of the one built by [`new`]. Lets a
    /// caller share a single connection pool across the connect handshake and
    /// the Web API calls.
    ///
    /// [`new`]: SlackApi::new
    pub fn with_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// Post a new message to `channel`; returns its `ts` on success.
    pub async fn post_message(&self, channel: &str, text: &str) -> Result<String, ApiError> {
        let body = serde_json::json!({ "channel": channel, "text": text });
        let resp = self.call("chat.postMessage", &body).await?;
        resp.ts.ok_or(ApiError::Malformed("ts"))
    }

    /// Edit the message at `ts` in `channel` in place.
    pub async fn update_message(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({ "channel": channel, "ts": ts, "text": text });
        self.call("chat.update", &body).await.map(|_| ())
    }

    /// Post an interactive message built from Block Kit `blocks`; returns its `ts`.
    ///
    /// `text` is still sent as the notification fallback — Slack uses it for push
    /// notifications and accessibility, where blocks are not rendered.
    pub async fn post_blocks(
        &self,
        channel: &str,
        text: &str,
        blocks: serde_json::Value,
    ) -> Result<String, ApiError> {
        let body = serde_json::json!({ "channel": channel, "text": text, "blocks": blocks });
        let resp = self.call("chat.postMessage", &body).await?;
        resp.ts.ok_or(ApiError::Malformed("ts"))
    }

    async fn call(&self, method: &str, body: &serde_json::Value) -> Result<ChatResponse, ApiError> {
        let url = format!("{}/{}", self.base, method);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        if !parsed.ok {
            return Err(ApiError::Slack(
                parsed.error.unwrap_or_else(|| "unknown".to_string()),
            ));
        }
        Ok(parsed)
    }
}
