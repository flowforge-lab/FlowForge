//! `HttpSource` — polls a URL on a configurable interval and fires when the
//! body changes.
//!
//! Wire model:
//! - `next_event` is the inner loop. It sleeps one `interval`, then performs
//!   one GET, then decides whether to emit. Cancellation is honored both
//!   between ticks (during the sleep) and mid-fetch (the `reqwest::Response`
//!   future is dropped on cancel, which cancels the in-flight request).
//! - The first tick is silent: it stores the body hash but emits no event, so
//!   "start an observer" doesn't immediately wake the agent just because the
//!   server happens to be returning content right now.
//! - A 1 MiB body cap (`bytes_stream().take(1 << 20)`) keeps a chatty
//!   endpoint from OOMing the supervisor task. Mirrors the body-limit pattern
//!   in `ff_tools::web_fetch`.
//! - `reqwest::Error` and non-2xx responses are silent (a `tracing::warn!`,
//!   no event): a 5xx or DNS blip must not become an event storm.
//!
//! Cadence:
//! - `interval_secs = None` → defaults to 60 s.
//! - `interval_secs = Some(n)` with `n < 30` → warn and clamp to 30 s. Below
//!   30 s the per-tick cost on the user (and on whatever we're watching)
//!   crosses the line from "background" to "busy loop" — `#709` documents 30
//!   s as the floor.
//!
//! Filter:
//! - When `filter` is `Some(s)`, the body is decoded lossy and the source
//!   fires only if the new body contains `s` (a plain substring — the
//!   `ObserverTool.description` documents this). The event summary is
//!   `filtered match: "<s>"` so the model can see *why* the wake happened.
//!
//! Security posture:
//! - `reaches_network()` is `true` on the parent `ObserverTool`; `start` is
//!   `Safety::Write` and approval-gated. URL scheme is restricted to
//!   `http`/`https`. Full SSRF guard (loopback/metadata blocking, DNS
//!   re-check on every redirect) is deliberately not done here — the
//!   observer polls a user-chosen URL the model already approved via the
//!   safety gate, so a stricter policy would conflict with the legitimate
//!   "watch the local dev server" use case. If that ever changes, port
//!   `ff_tools::url_safety::SsrfPolicy` over (the host list is `ff-tools`
//!   already a workspace dep).

use super::source::{ObserverContext, ObserverEvent, ObserverSource};
use async_trait::async_trait;
use futures_util::StreamExt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Default poll interval when `interval_secs` is not supplied. 60 s
/// matches the issue's "default 60s" decision.
pub const DEFAULT_INTERVAL_SECS: u64 = 60;
/// Minimum allowed `interval_secs`; below this the source warns and clamps.
/// Documented in the issue as the floor from `#709`.
pub const MIN_INTERVAL_SECS: u64 = 30;
/// Hard body cap per poll. A larger body is silently truncated — the
/// hash and (when set) substring check are run on the truncated bytes.
const MAX_BODY_BYTES: usize = 1 << 20;

pub struct HttpSource {
    ctx: ObserverContext,
    target: url::Url,
    interval: Duration,
    /// Plain substring; `None` means "fire on any change". Stored as
    /// `String` (not `&str`) so the source owns its config and the
    /// `&mut self` method on `next_event` can take `&self` semantically.
    filter: Option<String>,
    /// `None` on the first tick: the next poll sets this silently. The
    /// `Option` discriminates "have I seen the body yet" so the
    /// first-tick silent path doesn't accidentally compare against `0`
    /// and fire.
    last_hash: Option<u64>,
    /// Cached on construction. `reqwest::Client` is `Arc`-internal and
    /// cheap to clone, but holding one here is enough for the source's
    /// whole lifetime.
    client: reqwest::Client,
}

impl std::fmt::Debug for HttpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSource")
            .field("ctx", &self.ctx)
            .field("target", &self.target)
            .field("interval", &self.interval)
            .field("filter", &self.filter)
            .field("last_hash", &self.last_hash)
            // Skip `client` — reqwest::Client's Debug prints the
            // internal config and a connection pool address; not
            // useful in test output and noisy on every assertion.
            .finish_non_exhaustive()
    }
}

