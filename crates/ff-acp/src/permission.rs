//! [`PermissionCell`] ↔ `session/request_permission`.
//!
//! FlowForge has three permission outcomes; ACP's request/response can express only
//! two of them. The gap is not an oversight to paper over — it decides how `Deny` is
//! enforced at this boundary, so it is stated here and covered by tests.

use crate::wire;
use ff_agent::ApprovalOutcome;
use ff_core::{DenyReason, PermissionCell};

/// Our option ids. These are **ours**, not the protocol's — only [`wire::PermissionOptionKind`]
/// is standardised. Clients echo the id back in `SelectedPermissionOutcome`.
pub const ALLOW_ONCE: &str = "allow-once";
pub const ALLOW_ALWAYS: &str = "allow-always";
pub const REJECT_ONCE: &str = "reject-once";

/// Does this cell require asking the ACP client?
///
/// - [`PermissionCell::Allow`] → no round-trip.
/// - [`PermissionCell::Ask`] → a `session/request_permission` round-trip.
/// - [`PermissionCell::Deny`] → **no ACP equivalent**, and never reaches here.
///
/// ACP has no way to say "this tool exists but the model may not see it", so a `Deny`
/// cell is enforced by never advertising the tool (see [`crate::advertise`]). A `Deny`
/// arriving here would mean the advertised set has leaked, which is a security
/// regression rather than a case to map — hence `None` instead of a plausible-looking
/// default that would quietly turn it into a prompt.
pub fn needs_round_trip(cell: PermissionCell) -> Option<bool> {
    match cell {
        PermissionCell::Ask => Some(true),
        PermissionCell::Allow => Some(false),
        PermissionCell::Deny => None,
    }
}

/// The options offered for an `Ask`.
pub fn ask_options() -> Vec<wire::PermissionOption> {
    vec![
        wire::PermissionOption::new(
            ALLOW_ONCE,
            "Allow once",
            wire::PermissionOptionKind::AllowOnce,
        ),
        wire::PermissionOption::new(
            ALLOW_ALWAYS,
            "Always allow",
            wire::PermissionOptionKind::AllowAlways,
        ),
        wire::PermissionOption::new(
            REJECT_ONCE,
            "Reject",
            wire::PermissionOptionKind::RejectOnce,
        ),
    ]
}

