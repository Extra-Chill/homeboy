//! types — extracted from mod.rs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::core::error::Error;


#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct Hint {
    pub message: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigMissingKeyDetails {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct ConfigInvalidJsonDetails {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigInvalidValueDetails {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub problem: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigIdCollisionDetails {
    pub id: String,
    pub requested_type: String,
    pub existing_type: String,
}

#[derive(Debug, Serialize)]

pub struct NoActiveProjectDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Serialize)]

pub struct NotFoundDetails {
    pub id: String,
}

#[derive(Debug, Serialize)]

pub struct MissingArgumentDetails {
    pub args: Vec<String>,
}

#[derive(Debug, Serialize)]

pub struct InvalidArgumentDetails {
    pub field: String,
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tried: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationErrorItem {
    pub field: String,
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct MultipleValidationErrorsDetails {
    pub errors: Vec<ValidationErrorItem>,
}

#[derive(Debug, Serialize)]

pub struct InternalIoErrorDetails {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct InternalJsonErrorDetails {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct TargetDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct RemoteCommandFailedDetails {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub target: TargetDetails,
}

#[derive(Debug, Serialize)]

pub struct SshServerInvalidDetails {
    pub server_id: String,
    pub missing_fields: Vec<String>,
}

#[derive(Debug, Serialize)]

pub struct SshIdentityFileNotFoundDetails {
    pub server_id: String,
    pub identity_file: String,
}