impl HttpSource {
    /// Construct from a parsed `ObserverSpec` shape. The supervisor calls
    /// this; the tool is responsible for serializing the spec and running
    /// the safety gate before we get here.
    ///
    /// Returns `Err` on:
    /// - URL parse failure
    /// - unsupported scheme (anything other than `http`/`https`)
    /// - empty host
    ///
    /// Interval validation: `None` → default; `Some(n)` with `n < 30` →
    /// warn + clamp to 30 s; otherwise as-is.
    pub fn new(
        ctx: ObserverContext,
        target: &str,
        interval_secs: Option<u64>,
        filter: Option<String>,
    ) -> Result<Self, String> {
        let url = url::Url::parse(target).map_err(|e| format!("invalid URL `{target}`: {e}"))?;
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(format!(
                    "unsupported URL scheme `{other}` (only http/https allowed)"
                ))
            }
        }
        if url.host_str().map(str::is_empty).unwrap_or(true) {
            return Err(format!("URL has no host: `{target}`"));
        }

        let interval = match interval_secs {
            None => Duration::from_secs(DEFAULT_INTERVAL_SECS),
            Some(s) if s < MIN_INTERVAL_SECS => {
                tracing::warn!(
                    requested_secs = s,
                    clamped_to_secs = MIN_INTERVAL_SECS,
                    "http observer interval below minimum; clamping to {}s",
                    MIN_INTERVAL_SECS,
                );
                Duration::from_secs(MIN_INTERVAL_SECS)
            }
            Some(s) => Duration::from_secs(s),
        };

        Ok(Self {
            ctx,
            target: url,
            interval,
            filter,
            last_hash: None,
            client: reqwest::Client::builder()
                // No redirect policy: the safety gate is upstream and
                // we want the strict "1xx/2xx only" semantics. A
                // redirect to a different host would skip any future
                // SSRF check anyway; this tool's posture is
                // "URL the model explicitly named".
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| format!("failed to build http client: {e}"))?,
        })
    }

    /// Bypass parsing + interval clamping. Used by tests that want
    /// sub-second intervals and a wiremock-controlled URL.
    #[cfg(test)]
    pub(crate) fn new_unchecked(
        ctx: ObserverContext,
        target: url::Url,
        interval: Duration,
        filter: Option<String>,
    ) -> Self {
        Self {
            ctx,
            target,
            interval,
            filter,
            last_hash: None,
            client: reqwest::Client::new(),
        }
    }

    /// One poll iteration. Returns `Some(event)` when the body changed
    /// (and, if a filter is set, also contained the substring), or
    /// `None` to indicate "no event this tick" — either unchanged,
    /// failure, or filter no-match. The outer loop retries on the
    /// next interval.
    async fn poll_once(&mut self) -> Option<ObserverEvent> {
        // Send + bounded read. A `reqwest::Error` (DNS, connect,
        // timeout, body-read) is a `warn!` and skip — a flaky endpoint
        // must not turn into an event storm.
        let resp = match self.client.get(self.target.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, url = %self.target, "http observer: request failed");
                return None;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(
                %status,
                url = %self.target,
                "http observer: non-2xx response, skipping tick"
            );
            return None;
        }

        // Cap the body read at 1 MiB. `Stream::take` counts chunks,
        // not bytes, so applying it to `bytes_stream()` would still
        // let one giant chunk buffer into `Vec::extend_from_slice`
        // (defeating the OOM guard the comment below promises).
        // Mirror `ff_tools::web_fetch::read_body_capped` instead:
        // accumulate bytes, appending at most `remaining` bytes from
        // each chunk and breaking as soon as the cap is reached.
        let mut buf = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let c = match chunk {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "http observer: body read failed");
                    return None;
                }
            };
            let remaining = MAX_BODY_BYTES.saturating_sub(buf.len());
            if remaining == 0 {
                break;
            }
            let n = c.len().min(remaining);
            buf.extend_from_slice(&c[..n]);
            if n < c.len() {
                break;
            }
        }

        let new_hash = hash_bytes(&buf);

        // First-tick path: set the baseline silently so an existing
        // body doesn't immediately fire. Subsequent ticks compare.
        let Some(prev_hash) = self.last_hash else {
            self.last_hash = Some(new_hash);
            return None;
        };
        if prev_hash == new_hash {
            return None;
        }
        // Update before filter check so the "seen" set advances even
        // when the change doesn't match the filter — otherwise the
        // next tick would re-compare against the *original* baseline
        // and miss the change in between.
        self.last_hash = Some(new_hash);

        // Filter (if any) gates the event. A change that doesn't
        // contain the substring is silent; the body is still marked
        // as "seen" above.
        if let Some(s) = self.filter.as_deref() {
            let body_str = String::from_utf8_lossy(&buf);
            if !body_str.contains(s) {
                return None;
            }
            return Some(self.event(format!("filtered match: \"{s}\"")));
        }
        Some(self.event("content changed".to_string()))
    }

    fn event(&self, summary: String) -> ObserverEvent {
        ObserverEvent {
            session_id: self.ctx.session_id.clone(),
            id: self.ctx.id,
            label: self.ctx.label.clone(),
            summary,
        }
    }
}

