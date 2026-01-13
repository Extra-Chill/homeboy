use super::{codes, ErrorCode, Hint};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorHelpSummary {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorHelp {
    pub code: String,
    pub summary: String,
    pub details_schema: serde_json::Value,
    pub hints: Vec<Hint>,
}

pub fn list() -> Vec<ErrorHelpSummary> {
    codes::all_codes()
        .iter()
        .copied()
        .map(|code| {
            let help = explain(code);
            ErrorHelpSummary {
                code: help.code,
                summary: help.summary,
            }
        })
        .collect()
}

pub fn explain(code: ErrorCode) -> ErrorHelp {
    match code {
        ErrorCode::ConfigMissingKey => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Missing required configuration key".to_string(),
            details_schema: serde_json::json!({"key":"string","path":"string?"}),
            hints: vec![Hint {
                message: "Check your project/server/component JSON files for missing required keys".to_string(),
            }],
        },
        ErrorCode::ConfigInvalidJson => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Configuration JSON is invalid".to_string(),
            details_schema: serde_json::json!({"path":"string","error":"string"}),
            hints: vec![Hint {
                message: "Fix JSON syntax in the referenced file".to_string(),
            }],
        },
        ErrorCode::ConfigInvalidValue => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Configuration value is invalid".to_string(),
            details_schema: serde_json::json!({"key":"string","value":"string?","problem":"string"}),
            hints: vec![Hint {
                message: "Correct the config value to match expected type/format".to_string(),
            }],
        },
        ErrorCode::ValidationMissingArgument => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Missing required CLI argument".to_string(),
            details_schema: serde_json::json!({"args":"string[]"}),
            hints: vec![Hint {
                message: "Rerun the command with the required argument(s)".to_string(),
            }],
        },
        ErrorCode::ValidationInvalidArgument => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Invalid CLI argument".to_string(),
            details_schema: serde_json::json!({"field":"string","problem":"string","id":"string?","tried":"string[]?"}),
            hints: vec![Hint {
                message: "Verify the argument value and try again".to_string(),
            }],
        },
        ErrorCode::ValidationInvalidJson => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Invalid JSON input".to_string(),
            details_schema: serde_json::json!({"error":"string","context":"string?"}),
            hints: vec![Hint {
                message: "Validate the JSON you passed to the command".to_string(),
            }],
        },
        ErrorCode::ValidationUnknownErrorCode => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Unknown error code".to_string(),
            details_schema: serde_json::json!({"code":"string"}),
            hints: vec![Hint {
                message: "Run `homeboy error codes` to list available codes".to_string(),
            }],
        },
        ErrorCode::ProjectNotFound => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Project ID not found".to_string(),
            details_schema: serde_json::json!({"id":"string"}),
            hints: vec![Hint {
                message: "Run `homeboy project list` and verify the project ID".to_string(),
            }],
        },
        ErrorCode::ProjectNoActive => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "No active project is set".to_string(),
            details_schema: serde_json::json!({"configPath":"string?"}),
            hints: vec![Hint {
                message: "Run `homeboy project switch <projectId>`".to_string(),
            }],
        },
        ErrorCode::ServerNotFound => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Server ID not found".to_string(),
            details_schema: serde_json::json!({"id":"string"}),
            hints: vec![Hint {
                message: "Run `homeboy server list` and verify the server ID".to_string(),
            }],
        },
        ErrorCode::ComponentNotFound => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Component ID not found".to_string(),
            details_schema: serde_json::json!({"id":"string"}),
            hints: vec![Hint {
                message: "Verify the component ID in your project config".to_string(),
            }],
        },
        ErrorCode::ModuleNotFound => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Module ID not found".to_string(),
            details_schema: serde_json::json!({"id":"string"}),
            hints: vec![Hint {
                message: "Run `homeboy module list` and verify the module ID".to_string(),
            }],
        },
        ErrorCode::SshServerInvalid => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Server configuration is invalid".to_string(),
            details_schema: serde_json::json!({"serverId":"string","missingFields":"string[]"}),
            hints: vec![Hint {
                message: "Ensure the server config includes host/user/port and optional identityFile".to_string(),
            }],
        },
        ErrorCode::SshIdentityFileNotFound => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "SSH identity file not found".to_string(),
            details_schema: serde_json::json!({"serverId":"string","identityFile":"string"}),
            hints: vec![Hint {
                message: "Check the identityFile path in the server config".to_string(),
            }],
        },
        ErrorCode::SshAuthFailed => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "SSH authentication failed".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Verify SSH key setup and that the server accepts the key".to_string(),
            }],
        },
        ErrorCode::SshConnectFailed => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "SSH connection failed".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Verify host/port/network connectivity".to_string(),
            }],
        },
        ErrorCode::RemoteCommandFailed => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Remote command returned non-zero".to_string(),
            details_schema: serde_json::json!({"command":"string","exitCode":"number","stdout":"string","stderr":"string","target":"object"}),
            hints: vec![Hint {
                message: "Inspect stdout/stderr in error.details for the underlying failure".to_string(),
            }],
        },
        ErrorCode::RemoteCommandTimeout => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Remote command timed out".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Retry or increase remote timeout if supported".to_string(),
            }],
        },
        ErrorCode::DeployNoComponentsConfigured => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "No components configured for deploy".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Add component IDs to the project configuration".to_string(),
            }],
        },
        ErrorCode::DeployBuildFailed => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Build failed during deploy".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Inspect build logs and fix build issues".to_string(),
            }],
        },
        ErrorCode::DeployUploadFailed => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Upload failed during deploy".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Check SCP/SSH connectivity and remote paths".to_string(),
            }],
        },
        ErrorCode::GitCommandFailed => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Git command failed".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Inspect stdout/stderr for git failure details".to_string(),
            }],
        },
        ErrorCode::InternalIoError => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Internal IO error".to_string(),
            details_schema: serde_json::json!({"error":"string","context":"string?"}),
            hints: vec![Hint {
                message: "Report as a Homeboy bug if persistent".to_string(),
            }],
        },
        ErrorCode::InternalJsonError => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Internal JSON error".to_string(),
            details_schema: serde_json::json!({"error":"string","context":"string?"}),
            hints: vec![Hint {
                message: "Report as a Homeboy bug if persistent".to_string(),
            }],
        },
        ErrorCode::InternalUnexpected => ErrorHelp {
            code: code.as_str().to_string(),
            summary: "Unexpected internal error".to_string(),
            details_schema: serde_json::json!({}),
            hints: vec![Hint {
                message: "Report as a Homeboy bug with steps to reproduce".to_string(),
            }],
        },
    }
}
