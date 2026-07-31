//! Slack Socket Mode adapter for FlowForge messaging (#912 T2/T3, RFC 0021 §5.1).
//!
//! This crate is the Slack-specific transport. T2 landed the offline protocol
//! layer: parsing Socket Mode envelopes into a typed [`SlackEnvelope`]. T3 adds
//! the live connection and the [`ff_transport::transport::MessageTransport`]
//! implementation: a writer/reader demux over one WebSocket (so transport and a
//! future interactive approver share one socket without contention), ack of
//! Socket Mode envelopes, and throttled, 3000-char-chunked streaming edits via
//! the Slack Web API.
//!
//! Reconnect/backoff is deferred to Phase 2 (RFC 0021 §9): a dropped socket ends
//! `recv`, which the Router treats as a clean stop.

mod api;
mod approver;
mod envelope;
mod response;
mod transport;
mod writer;

pub use api::{ApiError, SlackApi};
pub use approver::{SlackApprover, ACTION_APPROVE, ACTION_DENY, DEFAULT_TIMEOUT};
pub use envelope::{parse_envelope, ParseError, SlackEnvelope, SlackInteraction};
pub use response::{SlackResponseStream, EDIT_THROTTLE, SLACK_TEXT_LIMIT};
pub use transport::{SlackTransport, TRANSPORT_NAME};
pub use writer::{OutboundOp, WriterHandle};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_t3;
#[cfg(test)]
mod tests_t4;