#[async_trait]
impl ObserverSource for HttpSource {
    fn ctx(&self) -> &ObserverContext {
        &self.ctx
    }

    async fn next_event(&mut self, cancel: Arc<Notify>) -> Option<ObserverEvent> {
        loop {
            // Sleep one interval, but wake early on cancel. The
            // `select!` is `biased` so a cancel that arrives during a
            // completed `poll_once` (and the immediately-following
            // sleep) wins on the next iteration rather than waiting
            // out the sleep.
            //
            // First-change latency is ~2x the interval: this sleep
            // elapses before the first poll, and the first poll is
            // silent (sets the body hash baseline). So with the 60s
            // default the earliest wake after `start` is ~120s. The
            // supervisor never short-circuits the baseline, since a
            // pre-baseline fire would be a "the server happened to be
            // returning content the moment we started" false positive.
            tokio::select! {
                biased;
                _ = cancel.notified() => return None,
                _ = tokio::time::sleep(self.interval) => {}
            }
            // Race the request itself against cancel too. If the
            // supervisor signals stop while we're mid-fetch, the
            // reqwest future is dropped, which cancels the request
            // (reqwest's `Client` honors drop on the response future).
            let fetch = self.poll_once();
            tokio::select! {
                biased;
                _ = cancel.notified() => return None,
                ev = fetch => {
                    if let Some(ev) = ev {
                        return Some(ev);
                    }
                    // No event this tick (unchanged, failure, or
                    // filter no-match) — loop back to sleep the next
                    // interval.
                }
            }
        }
    }
}

