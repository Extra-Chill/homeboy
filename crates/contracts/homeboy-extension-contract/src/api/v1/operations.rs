//! Catalog, capability-resolution, readiness, and read-only invocation envelopes.

use serde::{Deserialize, Serialize};

use super::{
    ExtensionApiCapabilityDescriptor, ExtensionApiCompatibility, ExtensionApiDescriptor,
    ExtensionApiVersion,
};

mod action;
mod agent_task_executor;
mod deployment;
mod environment;
mod external_check_detail;
mod invocation;
mod recipe_run;
pub use action::*;
pub use agent_task_executor::*;
pub use deployment::*;
pub use environment::*;
pub use external_check_detail::*;
pub use invocation::*;
pub use recipe_run::*;

pub const EXTENSION_API_CATALOG_REQUEST_SCHEMA: &str = "homeboy/extension-api-catalog-request/v1";
pub const EXTENSION_API_CATALOG_RESPONSE_SCHEMA: &str = "homeboy/extension-api-catalog-response/v1";
pub const EXTENSION_API_RESOLVE_REQUEST_SCHEMA: &str = "homeboy/extension-api-resolve-request/v1";
pub const EXTENSION_API_RESOLVE_RESPONSE_SCHEMA: &str = "homeboy/extension-api-resolve-response/v1";
pub const EXTENSION_API_READINESS_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-readiness-request/v1";
pub const EXTENSION_API_READINESS_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-readiness-response/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiCatalogRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiCatalogEntryStatus {
    Available,
    Incompatible,
    Invalid,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiCatalogDiagnosticCode {
    InvalidManifest,
    BrokenInstallation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiCatalogDiagnostic {
    pub code: ExtensionApiCatalogDiagnosticCode,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiCatalogEntry {
    pub id: String,
    pub status: ExtensionApiCatalogEntryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<ExtensionApiDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<ExtensionApiCompatibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ExtensionApiCatalogDiagnostic>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiOperationFailureCode {
    InvalidRequestSchema,
    UnsupportedApiVersion,
    ExtensionNotFound,
    ExtensionInvalid,
    ExtensionIncompatible,
    CapabilityNotProvided,
    CapabilityExecutionFailed,
    CapabilityOutputInvalid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiOperationFailure {
    pub code: ExtensionApiOperationFailureCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiCatalogResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ExtensionApiCatalogEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiResolveRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub capability_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiResolveResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<ExtensionApiDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<ExtensionApiCapabilityDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<ExtensionApiCompatibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiReadinessMode {
    Cached,
    Probe,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiReadinessState {
    Ready,
    NotReady,
    Unknown,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiReadinessStatus {
    pub state: ExtensionApiReadinessState,
    pub ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_age_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiReadinessRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub mode: ExtensionApiReadinessMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiReadinessResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExtensionApiReadinessStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}
