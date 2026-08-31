//! Version 1 Extension API discovery and compatibility contracts.
//!
//! This module contains serialized values only. Catalog projection and version
//! negotiation behavior belong to the application service that serves them.

use serde::{Deserialize, Serialize};

mod operations;

pub use operations::*;

pub const EXTENSION_API_DESCRIPTOR_SCHEMA: &str = "homeboy/extension-api-descriptor/v1";
pub const EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-handshake-request/v1";
pub const EXTENSION_API_HANDSHAKE_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-handshake-response/v1";
pub const EXTENSION_API_V1: ExtensionApiVersion = ExtensionApiVersion { major: 1 };
pub const FINGERPRINT_FILE_CAPABILITY_PREFIX: &str = "fingerprint.";
pub const FORMAT_FILE_CAPABILITY_PREFIX: &str = "format.";
pub const REFACTOR_FILE_CAPABILITY_PREFIX: &str = "refactor.";

/// A transport-neutral Extension API major version.
///
/// The numeric shape lets an older client retain and reject a future version
/// explicitly instead of failing to deserialize an unknown enum variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExtensionApiVersion {
    pub major: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiIdentity {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

/// A named serialized contract consumed or produced by a capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiSchemaReference {
    pub schema: String,
}

/// One extension capability projected independently of its implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiCapabilityDescriptor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_schema: Option<ExtensionApiSchemaReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<ExtensionApiSchemaReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<ExtensionApiSchemaReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_schemas: Vec<ExtensionApiSchemaReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiReadinessDescriptor {
    pub runtime_probe: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolchain_probe_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRuntimeRequirement {
    pub id: String,
    pub version: String,
}

/// Runner-neutral requirements declared by an extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiExecutionRequirements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<ExtensionApiRuntimeRequirement>,
}

