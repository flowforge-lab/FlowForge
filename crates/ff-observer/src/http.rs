//! `HttpSource` — periodic GET + body-hash diff. Min interval 30s.

#[cfg(test)]
mod tests;

use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use regex::Regex;
use sha2::{Digest, Sha256};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::event::{ObserverError, ObserverEvent, ObserverSpec};
use crate::source::ObserverSource;

/// Min poll interval — too-aggressive polling against remote hosts is bad
/// citizenship and the `reqwest` client is configured for the 30s minimum.
pub const MIN_INTERVAL: Duration = Duration::from_secs(30);
/// Default poll interval when the user does not provide one.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);
/// Max bytes we'll buffer for hashing. Big enough for typical status pages
/// (a few hundred KB); anything larger is truncated to the hash, so the
/// body is still diffable.
const MAX_BODY_BYTES: usize = 256 * 1024;
/// The body length at which we truncate the `summary` field; the URL + a
/// short fingerprint is enough for the agent to know what to do next.
const SUMMARY_PREVIEW: usize = 256;

#[derive(Debug)]
pub struct HttpSource {
    url: String,
    key: String,
    interval: Duration,
    filter: Option<Regex>,
    last_hash: Option<String>,
    client: reqwest::Client,
}

impl HttpSource {
    pub async fn from_spec(spec: ObserverSpec) -> Result<Self, ObserverError> {
        if spec.target.trim().is_empty() {
            return Err(ObserverError::InvalidTarget {
                kind: "http",
                reason: "URL must not be empty".into(),
            });
        }
        let url = spec.target.clone();
        let key = url.clone();
        let interval = spec.interval.unwrap_or(DEFAULT_INTERVAL).max(MIN_INTERVAL);
        let filter = spec
            .filter
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|e| ObserverError::InvalidFilter(e.to_string()))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| ObserverError::Other(format!("http client: {e}")))?;
        Ok(Self {
            url,
            key,
            interval,
            filter,
            last_hash: None,
            client,
        })
    }

    /// Hash the body, capped at MAX_BODY_BYTES. The cap is to keep memory
    /// bounded against an unexpectedly large response.
    fn hash(body: &[u8]) -> String {
        let mut hasher = Sha256::new();
        if body.len() > MAX_BODY_BYTES {
            hasher.update(&body[..MAX_BODY_BYTES]);
        } else {
            hasher.update(body);
        }
        hex::encode(hasher.finalize())
    }

    /// Build the summary the model will see. When the body is small, show a
    /// preview; when it's large, just a fingerprint line.
    fn summarize(&self, body: &str, hash: &str) -> String {
        let prev = body.get(..SUMMARY_PREVIEW).unwrap_or(body);
        if body.len() > SUMMARY_PREVIEW {
            format!(
                "content changed ({} bytes, sha256:{}…)",
                body.len(),
                &hash[..16]
            )
        } else {
            format!("content changed:\n{prev}")
        }
    }
}

#[async_trait]
impl ObserverSource for HttpSource {
    fn key(&self) -> &str {
        &self.key
    }

    async fn prime(
        &mut self,
        _id: crate::event::ObserverId,
    ) -> Result<Option<ObserverEvent>, ObserverError> {
        match self.client.get(&self.url).send().await {
            Ok(resp) => match resp.error_for_status() {
                Ok(ok) => match ok.bytes().await {
                    Ok(bytes) => {
                        let hash = Self::hash(&bytes);
                        self.last_hash = Some(hash);
                        Ok(None)
                    }
                    Err(e) => {
                        warn!(error = %e, url = %self.url, "prime: read body failed");
                        Ok(None)
                    }
                },
                Err(e) => {
                    warn!(error = %e, url = %self.url, "prime: status");
                    Ok(None)
                }
            },
            Err(e) => {
                warn!(error = %e, url = %self.url, "prime: request");
                Ok(None)
            }
        }
    }

    /// Block until the next event fires or `cancel` is tripped. Returning
    /// `Ok(None)` signals the supervisor that the source has terminated
    /// cleanly (e.g. process exited, watched file removed). `Err` is treated
    /// as a recoverable error and surfaced to the host, not the model.
    /// `id` is the supervisor-assigned id; sources stamp it onto the
    /// returned event.
    async fn next_event(
        &mut self,
        id: crate::event::ObserverId,
        cancel: &CancellationToken,
    ) -> Result<Option<ObserverEvent>, ObserverError> {
        loop {
            // Poll at the configured interval, cancellable.
            tokio::select! {
                _ = cancel.cancelled() => return Ok(None),
                _ = sleep(self.interval) => {}
            }
            let res = self.client.get(&self.url).send().await;
            let resp = match res.and_then(|r| r.error_for_status()) {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, url = %self.url, "poll: request failed");
                    continue;
                }
            };
            let body = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, url = %self.url, "poll: read body failed");
                    continue;
                }
            };
            let hash = Self::hash(&body);
            let Some(prev) = self.last_hash.clone() else {
                self.last_hash = Some(hash);
                continue;
            };
            if prev == hash {
                continue;
            }
            self.last_hash = Some(hash.clone());
            // Apply filter if present. Match against the body text (so the
            // model can scope to a substring, e.g. "ready" or "error").
            let body_text = String::from_utf8_lossy(&body);
            if let Some(re) = &self.filter {
                if !re.is_match(&body_text) {
                    continue;
                }
            }
            let summary = self.summarize(&body_text, &hash);
            return Ok(Some(ObserverEvent {
                id,
                key: self.key.clone(),
                summary,
                occurred_at: Utc::now(),
            }));
        }
    }
}
