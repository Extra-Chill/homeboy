use serde::{Deserialize, Serialize};

use super::{ExtensionApiOperationFailure, ExtensionApiVersion};

pub const EXTENSION_API_INVOKE_REQUEST_SCHEMA: &str = "homeboy/extension-api-invoke-request/v1";
pub const EXTENSION_API_INVOKE_RESPONSE_SCHEMA: &str = "homeboy/extension-api-invoke-response/v1";
pub const COMPILER_WARNINGS_CAPABILITY_ID: &str = "compiler-warnings";
pub const COMPILER_WARNING_FIXES_CAPABILITY_ID: &str = "compiler-warning-fixes";
pub const COMPILER_WARNINGS_INPUT_SCHEMA: &str = "homeboy/compiler-warnings-input/v1";
pub const COMPILER_WARNINGS_OUTPUT_SCHEMA: &str = "homeboy/compiler-warnings-output/v1";
pub const COMPILER_WARNING_FIXES_INPUT_SCHEMA: &str = "homeboy/compiler-warning-fixes-input/v1";
pub const COMPILER_WARNING_FIXES_OUTPUT_SCHEMA: &str = "homeboy/compiler-warning-fixes-output/v1";

/// Execute one explicitly selected non-mutating capability on the serving host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiInvokeRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub capability_id: String,
    pub working_directory: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiInvokeResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}