/// Map the client's answer onto our outcome.
///
/// Two decisions worth keeping visible:
///
/// **`Cancelled` is not a rejection.** The spec requires a client to answer every
/// pending `session/request_permission` with `cancelled` when it sends
/// `session/cancel`. Folding that into [`DenyReason::User`] would record a user
/// decision that never happened and tell the model it was refused rather than
/// interrupted, so it maps to [`DenyReason::Cancelled`] (added for this boundary).
///
/// **The `*Always` persistence belongs to the client.** We treat an "always" answer as
/// the one-shot answer it is on this call and let the client remember it. Building our
/// own allowlist here would silently diverge from what the user sees in their editor,
/// and would outlive the session they granted it in.
pub fn outcome_to_approval(outcome: &wire::RequestPermissionOutcome) -> ApprovalOutcome {
    match outcome {
        wire::RequestPermissionOutcome::Cancelled => ApprovalOutcome::Denied(DenyReason::Cancelled),
        wire::RequestPermissionOutcome::Selected(selected) => {
            match selected.option_id.to_string().as_str() {
                ALLOW_ONCE | ALLOW_ALWAYS => ApprovalOutcome::Allowed,
                _ => ApprovalOutcome::Denied(DenyReason::User),
            }
        }
        // `#[non_exhaustive]` upstream: a future outcome we do not understand must not
        // be read as consent.
        _ => ApprovalOutcome::Denied(DenyReason::User),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes produced by the **official** serializer, parsed back by the **official**
    /// type, before we map. This is what stops the fixture from merely re-checking our
    /// own shape — the failure mode that let #1200 ship three wrong field names green.
    fn round_trip(outcome: &wire::RequestPermissionOutcome) -> wire::RequestPermissionOutcome {
        let bytes = serde_json::to_string(outcome).unwrap();
        serde_json::from_str(&bytes).unwrap()
    }

    #[test]
    fn cancelled_is_not_recorded_as_a_user_rejection() {
        let bytes = serde_json::to_string(&wire::RequestPermissionOutcome::Cancelled).unwrap();
        assert_eq!(bytes, r#"{"outcome":"cancelled"}"#);

        let parsed: wire::RequestPermissionOutcome = serde_json::from_str(&bytes).unwrap();
        assert_eq!(
            outcome_to_approval(&parsed),
            ApprovalOutcome::Denied(DenyReason::Cancelled),
            "a cancelled turn must not look like the user declined"
        );
    }

    #[test]
    fn cancelled_and_user_rejection_are_distinguishable() {
        let cancelled = outcome_to_approval(&wire::RequestPermissionOutcome::Cancelled);
        let rejected = outcome_to_approval(&wire::RequestPermissionOutcome::Selected(
            wire::SelectedPermissionOutcome::new(REJECT_ONCE),
        ));
        assert_ne!(
            cancelled, rejected,
            "collapsing these would report a decision the user never made"
        );
    }

    #[test]
    fn allow_answers_map_to_allowed() {
        for id in [ALLOW_ONCE, ALLOW_ALWAYS] {
            let outcome = round_trip(&wire::RequestPermissionOutcome::Selected(
                wire::SelectedPermissionOutcome::new(id),
            ));
            assert_eq!(
                outcome_to_approval(&outcome),
                ApprovalOutcome::Allowed,
                "{id} should allow"
            );
        }
    }

    #[test]
    fn reject_maps_to_a_user_denial() {
        let outcome = round_trip(&wire::RequestPermissionOutcome::Selected(
            wire::SelectedPermissionOutcome::new(REJECT_ONCE),
        ));
        assert_eq!(
            outcome_to_approval(&outcome),
            ApprovalOutcome::Denied(DenyReason::User)
        );
    }

    #[test]
    fn an_unrecognised_option_id_is_not_consent() {
        let outcome = round_trip(&wire::RequestPermissionOutcome::Selected(
            wire::SelectedPermissionOutcome::new("something-we-never-offered"),
        ));
        assert_eq!(
            outcome_to_approval(&outcome),
            ApprovalOutcome::Denied(DenyReason::User),
            "unknown answers must fail closed"
        );
    }

    #[test]
    fn offered_options_carry_our_ids_and_the_protocols_kinds() {
        let json = serde_json::to_value(ask_options()).unwrap();
        let opts = json.as_array().unwrap();
        assert_eq!(opts.len(), 3);

        // Our ids…
        assert_eq!(opts[0]["optionId"], ALLOW_ONCE);
        assert_eq!(opts[1]["optionId"], ALLOW_ALWAYS);
        assert_eq!(opts[2]["optionId"], REJECT_ONCE);
        // …and the protocol's kinds, which are snake_case on the wire.
        assert_eq!(opts[0]["kind"], "allow_once");
        assert_eq!(opts[1]["kind"], "allow_always");
        assert_eq!(opts[2]["kind"], "reject_once");
    }

    #[test]
    fn every_offered_option_maps_back_to_an_outcome() {
        for opt in ask_options() {
            let selected = wire::RequestPermissionOutcome::Selected(
                wire::SelectedPermissionOutcome::new(opt.option_id.clone()),
            );
            // No panic, and never an accidental Cancelled.
            let mapped = outcome_to_approval(&round_trip(&selected));
            assert_ne!(mapped, ApprovalOutcome::Denied(DenyReason::Cancelled));
        }
    }

    #[test]
    fn only_ask_prompts_and_deny_never_reaches_the_client() {
        assert_eq!(needs_round_trip(PermissionCell::Ask), Some(true));
        assert_eq!(needs_round_trip(PermissionCell::Allow), Some(false));
        assert_eq!(
            needs_round_trip(PermissionCell::Deny),
            None,
            "Deny has no ACP representation; it must be enforced by non-advertisement"
        );
    }
}
