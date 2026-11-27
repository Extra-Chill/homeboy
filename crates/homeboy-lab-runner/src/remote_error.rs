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
    fn daemon_error_keeps_typed_code_message_details_and_hints() {
        let details = json!({
            "runner_job_absence": "ledger_terminal",
            "runner_job_status": "cancelled",
        });
        let error = from_wire(
            json!({
                "error": "validation.invalid_argument",
                "message": "job not found",
                "details": details,
                "hints": [{"message": "repair the record"}, "retry later"]
            }),
            "daemon job read failed",
            Some(404),
            "/jobs/missing",
        );

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.message, "job not found");
        assert_eq!(error.details, details);
        let hints: Vec<_> = error
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect();
        assert_eq!(hints, ["repair the record", "retry later"]);

        let bare = from_wire(json!({}), "not found", Some(404), "/jobs/missing");
        assert_eq!(bare.code, ErrorCode::InternalUnexpected);
        assert_eq!(bare.message, "not found");
        assert!(bare.details.get("runner_job_absence").is_none());
        assert_eq!(bare.details["path"], "/jobs/missing");
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
