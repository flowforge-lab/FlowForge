//! Transport abstraction layer for FlowForge messaging (#911, RFC 0021).
//!
//! Defines [`MessageTransport`] — the trait external messaging platforms implement
//! to bridge messages into FlowForge sessions — and [`Router`], the headless
//! orchestrator that maps inbound messages to sessions, drives agent turns, and
//! streams responses back through the transport.

mod approver;
mod channel_map;
mod router;
mod transport;
mod types;

pub use approver::MessagingApprover;
pub use channel_map::ChannelMap;
pub use router::{Router, RouterConfig};
pub use transport::{MessageTransport, ResponseStream, ShutdownHandle};
pub use types::{ChannelId, InboundMessage, Notification};

#[cfg(test)]
mod mock;
#[cfg(test)]
pub use mock::MockTransport;

#[cfg(test)]
mod tests;
