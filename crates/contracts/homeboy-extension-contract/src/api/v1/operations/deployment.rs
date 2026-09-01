use serde::{Deserialize, Serialize};

use super::{ExtensionApiOperationFailure, ExtensionApiVersion};

pub const DEPLOYMENT_PROVIDER_CAPABILITY_PREFIX: &str = "deployment-provider.";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-inventory-request/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-inventory-response/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-resolve-request/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-resolve-response/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-invoke-request/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-invoke-response/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderInventoryRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiDeploymentProviderValidation {
    Valid,
    Invalid,
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderDescriptor {
    pub id: String,
    pub owning_extension: String,
    pub supports_dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<String>,
    #[serde(default)]
    pub target_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<String>,
    pub resolvable: bool,
    pub validation: ExtensionApiDeploymentProviderValidation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderInventoryResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ExtensionApiDeploymentProviderDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderResolveRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub provider_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiDeploymentProviderDiagnosticKind {
    Unknown,
    Ambiguous,
    Invalid,
    NotReady,
    DryRunUnsupported,
    InvalidInput,
    ExecutionFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderDiagnostic {
    pub extension_id: String,
    pub provider_id: String,
    pub kind: ExtensionApiDeploymentProviderDiagnosticKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderResolveResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ExtensionApiDeploymentProviderDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ExtensionApiDeploymentProviderDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderInvokeRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub provider_id: String,
    pub project_id: String,
    pub component_id: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderResult {
    pub exit_code: i32,
    pub evidence: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderInvokeResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExtensionApiDeploymentProviderResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ExtensionApiDeploymentProviderDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_request_excludes_private_execution_paths() {
        let request = ExtensionApiDeploymentProviderInvokeRequest {
            schema: EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_REQUEST_SCHEMA.to_string(),
            api_version: crate::api::v1::EXTENSION_API_V1,
            extension_id: "fixture-extension".to_string(),
            provider_id: "fixture.deploy".to_string(),
            project_id: "site".to_string(),
            component_id: "fixture".to_string(),
            dry_run: true,
        };

        assert_eq!(
            serde_json::to_value(request).expect("request JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "extension_id": "fixture-extension",
                "provider_id": "fixture.deploy",
                "project_id": "site",
                "component_id": "fixture",
                "dry_run": true
            })
        );
    }
}
