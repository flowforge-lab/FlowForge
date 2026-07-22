//! Slack Socket Mode adapter for FlowForge messaging (#912 T2, RFC 0021 §5.1).
//!
//! This crate is the Slack-specific transport. T2 lands only the offline
//! protocol layer: parsing Socket Mode envelopes into a typed [`SlackEnvelope`].
//! The live WebSocket connection and the [`ff_transport::MessageTransport`]
//! implementation (writer/reader demux, ack, streaming edits) arrive in T3.

mod envelope;

pub use envelope::{parse_envelope, ParseError, SlackEnvelope, SlackInteraction};

#[cfg(test)]
mod tests;
