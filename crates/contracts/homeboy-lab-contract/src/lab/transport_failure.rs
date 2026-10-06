//! Durable, redacted evidence for a Lab transport attempt that failed before
//! provider execution.

use std::error::Error as StdError;
use std::io::ErrorKind;

use homeboy_error::{Error, ErrorCode};
use serde::{Deserialize, Serialize};

pub const LAB_TRANSPORT_ATTEMPT_RECEIPT_SCHEMA: &str = "homeboy/lab-transport-attempt-receipt/v2";
pub const LAB_TRANSPORT_CAUSE_LIMIT: usize = 4;
pub const LAB_TRANSPORT_CAUSE_MESSAGE_LIMIT: usize = 256;
pub const LAB_TRANSPORT_ERROR_MESSAGE_LIMIT: usize = 512;
pub const LAB_TRANSPORT_CONTEXT_LIMIT: usize = 4 * 1024;
pub const LAB_TRANSPORT_RETRY_LIMIT: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabTransportOperation {
    DispatchCookAttempt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabTransportErrorKind {
    Connect,
    Timeout,
    HttpStatus,
    BodyDecode,
    NotFound,
    PermissionDenied,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    NotConnected,
    BrokenPipe,
    WouldBlock,
    TimedOut,
    Interrupted,
    UnexpectedEof,
    InvalidInput,
    InvalidData,
    WriteZero,
    OutOfMemory,
    Other,
}

impl LabTransportErrorKind {
    pub fn from_io_kind(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::NotFound => Self::NotFound,
            ErrorKind::PermissionDenied => Self::PermissionDenied,
            ErrorKind::ConnectionRefused => Self::ConnectionRefused,
            ErrorKind::ConnectionReset => Self::ConnectionReset,
            ErrorKind::ConnectionAborted => Self::ConnectionAborted,
            ErrorKind::NotConnected => Self::NotConnected,
            ErrorKind::BrokenPipe => Self::BrokenPipe,
            ErrorKind::WouldBlock => Self::WouldBlock,
            ErrorKind::TimedOut => Self::TimedOut,
            ErrorKind::Interrupted => Self::Interrupted,
            ErrorKind::UnexpectedEof => Self::UnexpectedEof,
            ErrorKind::InvalidInput => Self::InvalidInput,
            ErrorKind::InvalidData => Self::InvalidData,
            ErrorKind::WriteZero => Self::WriteZero,
            ErrorKind::OutOfMemory => Self::OutOfMemory,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabJobAcceptanceDisposition {
    NoJobAccepted,
    AcceptedIdentityLost,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabTransportCause {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<LabTransportErrorKind>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabTransportFailure {
    pub code: String,
    pub kind: LabTransportErrorKind,
    pub message: String,
    /// Operation/path context is bounded independently from the primary cause
    /// so a long remote path cannot crowd the failure reason out of `message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub causes: Vec<LabTransportCause>,
    pub causes_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabTransportAttemptReceipt {
    pub schema: String,
    pub receipt_id: String,
    pub phase: String,
    pub operation: LabTransportOperation,
    pub transport_attempt: u8,
    pub transport_retry_limit: u8,
    pub selected_runner: String,
    pub outcome: String,
    pub retryable: bool,
    pub acceptance: LabJobAcceptanceDisposition,
    pub error: LabTransportFailure,
    pub provider_executions_consumed: u8,
}

impl LabTransportAttemptReceipt {
    pub fn from_error(
        run_id: &str,
        selected_runner: &str,
        operation: LabTransportOperation,
        acceptance: LabJobAcceptanceDisposition,
        error: &Error,
    ) -> Self {
        let (causes, causes_truncated) = bounded_cause_chain(error);
        let kind = typed_error_kind(error);
        let retryable = error.retryable == Some(true)
            && acceptance == LabJobAcceptanceDisposition::NoJobAccepted;
        let context = transport_operation_context(error);
        let original_error = error
            .details
            .get("diagnostic_cause")
            .or_else(|| error.details.get("error"))
            .or_else(|| error.details.get("stderr"))
            .or_else(|| error.details.pointer("/source/cause"))
            .and_then(serde_json::Value::as_str);
        let message = original_error.unwrap_or(&error.message);
        let run_id = bounded_redacted(run_id, LAB_TRANSPORT_CAUSE_MESSAGE_LIMIT);
        Self {
            schema: LAB_TRANSPORT_ATTEMPT_RECEIPT_SCHEMA.to_string(),
            receipt_id: format!("{run_id}:dispatch_cook_attempt"),
            phase: "lab_handoff_preacceptance".to_string(),
            operation,
            transport_attempt: u8::from(run_id.ends_with("-transport-retry")),
            transport_retry_limit: LAB_TRANSPORT_RETRY_LIMIT,
            selected_runner: bounded_redacted(selected_runner, LAB_TRANSPORT_CAUSE_MESSAGE_LIMIT),
            outcome: "failed".to_string(),
            retryable,
            acceptance,
            error: LabTransportFailure {
                code: ErrorCode::RunnerLabTransportFailure.as_str().to_string(),
                kind,
                message: bounded_redacted(message, LAB_TRANSPORT_ERROR_MESSAGE_LIMIT),
                context: context
                    .as_deref()
                    .map(|context| bounded_redacted(context, LAB_TRANSPORT_CONTEXT_LIMIT)),
                causes,
                causes_truncated,
            },
            provider_executions_consumed: 0,
        }
    }
}

fn transport_operation_context(error: &Error) -> Option<String> {
    if let Some(context) = error
        .details
        .get("context")
        .and_then(serde_json::Value::as_str)
    {
        return Some(context.to_string());
    }
    let operation = error
        .details
        .get("operation")
        .and_then(serde_json::Value::as_str)?;
    let runner = error
        .details
        .get("runner_id")
        .and_then(serde_json::Value::as_str);
    let path = error
        .details
        .get("remote_path")
        .and_then(serde_json::Value::as_str);
    let transport = error
        .details
        .get("transport")
        .and_then(serde_json::Value::as_str);
    let mut context = vec![operation.to_string()];
    context.extend(runner.map(|value| format!("runner={value}")));
    context.extend(transport.map(|value| format!("transport={value}")));
    context.extend(path.map(|value| format!("path={value}")));
    Some(context.join("; "))
}

pub fn preacceptance_transport_error(
    run_id: &str,
    selected_runner: &str,
    operation: LabTransportOperation,
    acceptance: LabJobAcceptanceDisposition,
    error: Error,
) -> Error {
    let receipt = LabTransportAttemptReceipt::from_error(
        run_id,
        selected_runner,
        operation,
        acceptance,
        &error,
    );
    let retryable = receipt.retryable;
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        receipt.error.message.clone(),
        serde_json::json!({ "lab_transport_attempt_receipt": receipt }),
    )
    .with_retryable(retryable)
    .with_source(error)
}

fn typed_error_kind(error: &Error) -> LabTransportErrorKind {
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            return LabTransportErrorKind::from_io_kind(io_error.kind());
        }
        source = cause.source();
    }
    match error
        .details
        .pointer("/daemon_transport_error/kind")
        .and_then(serde_json::Value::as_str)
    {
        Some("connect") => LabTransportErrorKind::Connect,
        Some("timeout") => LabTransportErrorKind::Timeout,
        Some("status") => LabTransportErrorKind::HttpStatus,
        Some("body_decode") => LabTransportErrorKind::BodyDecode,
        _ => LabTransportErrorKind::Other,
    }
}

fn bounded_cause_chain(error: &Error) -> (Vec<LabTransportCause>, bool) {
    let mut causes = vec![LabTransportCause {
        code: Some(error.code.as_str().to_string()),
        kind: None,
        message: bounded_redacted(&error.message, LAB_TRANSPORT_CAUSE_MESSAGE_LIMIT),
    }];
    // HTTP/file-transfer adapters retain remote rejection details under
    // `details.source`; they are structured response facts, not necessarily
    // `std::error::Error::source()` nodes. Lift a bounded textual cause into the
    // durable chain before wrapping discards that detail object.
    if let Some(message) = error
        .details
        .pointer("/source/cause")
        .and_then(serde_json::Value::as_str)
    {
        if causes.len() == LAB_TRANSPORT_CAUSE_LIMIT {
            return (causes, true);
        }
        causes.push(LabTransportCause {
            code: None,
            kind: None,
            message: bounded_redacted(message, LAB_TRANSPORT_CAUSE_MESSAGE_LIMIT),
        });
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if causes.len() == LAB_TRANSPORT_CAUSE_LIMIT {
            return (causes, true);
        }
        let homeboy_error = cause.downcast_ref::<Error>();
        let io_error = cause.downcast_ref::<std::io::Error>();
        causes.push(LabTransportCause {
            code: homeboy_error.map(|error| error.code.as_str().to_string()),
            kind: io_error.map(|error| LabTransportErrorKind::from_io_kind(error.kind())),
            message: bounded_redacted(&cause.to_string(), LAB_TRANSPORT_CAUSE_MESSAGE_LIMIT),
        });
        source = cause.source();
    }
    (causes, false)
}

fn bounded_redacted(value: &str, limit: usize) -> String {
    homeboy_redaction::redact_string(value)
        .chars()
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preacceptance_receipt_preserves_typed_bounded_redacted_io_evidence() {
        let io_error = std::io::Error::new(
            ErrorKind::BrokenPipe,
            "Authorization: Bearer fixture-secret",
        );
        let error = Error::internal_io(
            io_error.to_string(),
            Some("submit the selected runner job".to_string()),
        )
        .with_source(io_error)
        .with_retryable(true);
        let wrapped = preacceptance_transport_error(
            "cook-attempt-1-transport-retry",
            "homeboy-lab",
            LabTransportOperation::DispatchCookAttempt,
            LabJobAcceptanceDisposition::NoJobAccepted,
            error,
        );
        let receipt: LabTransportAttemptReceipt =
            serde_json::from_value(wrapped.details["lab_transport_attempt_receipt"].clone())
                .expect("typed receipt");

        assert_eq!(receipt.error.kind, LabTransportErrorKind::BrokenPipe);
        assert_eq!(receipt.transport_attempt, 1);
        assert_eq!(receipt.transport_retry_limit, 1);
        assert_eq!(
            receipt.acceptance,
            LabJobAcceptanceDisposition::NoJobAccepted
        );
        assert_eq!(receipt.selected_runner, "homeboy-lab");
        assert!(receipt.causes_are_bounded());
        assert!(!serde_json::to_string(&wrapped.details)
            .expect("serialize details")
            .contains("fixture-secret"));
    }

    #[test]
    fn v1_receipt_decodes_without_the_v2_independent_context_field() {
        let wrapped = preacceptance_transport_error(
            "cook-attempt-legacy-receipt",
            "homeboy-lab",
            LabTransportOperation::DispatchCookAttempt,
            LabJobAcceptanceDisposition::NoJobAccepted,
            Error::internal_io(
                "No such file or directory (os error 2)",
                Some("open provider evidence /tmp/missing.json".to_string()),
            ),
        );
        let mut legacy = wrapped.details["lab_transport_attempt_receipt"].clone();
        legacy["schema"] = serde_json::json!("homeboy/lab-transport-attempt-receipt/v1");
        legacy["error"]
            .as_object_mut()
            .expect("failure object")
            .remove("context");

        let receipt: LabTransportAttemptReceipt =
            serde_json::from_value(legacy).expect("deserialize legacy receipt");
        assert_eq!(receipt.schema, "homeboy/lab-transport-attempt-receipt/v1");
        assert_eq!(receipt.error.context, None);
        assert!(receipt.error.message.contains("No such file"));
    }

    impl LabTransportAttemptReceipt {
        fn causes_are_bounded(&self) -> bool {
            self.error.causes.len() <= LAB_TRANSPORT_CAUSE_LIMIT
                && self
                    .error
                    .causes
                    .iter()
                    .all(|cause| cause.message.chars().count() <= LAB_TRANSPORT_CAUSE_MESSAGE_LIMIT)
        }
    }

    #[test]
    fn acceptance_dispositions_remain_distinct() {
        assert_ne!(
            serde_json::to_value(LabJobAcceptanceDisposition::NoJobAccepted)
                .expect("serialize disposition"),
            serde_json::to_value(LabJobAcceptanceDisposition::AcceptedIdentityLost)
                .expect("serialize disposition")
        );

        let wrapped = preacceptance_transport_error(
            "cook-attempt-accepted",
            "homeboy-lab",
            LabTransportOperation::DispatchCookAttempt,
            LabJobAcceptanceDisposition::AcceptedIdentityLost,
            Error::internal_unexpected("accepted response lost").with_retryable(true),
        );
        let receipt: LabTransportAttemptReceipt =
            serde_json::from_value(wrapped.details["lab_transport_attempt_receipt"].clone())
                .expect("typed receipt");
        assert!(!receipt.retryable);
        assert_eq!(wrapped.retryable, Some(false));
    }

    #[test]
    fn cause_chain_is_truncated_at_the_contract_limit() {
        let io_error = std::io::Error::new(
            ErrorKind::BrokenPipe,
            "Authorization: Bearer deeply-nested-secret",
        );
        let mut error = Error::internal_io(io_error.to_string(), None).with_source(io_error);
        for depth in 0..LAB_TRANSPORT_CAUSE_LIMIT + 2 {
            error =
                Error::internal_unexpected(format!("transport wrapper {depth}")).with_source(error);
        }
        error.retryable = Some(true);

        let wrapped = preacceptance_transport_error(
            "cook-attempt-bounded-causes",
            "homeboy-lab",
            LabTransportOperation::DispatchCookAttempt,
            LabJobAcceptanceDisposition::NoJobAccepted,
            error,
        );
        let receipt: LabTransportAttemptReceipt =
            serde_json::from_value(wrapped.details["lab_transport_attempt_receipt"].clone())
                .expect("typed receipt");

        assert_eq!(receipt.error.causes.len(), LAB_TRANSPORT_CAUSE_LIMIT);
        assert!(receipt.error.causes_truncated);
        assert_eq!(receipt.error.kind, LabTransportErrorKind::BrokenPipe);
        assert!(!serde_json::to_string(&receipt)
            .expect("serialize receipt")
            .contains("deeply-nested-secret"));
    }
}
