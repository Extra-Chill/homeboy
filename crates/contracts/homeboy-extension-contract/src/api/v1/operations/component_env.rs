use serde::{Deserialize, Serialize};

use super::super::ExtensionApiRuntimeRequirement;
use super::{
    ExtensionApiInvocationProcessEvidence, ExtensionApiOperationFailure, ExtensionApiVersion,
};

pub const COMPONENT_ENV_CAPABILITY_ID: &str = "component-env";
pub const EXTENSION_API_COMPONENT_ENV_DETECT_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-component-env-detect-request/v1";
pub const EXTENSION_API_COMPONENT_ENV_DETECT_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-component-env-detect-response/v1";

/// Select one installed component-env detector without serializing script paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiComponentEnvDetectRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiComponentEnvDetectResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    /// Runtime requirements emitted by the detector. Empty when the detector
    /// produced no output or the extension does not provide the capability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detected_runtimes: Vec<ExtensionApiRuntimeRequirement>,
    /// Manifest-declared runtime defaults from the public descriptor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_runtimes: Vec<ExtensionApiRuntimeRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
    /// Captured process evidence when detector execution reached a child process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ExtensionApiInvocationProcessEvidence>,
}
