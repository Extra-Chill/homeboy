use serde::{Deserialize, Serialize};

use super::{
    ExtensionApiInvocationProcessEvidence, ExtensionApiOperationFailure, ExtensionApiVersion,
};

pub const ENVIRONMENT_CAPABILITY_ID: &str = "environment";
pub const EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-environment-resolve-request/v1";
pub const EXTENSION_API_ENVIRONMENT_RESOLVE_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-environment-resolve-response/v1";

/// Select one installed environment provider without serializing runtime values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiEnvironmentResolveRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
}

/// The non-secret environment contribution retained as provider provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiEnvironmentContribution {
    pub extension_id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiEnvironmentResolveResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contribution: Option<ExtensionApiEnvironmentContribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
    /// Captured process evidence when capability execution reached a child process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ExtensionApiInvocationProcessEvidence>,
}