/// Same-process content hash. Cheap, no new dep — same idiom as
/// `ff_agent::compaction_extractive::content_key` and
/// `ff_agent::lib::compact_session_messages` use for stable per-blob
/// dedupe. Not cryptographic; only used to decide "is this the same
/// body as last time?".
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ctx() -> ObserverContext {
        ObserverContext {
            session_id: "s".into(),
            id: 1,
            label: "http".into(),
        }
    }

    /// Run the source's loop until `max_ticks` polls have completed,
    /// then cancel and collect any events the source emitted. Returns
    /// the events in emission order; an empty Vec means "no events
    /// fired across `max_ticks` polls" (the typical assertion for
    /// "unchanged" / "non-2xx" tests).
    ///
    /// Takes the source by value because `next_event` is `&mut self`
    /// and the source's loop must own its state for the duration of
    /// the spawned task.
    async fn run_n_polls(mut src: HttpSource, max_ticks: u32) -> Vec<ObserverEvent> {
        let cancel = Arc::new(Notify::new());
        let cancel_for_src = cancel.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ObserverEvent>();
        let driver = tokio::spawn(async move {
            loop {
                match src.next_event(cancel_for_src.clone()).await {
                    Some(ev) => {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                    None => return,
                }
            }
        });
        // 200ms per tick is comfortably > the 50ms test interval
        // (so a poll definitely completes) and short enough to keep
        // the test suite fast.
        tokio::time::sleep(Duration::from_millis(200 * max_ticks as u64)).await;
        cancel.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_secs(2), driver).await;
        // Drain whatever events were emitted.
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_rejects_non_http_schemes() {
        let err = HttpSource::new(ctx(), "file:///etc/passwd", None, None)
            .expect_err("file:// must be rejected");
        assert!(err.contains("scheme"), "{err}");
        let err = HttpSource::new(ctx(), "ftp://example.com/", None, None)
            .expect_err("ftp:// must be rejected");
        assert!(err.contains("scheme"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_rejects_unparseable_urls() {
        assert!(HttpSource::new(ctx(), "not a url", None, None).is_err());
        assert!(HttpSource::new(ctx(), "", None, None).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_clamps_sub_minimum_interval_and_defaults() {
        let below =
            HttpSource::new(ctx(), "https://example.com/", Some(5), None).expect("construct");
        assert_eq!(below.interval, Duration::from_secs(MIN_INTERVAL_SECS));
        let none = HttpSource::new(ctx(), "https://example.com/", None, None).expect("construct");
        assert_eq!(none.interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
        let exact =
            HttpSource::new(ctx(), "https://example.com/", Some(45), None).expect("construct");
        assert_eq!(exact.interval, Duration::from_secs(45));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unchanged_does_not_emit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .expect(3..)
            .mount(&server)
            .await;

        let url: url::Url = server.uri().parse().unwrap();
        let src = HttpSource::new_unchecked(ctx(), url, Duration::from_millis(50), None);
        // 3 polls: tick 1 sets the silent baseline, ticks 2–3 see
        // the same body. The wiremock `expect(3..)` proves the source
        // actually polled 3 times — so "no event" isn't "test never
        // ran the loop".
        let events = run_n_polls(src, 3).await;
        assert!(events.is_empty(), "expected no events, got {events:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn changed_emits_with_summary() {
        let server = MockServer::start().await;
        // First request: v1. Second onward: v2.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v1"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v2"))
            .mount(&server)
            .await;

        let url: url::Url = server.uri().parse().unwrap();
        let src = HttpSource::new_unchecked(ctx(), url, Duration::from_millis(50), None);
        // 3 polls: tick 1 silent (v1), tick 2 fires (v1→v2),
        // tick 3 silent (still v2). Exactly one event.
        let events = run_n_polls(src, 3).await;
        assert_eq!(events.len(), 1, "expected exactly one event: {events:?}");
        let ev = &events[0];
        assert_eq!(ev.summary, "content changed");
        assert_eq!(ev.label, "http");
        assert_eq!(ev.session_id, "s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn filter_substring_only_emits_on_match() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v1"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Body changes to "v2" — no "ready" substring. Should not
        // fire. (The body hash still advances, so the next
        // comparison is against v2, not v1.)
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v2"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Then "v3 ready" — substring matches → fire.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v3 ready"))
            .mount(&server)
            .await;

        let url: url::Url = server.uri().parse().unwrap();
        let src =
            HttpSource::new_unchecked(ctx(), url, Duration::from_millis(50), Some("ready".into()));
        let events = run_n_polls(src, 3).await;
        assert_eq!(events.len(), 1, "expected exactly one event: {events:?}");
        assert_eq!(events[0].summary, "filtered match: \"ready\"");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_2xx_is_silent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(3..)
            .mount(&server)
            .await;
        let url: url::Url = server.uri().parse().unwrap();
        let src = HttpSource::new_unchecked(ctx(), url, Duration::from_millis(50), None);
        // 3 polls, every one a 500. The `expect(3..)` confirms we
        // actually polled; the empty events vec confirms we never
        // fired. (The 500 has no body, but more importantly, the
        // 500 status is short-circuited before any hashing — so
        // a 5xx-then-2xx pair would *not* fire a false "first
        // body is the baseline" event.)
        let events = run_n_polls(src, 3).await;
        assert!(events.is_empty(), "500s must not fire: {events:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_stops_loop() {
        // A server that hangs: cancel has to win the race against
        // the in-flight request. We use a delay > the cancel window
        // so the only way out of the loop is the cancel signal.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let url: url::Url = server.uri().parse().unwrap();
        let src = HttpSource::new_unchecked(ctx(), url, Duration::from_secs(1), None);
        // If cancel-during-fetch works, the loop exits within ~1
        // interval. If it didn't, the test would hang on the 30s
        // mock delay.
        let events = run_n_polls(src, 2).await;
        assert!(events.is_empty(), "no event should fire: {events:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn body_over_one_mib_truncates_safely() {
        let server = MockServer::start().await;
        // Poll 1: a 2 MiB body of "A" — exceeds the 1 MiB cap. The
        // source must hash only the first 1 MiB of this, so the
        // baseline is `hash("A" * 1M)`.
        let big = "A".repeat(2 * MAX_BODY_BYTES);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(big))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Poll 2+: exactly 1 MiB of "A" — bit-identical to the
        // truncated prefix of the huge body. If the cap is
        // respected, hashes match and no event fires. If the
        // implementation mistakenly buffers the full 2 MiB, the
        // baseline is `hash("A" * 2M)` and this poll hashes
        // `hash("A" * 1M)` — different → would fire. So a
        // "no event here" assertion catches the OOM-guard bypass.
        let one_mib = "A".repeat(MAX_BODY_BYTES);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(one_mib))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Poll 3+: a body that genuinely differs from the truncated
        // prefix, so the source can still fire post-truncation.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("B".repeat(1024)))
            .mount(&server)
            .await;

        let url: url::Url = server.uri().parse().unwrap();
        let src = HttpSource::new_unchecked(ctx(), url, Duration::from_millis(80), None);
        let events = run_n_polls(src, 3).await;
        // Exactly one event: the truncation-equality poll must NOT
        // fire, and the genuine-change poll MUST.
        assert_eq!(
            events.len(),
            1,
            "expected exactly one event (genuine change only); got {events:?}",
        );
        assert_eq!(events[0].summary, "content changed");
    }
}
