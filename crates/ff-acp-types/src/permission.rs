//! Permission types — the `session/request_permission` round-trip.

use serde::{Deserialize, Serialize};

use crate::rpc::Meta;
use crate::session::SessionId;
use crate::tool::ToolCallUpdate;

/// A unique identifier for a permission option.
pub type PermissionOptionId = String;

/// An option presented to the user when requesting permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: PermissionOptionId,
    /// Human-readable label.
    pub name: String,
    /// Hint about the nature of this option (affects UI rendering).
    pub kind: PermissionOptionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The type of permission option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// Request payload for `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: SessionId,
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response payload for `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
    pub outcome: RequestPermissionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The user's decision. Tagged by `outcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum RequestPermissionOutcome {
    /// The request was cancelled by the client.
    Cancelled,
    /// The user selected one of the options.
    Selected(SelectedPermissionOutcome),
}

/// The user selected a specific permission option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedPermissionOutcome {
    pub option_id: PermissionOptionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_option_round_trip() {
        let opt = PermissionOption {
            option_id: "opt_1".into(),
            name: "Allow once".into(),
            kind: PermissionOptionKind::AllowOnce,
            _meta: None,
        };
        let json = serde_json::to_value(&opt).unwrap();
        assert_eq!(json["optionId"], "opt_1");
        assert_eq!(json["name"], "Allow once");
        assert_eq!(json["kind"], "allow_once");

        let back: PermissionOption = serde_json::from_value(json).unwrap();
        assert_eq!(back, opt);
    }

    #[test]
    fn test_permission_option_kind_round_trip() {
        for kind in [
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::AllowAlways,
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ] {
            let json = serde_json::to_value(kind).unwrap();
            let back: PermissionOptionKind = serde_json::from_value(json).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn test_permission_option_kind_serialization() {
        assert_eq!(
            serde_json::to_value(PermissionOptionKind::AllowAlways).unwrap(),
            "allow_always"
        );
        assert_eq!(
            serde_json::to_value(PermissionOptionKind::RejectAlways).unwrap(),
            "reject_always"
        );
    }

    #[test]
    fn test_request_permission_outcome_cancelled() {
        let outcome = RequestPermissionOutcome::Cancelled;
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["outcome"], "cancelled");
        let back: RequestPermissionOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, RequestPermissionOutcome::Cancelled);
    }

    #[test]
    fn test_request_permission_outcome_selected() {
        let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome {
            option_id: "opt_1".into(),
            _meta: None,
        });
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["outcome"], "selected");
        assert_eq!(json["optionId"], "opt_1");
        let back: RequestPermissionOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, outcome);
    }

    #[test]
    fn test_request_permission_request_round_trip() {
        let req = RequestPermissionRequest {
            session_id: "sess_1".into(),
            tool_call: crate::tool::ToolCallUpdate {
                tool_call_id: "tc_1".into(),
                kind: None,
                status: None,
                title: None,
                content: None,
                locations: None,
                raw_input: None,
                raw_output: None,
                _meta: None,
            },
            options: vec![PermissionOption {
                option_id: "opt_1".into(),
                name: "Allow".into(),
                kind: PermissionOptionKind::AllowOnce,
                _meta: None,
            }],
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["toolCall"]["toolCallId"], "tc_1");
        assert_eq!(json["options"][0]["optionId"], "opt_1");

        let back: RequestPermissionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_unknown_fields_tolerated_on_permission_option() {
        let json = serde_json::json!({
            "optionId": "opt_1",
            "name": "Allow",
            "kind": "allow_once",
            "extra": "ignored"
        });
        let opt: PermissionOption = serde_json::from_value(json).unwrap();
        assert_eq!(opt.option_id, "opt_1");
    }
}
