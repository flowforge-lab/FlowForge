//! JSON-RPC 2.0 envelope types shared by every ACP method.
//!
//! ACP transports JSON-RPC 2.0 messages. Each message carries a `"jsonrpc": "2.0"`
//! member, a `method`, and method-specific `params`. The request/response payload
//! structs live in [`crate::client`] and [`crate::agent`]; the envelope here wraps
//! them on the wire.
//!
//! The envelope keeps a single source of truth for the wire shape while the
//! direction-specific modules stay focused on method payloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The JSON-RPC version string required on every message.
pub const JSONRPC_VERSION: &str = "2.0";

/// The protocol-level notification methods (those starting with `$/`) are
/// implementation-dependent and may be ignored by a receiver.
pub const CANCEL_REQUEST_METHOD: &str = "$/cancel_request";

/// A JSON-RPC request id. Correlates a request with its response. Either an
/// integer, a string, or `null`; fractional numbers are prohibited by the spec
/// but tolerated on input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric id.
    Number(i64),
    /// String id.
    Str(String),
    /// `null` id — valid for responses whose request id is unknown.
    Null,
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A JSON-RPC error code (ACP schema `ErrorCode`). The named variants carry the
/// standard integer codes; `Other` catches any out-of-spec code so we never
/// hard-fail on a value a peer adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// -32700 Parse error.
    ParseError,
    /// -32600 Invalid request.
    InvalidRequest,
    /// -32601 Method not found.
    MethodNotFound,
    /// -32602 Invalid params.
    InvalidParams,
    /// -32603 Internal error.
    InternalError,
    /// -32800 Request cancelled.
    Cancelled,
    /// -32000 Authentication required.
    AuthenticationRequired,
    /// -32002 Resource not found.
    ResourceNotFound,
    /// Any other int32 code.
    Other(i32),
}

impl ErrorCode {
    /// The integer code this variant maps to on the wire.
    pub const fn code(self) -> i32 {
        match self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::InternalError => -32603,
            ErrorCode::Cancelled => -32800,
            ErrorCode::AuthenticationRequired => -32000,
            ErrorCode::ResourceNotFound => -32002,
            ErrorCode::Other(code) => code,
        }
    }

    /// The named variant for a wire code, or [`ErrorCode::Other`] when unknown.
    pub const fn from_code(code: i32) -> Self {
        match code {
            -32700 => ErrorCode::ParseError,
            -32600 => ErrorCode::InvalidRequest,
            -32601 => ErrorCode::MethodNotFound,
            -32602 => ErrorCode::InvalidParams,
            -32603 => ErrorCode::InternalError,
            -32800 => ErrorCode::Cancelled,
            -32000 => ErrorCode::AuthenticationRequired,
            -32002 => ErrorCode::ResourceNotFound,
            other => ErrorCode::Other(other),
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(self.code())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(ErrorCode::from_code(i32::deserialize(d)?))
    }
}

/// A JSON-RPC message envelope. The generic parameters pick the concrete
/// `params` and `result` payload types for a direction; the transport decides
/// which variant a message is by which fields are present.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcMessage<P, R> {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<R>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Key-value metadata attached to any wire object. `_meta` is reserved by ACP
/// for extensions; implementations MUST NOT interpret values at these keys.
pub type Meta = BTreeMap<String, serde_json::Value>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_round_trip() {
        for (code, expected) in [
            (ErrorCode::ParseError, -32700),
            (ErrorCode::InvalidRequest, -32600),
            (ErrorCode::MethodNotFound, -32601),
            (ErrorCode::InvalidParams, -32602),
            (ErrorCode::InternalError, -32603),
            (ErrorCode::Cancelled, -32800),
            (ErrorCode::AuthenticationRequired, -32000),
            (ErrorCode::ResourceNotFound, -32002),
        ] {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: ErrorCode = serde_json::from_value(json).unwrap();
            assert_eq!(back, code, "mismatch for code {expected}");
        }
    }

    #[test]
    fn test_unknown_error_code_round_trip() {
        let code = ErrorCode::Other(-999);
        let json = serde_json::to_value(code).unwrap();
        assert_eq!(json, serde_json::json!(-999));
        let back: ErrorCode = serde_json::from_value(json).unwrap();
        assert_eq!(back, ErrorCode::Other(-999));
    }

    #[test]
    fn test_rpc_error_round_trip() {
        let err = RpcError {
            code: ErrorCode::MethodNotFound,
            message: "method not found".into(),
            data: None,
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], serde_json::json!(-32601));
        assert_eq!(json["message"], "method not found");

        let back: RpcError = serde_json::from_value(json).unwrap();
        assert_eq!(back.code, ErrorCode::MethodNotFound);
    }

    #[test]
    fn test_rpc_error_with_data() {
        let err = RpcError {
            code: ErrorCode::InternalError,
            message: "oops".into(),
            data: Some(serde_json::json!({"detail": "stack overflow"})),
        };
        let json = serde_json::to_value(&err).unwrap();
        let back: RpcError = serde_json::from_value(json).unwrap();
        assert_eq!(back.data.unwrap()["detail"], "stack overflow");
    }

    #[test]
    fn test_request_id_number() {
        let rid = RequestId::Number(42);
        let json = serde_json::to_value(&rid).unwrap();
        assert_eq!(json, 42);
        let back: RequestId = serde_json::from_value(json).unwrap();
        assert_eq!(back, RequestId::Number(42));
    }

    #[test]
    fn test_request_id_string() {
        let rid = RequestId::Str("abc".into());
        let json = serde_json::to_value(&rid).unwrap();
        assert_eq!(json, "abc");
        let back: RequestId = serde_json::from_value(json).unwrap();
        assert_eq!(back, RequestId::Str("abc".into()));
    }

    #[test]
    fn test_request_id_null() {
        let rid = RequestId::Null;
        let json = serde_json::to_value(&rid).unwrap();
        assert!(json.is_null());
        let back: RequestId = serde_json::from_value(json).unwrap();
        assert_eq!(back, RequestId::Null);
    }

    #[test]
    fn test_json_rpc_message_request_serialize() {
        let msg = JsonRpcMessage::<serde_json::Value, serde_json::Value> {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(RequestId::Number(1)),
            method: "test/method".into(),
            params: Some(serde_json::json!({"key": "val"})),
            result: None,
            error: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert_eq!(json["method"], "test/method");
        assert_eq!(json["params"]["key"], "val");
        assert!(json.get("result").is_none());
        assert!(json.get("error").is_none());
    }

    #[test]
    fn test_unknown_fields_tolerated_on_rpc_error() {
        let json = serde_json::json!({
            "code": -32603,
            "message": "err",
            "extraField": "should be ignored"
        });
        let err: RpcError = serde_json::from_value(json).unwrap();
        assert_eq!(err.code, ErrorCode::InternalError);
        assert_eq!(err.message, "err");
    }
}
