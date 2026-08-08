//! [`Mode`] ↔ [`wire::SessionMode`].
//!
//! ACP's mode vocabulary is **agent-defined**: `SessionMode` carries an opaque `id`
//! plus a human `name`/`description`, and the protocol accepts any id we choose.
//! Nothing upstream validates these strings, so this mapping is entirely ours — which
//! is exactly why it needs tests that assert the wire-visible bytes.
//!
//! The ids are taken from [`Mode`]'s own serde representation rather than written out
//! again here, so the ACP surface cannot drift away from the IPC/settings surface.

use crate::wire;
use ff_core::Mode;

/// Every mode FlowForge exposes, in the order clients should display them.
const ALL: [Mode; 3] = [Mode::Plan, Mode::Act, Mode::Auto];

/// The ACP id for a mode.
///
/// Must equal `Mode`'s own serde form (`rename_all = "camelCase"`) so the ACP surface
/// cannot drift from the `--mode` flag and the TypeScript bindings. That equivalence is
/// enforced by [`tests::ids_match_modes_own_serde_form`] rather than by routing every
/// lookup through `serde_json` — the test is what prevents a second vocabulary, so the
/// implementation can stay allocation-free.
pub fn mode_id(mode: Mode) -> &'static str {
    match mode {
        Mode::Plan => "plan",
        Mode::Act => "act",
        Mode::Auto => "auto",
    }
}

/// Resolve an ACP mode id back to a [`Mode`].
///
/// Takes the typed [`wire::SessionModeId`] rather than a bare `&str` so callers cannot
/// pass a mode id where some other protocol string was meant — the same newtype
/// discipline `session.rs` keeps for [`wire::SessionId`].
///
/// Returns `None` for anything unrecognised. This is deliberately *not* a fallback to
/// [`Mode::Plan`]: silently coercing an unknown id would leave us and the client
/// disagreeing about what mode the session is in, and "safe-looking" is not the same
/// as safe when the disagreement is invisible. `Option` (rather than the spec's
/// `Result<_, wire::Error>`) is faithful while there is no server: only the #1201
/// request handler can turn a miss into a JSON-RPC error with a real request id, so
/// minting a `wire::Error` here would fabricate one. #1201 lifts this to `Result` at
/// the call site.
pub fn mode_from_id(id: &wire::SessionModeId) -> Option<Mode> {
    ALL.into_iter().find(|m| mode_id(*m) == &*id.0)
}

fn name(mode: Mode) -> &'static str {
    match mode {
        Mode::Plan => "Plan",
        Mode::Act => "Act",
        Mode::Auto => "Auto",
    }
}

/// Descriptions shown in the client's mode picker. These mirror the doc comments on
/// [`Mode`]; they describe the *default* matrix cells, since a custom matrix can
/// change the per-tier outcome.
fn description(mode: Mode) -> &'static str {
    match mode {
        Mode::Plan => "Read-only tools only; nothing can mutate",
        Mode::Act => "Full toolset; dangerous actions ask for confirmation",
        Mode::Auto => "Full toolset; sensitive actions ask, dangerous actions are denied",
    }
}

fn one(mode: Mode) -> wire::SessionMode {
    // Upstream marks these types `#[non_exhaustive]`, so construction goes through
    // the builder rather than a struct literal.
    wire::SessionMode::new(mode_id(mode), name(mode)).description(description(mode))
}

/// The full mode state to report in `session/new` and `session/set_mode`.
pub fn mode_state(current: Mode) -> wire::SessionModeState {
    wire::SessionModeState::new(mode_id(current), ALL.into_iter().map(one).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_state_uses_the_protocols_field_names() {
        let json = serde_json::to_value(mode_state(Mode::Plan)).unwrap();
        assert_eq!(json["currentModeId"], "plan");

        // The field is `id`. #1200 hand-wrote `modeId` here and pinned that mistake in
        // its own fixtures; asserting the wire name is what makes the error visible.
        let modes = json["availableModes"].as_array().unwrap();
        assert_eq!(modes.len(), 3);
        assert_eq!(modes[0]["id"], "plan");
        assert_eq!(modes[1]["id"], "act");
        assert_eq!(modes[2]["id"], "auto");
        assert!(
            modes[0].get("modeId").is_none(),
            "`modeId` is not a field in ACP v1"
        );
    }

    #[test]
    fn every_advertised_mode_is_resolvable() {
        let json = serde_json::to_value(mode_state(Mode::Act)).unwrap();
        for m in json["availableModes"].as_array().unwrap() {
            let id = m["id"].as_str().unwrap();
            assert!(
                mode_from_id(&wire::SessionModeId::new(id)).is_some(),
                "advertised id {id:?} does not resolve back to a Mode"
            );
        }
    }

    #[test]
    fn ids_match_modes_own_serde_form() {
        // Guards against this crate growing a second mode vocabulary.
        for m in ALL {
            let via_serde = serde_json::to_value(m).unwrap();
            assert_eq!(via_serde.as_str().unwrap(), mode_id(m));
        }
    }

    #[test]
    fn unknown_ids_do_not_fall_back() {
        assert_eq!(mode_from_id(&wire::SessionModeId::new("yolo")), None);
        assert_eq!(mode_from_id(&wire::SessionModeId::new("")), None);
        // Case matters: the protocol id is exact, not normalised.
        assert_eq!(mode_from_id(&wire::SessionModeId::new("Plan")), None);
    }

    #[test]
    fn each_mode_carries_a_name_and_description() {
        let json = serde_json::to_value(mode_state(Mode::Auto)).unwrap();
        for m in json["availableModes"].as_array().unwrap() {
            assert!(m["name"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(m["description"].as_str().is_some_and(|s| !s.is_empty()));
        }
    }
}
