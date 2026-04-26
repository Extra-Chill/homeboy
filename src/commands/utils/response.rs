//! CLI response formatting and output.
//!
//! Provides JSON envelope, printing, and exit code mapping.

use homeboy::error::Hint;
use homeboy::{Error, ErrorCode, Result};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CliResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
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

impl<T: Serialize> CliResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| {
            Error::internal_json(e.to_string(), Some("serialize response".to_string()))
        })
    }
}

impl CliResponse<()> {
    pub fn from_error(err: &Error) -> Self {
        Self {
            success: false,
            data: None,
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

fn print_response<T: Serialize>(response: &CliResponse<T>) -> Result<()> {
    use std::io::{self, Write};

    let payload = response.to_json()?;
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    if let Err(e) = writeln!(handle, "{}", payload) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return Ok(()); // Exit gracefully on SIGPIPE
        }
        return Err(Error::internal_io(
            e.to_string(),
            Some("write stdout".to_string()),
        ));
    }
    Ok(())
}

pub fn print_success<T: Serialize>(data: T) -> Result<()> {
    print_response(&CliResponse::success(data))
}

pub fn print_result<T: Serialize>(result: Result<T>) -> Result<()> {
    match result {
        Ok(data) => print_success(data),
        Err(err) => print_response(&CliResponse::<()>::from_error(&err)),
    }
}

pub fn map_cmd_result_to_json<T: Serialize>(
    result: Result<(T, i32)>,
) -> (Result<serde_json::Value>, i32) {
    match result {
        Ok((data, exit_code)) => match serde_json::to_value(data) {
            Ok(value) => (Ok(value), exit_code),
            Err(err) => (
                Err(Error::internal_json(
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
        | ErrorCode::ConfigIdCollision
        | ErrorCode::ValidationMissingArgument
        | ErrorCode::ValidationInvalidArgument
        | ErrorCode::ValidationInvalidJson
        | ErrorCode::ValidationMultipleErrors => 2,

        ErrorCode::ProjectNotFound
        | ErrorCode::ServerNotFound
        | ErrorCode::ComponentNotFound
        | ErrorCode::ComponentNotAttached
        | ErrorCode::FleetNotFound
        | ErrorCode::ExtensionNotFound
        | ErrorCode::DocsTopicNotFound
        | ErrorCode::RigNotFound
        | ErrorCode::StackNotFound
        | ErrorCode::ProjectNoActive => 4,

        ErrorCode::RigPipelineFailed
        | ErrorCode::RigServiceFailed
        | ErrorCode::StackApplyConflict => 20,

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

pub fn print_json_result(result: Result<serde_json::Value>, exit_code: i32) -> Result<()> {
    match result {
        Ok(data) if exit_code == 0 => print_success(data),
        Ok(data) => {
            // Command returned data but with a non-zero exit code (e.g., release
            // succeeded but deploy failed). The envelope should reflect the failure.
            print_response(&CliResponse {
                success: false,
                data: Some(data),
                error: None,
            })
        }
        Err(err) => print_response(&CliResponse::<()>::from_error(&err)),
    }
}

/// Write the JSON output envelope to a file. Best-effort — failures are
/// logged to stderr but don't affect the command's exit code.
pub fn write_json_to_file(result: &Result<serde_json::Value>, path: &str, exit_code: i32) {
    let response = match result {
        Ok(data) => CliResponse {
            success: exit_code == 0,
            data: Some(data.clone()),
            error: None,
        },
        Err(err) => {
            let error_response = CliResponse::<()>::from_error(err);
            CliResponse {
                success: false,
                data: None,
                error: error_response.error,
            }
        }
    };

    let json = match serde_json::to_string_pretty(&response) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Warning: failed to serialize JSON for --output: {}", e);
            return;
        }
    };

    if let Err(e) = std::fs::write(path, json) {
        eprintln!("Warning: failed to write --output file '{}': {}", path, e);
    }
}
