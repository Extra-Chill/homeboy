use serde::{Deserialize, Serialize};

use super::{ExtensionApiOperationFailure, ExtensionApiVersion};
use crate::ExternalCheckDetailResponse;

pub const EXTERNAL_CHECK_DETAIL_RESOLVER_CAPABILITY_PREFIX: &str =
    "external-check-detail-resolver.";
pub const EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-external-check-detail-inventory-request/v1";
pub const EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-external-check-detail-inventory-response/v1";
pub const EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-external-check-detail-hydrate-request/v1";
pub const EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-external-check-detail-hydrate-response/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiExternalCheckDetailInventoryRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiExternalCheckDetailProviderValidation {
    Valid,
    Invalid,
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiExternalCheckDetailInventoryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub owning_extension: String,
    pub resolvable: bool,
    pub validation: ExtensionApiExternalCheckDetailProviderValidation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiExternalCheckDetailInventoryResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ExtensionApiExternalCheckDetailInventoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiExternalCheckDetailHydrateRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub provider: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiExternalCheckDetailDiagnosticKind {
    Unknown,
    Ambiguous,
    Malformed,
    MalformedIdentity,
    Unavailable,
    BudgetExhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiExternalCheckDetailDiagnostic {
    pub provider: String,
    pub kind: ExtensionApiExternalCheckDetailDiagnosticKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiExternalCheckDetailHydrateResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ExternalCheckDetailResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ExtensionApiExternalCheckDetailDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}
