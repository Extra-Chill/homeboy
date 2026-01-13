use crate::Error;
use homeboy_error::{ErrorCode, Hint};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CliResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<CliWarning>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CliError>,
}

#[derive(Debug, Serialize)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<Hint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CliWarning {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<Hint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl<T: Serialize> CliResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            warnings: None,
            error: None,
        }
    }

    pub fn success_with_warnings(data: T, warnings: Vec<CliWarning>) -> Self {
        Self {
            success: true,
            data: Some(data),
            warnings: if warnings.is_empty() {
                None
            } else {
                Some(warnings)
            },
            error: None,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| {
            r#"{"success":false,"error":{"code":"internal.json_error","message":"Failed to serialize response","details":{}}}"#
                .to_string()
        })
    }
}

impl CliResponse<()> {
    pub fn from_error(err: &Error) -> Self {
        Self {
            success: false,
            data: None,
            warnings: None,
            error: Some(CliError {
                code: err.code.as_str().to_string(),
                message: err.message.clone(),
                details: err.details.clone(),
                hints: if err.hints.is_empty() {
                    None
                } else {
                    Some(err.hints.clone())
                },
                retryable: err.retryable,
            }),
        }
    }
}

pub fn print_success<T: Serialize>(data: T) {
    println!("{}", CliResponse::success(data).to_json());
}

pub fn print_success_with_warnings<T: Serialize>(data: T, warnings: Vec<CliWarning>) {
    println!(
        "{}",
        CliResponse::success_with_warnings(data, warnings).to_json()
    );
}

pub fn print_result<T: Serialize>(result: crate::Result<T>) {
    match result {
        Ok(data) => print_success(data),
        Err(err) => println!("{}", CliResponse::<()>::from_error(&err).to_json()),
    }
}

#[derive(Debug, Serialize)]
pub struct CmdSuccess {
    pub payload: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<CliWarning>,
}

pub type CmdResult = crate::Result<(serde_json::Value, Vec<CliWarning>, i32)>;

pub fn map_cmd_result_to_json<T: Serialize>(
    result: crate::Result<(T, Vec<CliWarning>, i32)>,
) -> (crate::Result<CmdSuccess>, i32) {
    match result {
        Ok((data, warnings, exit_code)) => match serde_json::to_value(data) {
            Ok(value) => (
                Ok(CmdSuccess {
                    payload: value,
                    warnings,
                }),
                exit_code,
            ),
            Err(err) => (
                Err(crate::Error::internal_json(
                    err.to_string(),
                    Some("serialize response".to_string()),
                )),
                1,
            ),
        },
        Err(err) => {
            let exit_code = exit_code_for_error(err.code);
            (Err(err), exit_code)
        }
    }
}

fn exit_code_for_error(code: ErrorCode) -> i32 {
    match code {
        ErrorCode::ConfigMissingKey
        | ErrorCode::ConfigInvalidJson
        | ErrorCode::ConfigInvalidValue
        | ErrorCode::ValidationMissingArgument
        | ErrorCode::ValidationInvalidArgument
        | ErrorCode::ValidationInvalidJson
        | ErrorCode::ValidationUnknownErrorCode => 2,

        ErrorCode::ProjectNotFound
        | ErrorCode::ServerNotFound
        | ErrorCode::ComponentNotFound
        | ErrorCode::ModuleNotFound
        | ErrorCode::ProjectNoActive => 4,

        ErrorCode::SshServerInvalid
        | ErrorCode::SshIdentityFileNotFound
        | ErrorCode::SshAuthFailed
        | ErrorCode::SshConnectFailed => 10,

        ErrorCode::RemoteCommandFailed
        | ErrorCode::RemoteCommandTimeout
        | ErrorCode::DeployNoComponentsConfigured
        | ErrorCode::DeployBuildFailed
        | ErrorCode::DeployUploadFailed
        | ErrorCode::GitCommandFailed => 20,

        ErrorCode::InternalIoError
        | ErrorCode::InternalJsonError
        | ErrorCode::InternalUnexpected => 1,
    }
}

pub fn print_json_result(result: crate::Result<CmdSuccess>) {
    match result {
        Ok(success) => print_success_with_warnings(success.payload, success.warnings),
        Err(err) => println!("{}", CliResponse::<()>::from_error(&err).to_json()),
    }
}