/// The stable catalog projection of one installed extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiDescriptor {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub identity: ExtensionApiIdentity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<ExtensionApiCapabilityDescriptor>,
    pub readiness: ExtensionApiReadinessDescriptor,
    pub execution_requirements: ExtensionApiExecutionRequirements,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_homeboy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiHandshakeRequest {
    pub schema: String,
    pub supported_versions: Vec<ExtensionApiVersion>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiCompatibilityStatus {
    Compatible,
    Incompatible,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiCompatibilityFailureCode {
    InvalidHandshakeSchema,
    NoSharedApiVersion,
    InvalidHomeboyVersionConstraint,
    HomeboyVersionIncompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiCompatibilityFailure {
    pub code: ExtensionApiCompatibilityFailureCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiCompatibility {
    pub status: ExtensionApiCompatibilityStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<ExtensionApiCompatibilityFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiHandshakeResponse {
    pub schema: String,
    pub supported_versions: Vec<ExtensionApiVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_version: Option<ExtensionApiVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<ExtensionApiDescriptor>,
    pub compatibility: ExtensionApiCompatibility,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn future_api_versions_remain_parseable() {
        let version: ExtensionApiVersion =
            serde_json::from_value(serde_json::json!({ "major": 99 })).expect("api version");
        assert_eq!(version.major, 99);
    }

    #[test]
    fn descriptor_wire_shape_is_explicitly_versioned() {
        let descriptor = ExtensionApiDescriptor {
            schema: EXTENSION_API_DESCRIPTOR_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            identity: ExtensionApiIdentity {
                id: "fixture".to_string(),
                name: "Fixture".to_string(),
                version: "1.2.3".to_string(),
                source_revision: Some("abc123".to_string()),
            },
            capabilities: vec![ExtensionApiCapabilityDescriptor {
                id: "test".to_string(),
                contract_version: Some("1".to_string()),
                configuration_schema: None,
                input_schema: None,
                output_schema: Some(ExtensionApiSchemaReference {
                    schema: "homeboy/test-results/v1".to_string(),
                }),
                artifact_schemas: Vec::new(),
            }],
            readiness: ExtensionApiReadinessDescriptor {
                runtime_probe: true,
                toolchain_probe_ids: vec!["runtime".to_string()],
            },
            execution_requirements: ExtensionApiExecutionRequirements {
                runtimes: vec![ExtensionApiRuntimeRequirement {
                    id: "php".to_string(),
                    version: ">=8.0".to_string(),
                }],
            },
            requires_homeboy: Some(">=0.1.0".to_string()),
        };

        assert_eq!(
            serde_json::to_value(descriptor).expect("descriptor JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_DESCRIPTOR_SCHEMA,
                "api_version": { "major": 1 },
                "identity": {
                    "id": "fixture",
                    "name": "Fixture",
                    "version": "1.2.3",
                    "source_revision": "abc123"
                },
                "capabilities": [{
                    "id": "test",
                    "contract_version": "1",
                    "output_schema": { "schema": "homeboy/test-results/v1" }
                }],
                "readiness": {
                    "runtime_probe": true,
                    "toolchain_probe_ids": ["runtime"]
                },
                "execution_requirements": {
                    "runtimes": [{ "id": "php", "version": ">=8.0" }]
                },
                "requires_homeboy": ">=0.1.0"
            })
        );
    }

    #[test]
    fn resolve_failure_wire_shape_is_typed() {
        let response = ExtensionApiResolveResponse {
            schema: EXTENSION_API_RESOLVE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            descriptor: None,
            capability: None,
            compatibility: None,
            failure: Some(ExtensionApiOperationFailure {
                code: ExtensionApiOperationFailureCode::CapabilityNotProvided,
                message: "Extension 'fixture' does not provide 'deploy'".to_string(),
            }),
        };

        assert_eq!(
            serde_json::to_value(response).expect("resolve response JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_RESOLVE_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "failure": {
                    "code": "capability_not_provided",
                    "message": "Extension 'fixture' does not provide 'deploy'"
                }
            })
        );
    }

    #[test]
    fn readiness_status_wire_shape_preserves_probe_evidence() {
        let response = ExtensionApiReadinessResponse {
            schema: EXTENSION_API_READINESS_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: "fixture".to_string(),
            status: Some(ExtensionApiReadinessStatus {
                state: ExtensionApiReadinessState::TimedOut,
                ready: None,
                reason: Some("ready_check_timeout".to_string()),
                detail: None,
                cache_age_seconds: Some(0),
                probe_duration_ms: Some(30_000),
                timeout_ms: Some(30_000),
                follow_up_command: Some(
                    "homeboy extension show fixture --live-readiness".to_string(),
                ),
            }),
            failure: None,
        };

        assert_eq!(
            serde_json::to_value(response).expect("readiness response JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_READINESS_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "extension_id": "fixture",
                "status": {
                    "state": "timed_out",
                    "ready": null,
                    "reason": "ready_check_timeout",
                    "cache_age_seconds": 0,
                    "probe_duration_ms": 30_000,
                    "timeout_ms": 30_000,
                    "follow_up_command": "homeboy extension show fixture --live-readiness"
                }
            })
        );
    }

    #[test]
    fn read_only_invoke_wire_shape_is_typed() {
        let response = ExtensionApiInvokeResponse {
            schema: EXTENSION_API_INVOKE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            output: Some(serde_json::json!({ "warnings": [] })),
            failure: None,
            process: None,
        };

        assert_eq!(
            serde_json::to_value(response).expect("invoke response JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_INVOKE_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "output": { "warnings": [] }
            })
        );
    }

    #[test]
    fn environment_resolve_wire_shape_excludes_private_execution_inputs() {
        let request = ExtensionApiEnvironmentResolveRequest {
            schema: EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: "fixture".to_string(),
        };
        let response = ExtensionApiEnvironmentResolveResponse {
            schema: EXTENSION_API_ENVIRONMENT_RESOLVE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            contribution: Some(ExtensionApiEnvironmentContribution {
                extension_id: "fixture".to_string(),
                version: "1.2.3".to_string(),
                public_env: vec![("PUBLIC_PATH".to_string(), "/opt/tool/bin".to_string())],
                secret_env_names: vec!["FIXTURE_TOKEN".to_string()],
            }),
            failure: None,
            process: None,
        };

        assert_eq!(
            serde_json::to_value(request).expect("environment request JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "extension_id": "fixture"
            })
        );
        assert_eq!(
            serde_json::to_value(response).expect("environment response JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_ENVIRONMENT_RESOLVE_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "contribution": {
                    "extension_id": "fixture",
                    "version": "1.2.3",
                    "public_env": [["PUBLIC_PATH", "/opt/tool/bin"]],
                    "secret_env_names": ["FIXTURE_TOKEN"]
                }
            })
        );
    }

    #[test]
    fn recipe_run_plan_wire_shape_exposes_only_safe_identity_and_literal_argv() {
        let response = ExtensionApiRecipeRunPlanResponse {
            schema: EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            plan: Some(ExtensionApiRecipeRunPlan {
                provider_id: "fixture.recipe-run".to_string(),
                provider_version: "1.2.3".to_string(),
                owning_extension: "fixture".to_string(),
                command: vec![
                    "fixture-run".to_string(),
                    "--recipe".to_string(),
                    "recipe.json".to_string(),
                ],
            }),
            available_provider_ids: Vec::new(),
            selection_failure: None,
            failure: None,
        };

        assert_eq!(
            serde_json::to_value(response).expect("recipe run plan JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "plan": {
                    "provider_id": "fixture.recipe-run",
                    "provider_version": "1.2.3",
                    "owning_extension": "fixture",
                    "command": ["fixture-run", "--recipe", "recipe.json"]
                }
            })
        );
    }
}
