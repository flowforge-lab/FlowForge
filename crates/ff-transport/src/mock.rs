use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::transport::{MessageTransport, ResponseStream, ShutdownHandle};
use crate::types::{ChannelId, InboundMessage, Notification};

/// Collected response chunks for a single response stream.
#[derive(Debug, Clone, Default)]
pub struct ResponseRecord {
    pub chunks: Vec<String>,
    pub finished: bool,
}

/// A mock transport for testing. Messages are injected via `send()` and
/// responses are collected in `responses()`.
pub struct MockTransport {
    name: String,
    rx: mpsc::UnboundedReceiver<InboundMessage>,
    tx: Option<mpsc::UnboundedSender<InboundMessage>>,
    responses: Arc<Mutex<Vec<ResponseRecord>>>,
    notifications: Arc<Mutex<Vec<(ChannelId, Notification)>>>,
}

impl MockTransport {
    pub fn new(name: impl Into<String>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            name: name.into(),
            rx,
            tx: Some(tx),
            responses: Arc::new(Mutex::new(Vec::new())),
            notifications: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get a sender handle to inject inbound messages.
    ///
    /// # Panics
    /// If called after shutdown, when the transport has released its sender.
    pub fn sender(&self) -> mpsc::UnboundedSender<InboundMessage> {
        self.tx
            .clone()
            .expect("sender() after shutdown: the transport has released its sender")
    }

    /// Get all collected response records.
    pub fn responses(&self) -> Vec<ResponseRecord> {
        self.responses.lock().unwrap().clone()
    }

    /// Get all collected notifications.
    pub fn notifications(&self) -> Vec<(ChannelId, Notification)> {
        self.notifications.lock().unwrap().clone()
    }
}

#[async_trait]
impl MessageTransport for MockTransport {
    fn name(&self) -> &str {
        &self.name
    }

    async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn recv(&mut self) -> Option<InboundMessage> {
        self.rx.recv().await
    }

    fn begin_response(&self, _channel: &ChannelId) -> Box<dyn ResponseStream> {
        let record = ResponseRecord::default();
        let idx = {
            let mut responses = self.responses.lock().unwrap();
            responses.push(record);
            responses.len() - 1
        };
        Box::new(MockResponseStream {
            responses: self.responses.clone(),
            idx,
        })
    }

    fn notify(&self, channel: &ChannelId, notification: Notification) {
        self.notifications
            .lock()
            .unwrap()
            .push((channel.clone(), notification));
    }

    /// Closes the inbound channel on signal by releasing the sender the mock
    /// holds for injection. Anything already queued is still delivered — an
    /// `UnboundedReceiver` drains its buffer before reporting the close — so this
    /// exercises the same "finish in-flight work, then stop" path Slack uses.
    fn shutdown_handle(&mut self) -> ShutdownHandle {
        let (handle, notify) = ShutdownHandle::new();
        let Some(tx) = self.tx.take() else {
            return handle;
        };
        // Keep the sender alive inside the task so the channel stays open until
        // the signal arrives, then drop it.
        tokio::spawn(async move {
            notify.notified().await;
            drop(tx);
        });
        handle
    }
}

struct MockResponseStream {
    responses: Arc<Mutex<Vec<ResponseRecord>>>,
    idx: usize,
}

#[async_trait]
impl ResponseStream for MockResponseStream {
    async fn chunk(&self, text: &str) {
        let mut responses = self.responses.lock().unwrap();
        responses[self.idx].chunks.push(text.to_string());
    }

    async fn finish(&self) {
        let mut responses = self.responses.lock().unwrap();
        responses[self.idx].finished = true;
    }
}
