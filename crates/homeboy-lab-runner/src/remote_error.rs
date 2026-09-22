use homeboy_core::error::{Error, ErrorCode, Hint};
use serde_json::{json, Value};

/// Rehydrate a daemon/broker error without collapsing its typed fields into a
/// formatted transport message. Some older proxies put the error response in
/// a JSON string, so decode that representation before inspecting its fields.
pub(crate) fn from_wire(
    value: Value,
    context: &str,
    status_code: Option<u16>,
    path: &str,
) -> Error {
    let value = match value {
        Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
        value => value,
    };
    let object = value.as_object();
    let code = object
        .and_then(|object| object.get("error"))
        .and_then(Value::as_str)
        .and_then(ErrorCode::from_str)
        .unwrap_or(ErrorCode::InternalUnexpected);
    let message = object
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(context)
        .to_string();
    let details = object
        .and_then(|object| object.get("details"))
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "remote_error": value,
                "http_status": status_code,
                "path": path,
            })
        });
    let hints = object
        .and_then(|object| object.get("hints"))
        .and_then(Value::as_array)
        .map(|hints| {
            hints
                .iter()
                .filter_map(|hint| {
                    hint.as_str()
                        .or_else(|| hint.get("message").and_then(Value::as_str))
                        .map(|message| Hint {
                            message: message.to_string(),
                        })
                })
                .collect()
        })
        .unwrap_or_default();
    Error {
        code,
        message,
        details,
        hints,
        retryable: object
            .and_then(|object| object.get("retryable"))
            .and_then(Value::as_bool),
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_error_keeps_nested_json_error_typed() {
        let error = from_wire(
            serde_json::json!({
                "error": "internal.json_error",
                "message": "staging record schema is invalid",
                "details": {"phase": "lab_staging_submission"},
                "hints": [{"message": "repair the staging record"}]
            }),
            "daemon request failed",
            Some(500),
            "/runner/staging/submit",
        );

        assert_eq!(error.code, ErrorCode::InternalJsonError);
        assert_eq!(error.message, "staging record schema is invalid");
        assert_eq!(error.details["phase"], "lab_staging_submission");
        assert_eq!(error.hints[0].message, "repair the staging record");
    }

    #[test]
    fn daemon_error_decodes_json_string_without_matching_message_text() {
        let error = from_wire(
            serde_json::Value::String(
                r#"{"error":"internal.json_error","message":"wrapped"}"#.to_string(),
            ),
            "daemon request failed",
            None,
            "/runner/staging/submit",
        );

        assert_eq!(error.code, ErrorCode::InternalJsonError);
        assert_eq!(error.message, "wrapped");
    }
}
