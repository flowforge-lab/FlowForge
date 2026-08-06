//! FlowForge ↔ ACP mapping layer.
//!
//! This crate deliberately owns **no wire types**. The Agent Client Protocol's
//! vocabulary comes from the official [`agent_client_protocol`] crate, re-exported
//! below as [`wire`]. What lives here is only the part ACP has no opinion about and
//! FlowForge does: how our modes, permission cells, tool advertisement and
//! cancellation map onto the protocol's shapes.
//!
//! # Why the wire types are not hand-written (#1215)
//!
//! They were, once (#1200), and three of them silently disagreed with the schema —
//! including `session/prompt`, which invented a `PromptMessage {role, content}`
//! wrapper around what is actually a flat `Vec<ContentBlock>`. All 103 tests passed,
//! because every fixture was built from our own shape: the suite proved
//! self-consistency and nothing else. A negative test can show a field is *required*;
//! it can never show a field is *correctly named*.
//!
//! The lesson generalises to anything on this boundary: **a fixture is only
//! schema-derived if the JSON text came from the schema.** JSON we generate from our
//! own types is self-satisfying however much verbatim text is involved. Tests here
//! assert wire-visible results, and the permission round-trip uses bytes produced by
//! the official serializer so it cannot degenerate that way.
//!
//! # Why `v1` is pinned structurally
//!
//! In the schema crate, protocol version is a **module path plus a feature gate**
//! (`pub mod v1;` and `#[cfg(feature = "unstable_protocol_v2")] pub mod v2;`), not the
//! crate version. Depending on `schema::v1` therefore pins the protocol harder than
//! hand-writing did: v2 is invisible unless the feature is enabled, and enabling it
//! deliberately withdraws `LATEST` to force an explicit choice.
//!
//! Do **not** enable `unstable_protocol_v2` to "stay current". v2 is a draft, and a
//! silent protocol upgrade is precisely the drift this crate exists to end.

pub use agent_client_protocol::schema::v1 as wire;

pub mod advertise;
pub mod mode;
pub mod permission;
pub mod session;
