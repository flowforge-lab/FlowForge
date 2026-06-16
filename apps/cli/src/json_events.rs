//! JSON-line renderer for [`AgentEvent`]. Each event becomes one line of JSON on
//! stdout; nothing else leaks to stdout in --json mode.

use std::io::{self, Write};

use ff_agent::AgentEvent;

/// Serialize an [`AgentEvent`] as a single JSON line and write it to stdout.
pub fn emit_line(event: &AgentEvent) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    emit_line_to(event, &mut out).expect("stdout writable");
}

/// Serialize an [`AgentEvent`] as a single JSON line and write it to `out`.
pub fn emit_line_to(event: &AgentEvent, out: &mut impl Write) -> io::Result<()> {
    let json = serde_json::to_string(event).expect("AgentEvent serializable");
    writeln!(out, "{json}")?;
    out.flush()
}
