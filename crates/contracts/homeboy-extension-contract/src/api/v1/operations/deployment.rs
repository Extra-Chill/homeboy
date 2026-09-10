use serde::{Deserialize, Serialize};

use super::{ExtensionApiOperationFailure, ExtensionApiVersion};
use homeboy_control_plane_contract::EffectId;

pub const DEPLOYMENT_PROVIDER_CAPABILITY_PREFIX: &str = "deployment-provider.";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-inventory-request/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-inventory-response/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-resolve-request/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-resolve-response/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-submit-request/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-submit-response/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-status-request/v1";
pub const EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-deployment-provider-status-response/v1";

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
    Conflict,
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
pub struct ExtensionApiDeploymentProviderSubmitRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub provider_id: String,
    pub effect_id: EffectId,
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
pub enum ExtensionApiDeploymentProviderEffectState {
    NotStarted,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderSubmitResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub effect_id: EffectId,
    pub state: ExtensionApiDeploymentProviderEffectState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExtensionApiDeploymentProviderResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ExtensionApiDeploymentProviderDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderStatusRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub effect_id: EffectId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDeploymentProviderStatusResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub effect_id: EffectId,
    pub state: ExtensionApiDeploymentProviderEffectState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExtensionApiDeploymentProviderResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_request_excludes_private_execution_paths() {
        let request = ExtensionApiDeploymentProviderSubmitRequest {
            schema: EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA.to_string(),
            api_version: crate::api::v1::EXTENSION_API_V1,
            extension_id: "fixture-extension".to_string(),
            provider_id: "fixture.deploy".to_string(),
            effect_id: EffectId("fixture:deploy:1".to_string()),
            project_id: "site".to_string(),
            component_id: "fixture".to_string(),
            dry_run: true,
        };

        assert_eq!(
            serde_json::to_value(request).expect("request JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "extension_id": "fixture-extension",
                "provider_id": "fixture.deploy",
                "effect_id": "fixture:deploy:1",
                "project_id": "site",
                "component_id": "fixture",
                "dry_run": true
            })
        );
    }
}
