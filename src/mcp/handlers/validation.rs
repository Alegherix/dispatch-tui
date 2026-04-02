use serde_json::Value;

use crate::models::{SubStatus, TaskStatus, TaskTag};

use super::types::JsonRpcResponse;

/// Parse a status string or return a JSON-RPC error response.
pub(super) fn parse_status_or_error(
    s: &str,
    id: &Option<Value>,
) -> Result<TaskStatus, JsonRpcResponse> {
    TaskStatus::parse(s).ok_or_else(|| {
        JsonRpcResponse::err(
            id.clone(),
            -32602,
            format!(
                "Unknown status: {s}. Valid values: {}",
                TaskStatus::ALL
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })
}

/// Parse a tag string or return a JSON-RPC error response.
pub(super) fn parse_tag_or_error(
    s: &str,
    id: &Option<Value>,
) -> Result<TaskTag, JsonRpcResponse> {
    TaskTag::parse(s).ok_or_else(|| {
        JsonRpcResponse::err(
            id.clone(),
            -32602,
            format!("Invalid tag: {s}. Valid values: bug, feature, chore, epic"),
        )
    })
}

/// Parse a sub_status string or return a JSON-RPC error response.
pub(super) fn parse_substatus_or_error(
    s: &str,
    id: &Option<Value>,
) -> Result<SubStatus, JsonRpcResponse> {
    SubStatus::parse(s).ok_or_else(|| {
        JsonRpcResponse::err(
            id.clone(),
            -32602,
            format!(
                "Invalid sub_status: {s}. Valid values: {}",
                SubStatus::ALL
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })
}

/// Check that at least one field is present. Returns Ok(()) if any field is Some,
/// or a JSON-RPC error listing the expected field names.
pub(super) fn require_some_update(
    fields: &[(&str, bool)],
    id: &Option<Value>,
) -> Result<(), JsonRpcResponse> {
    if fields.iter().any(|(_, present)| *present) {
        Ok(())
    } else {
        let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
        Err(JsonRpcResponse::err(
            id.clone(),
            -32602,
            format!(
                "At least one of {} must be provided",
                names.join(", ")
            ),
        ))
    }
}
