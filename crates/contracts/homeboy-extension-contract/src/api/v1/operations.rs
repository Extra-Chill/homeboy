//! Catalog and explicit capability-resolution operation envelopes.

use serde::{Deserialize, Serialize};

use super::{
    ExtensionApiCapabilityDescriptor, ExtensionApiCompatibility, ExtensionApiDescriptor,
    ExtensionApiVersion,
};

pub const EXTENSION_API_CATALOG_REQUEST_SCHEMA: &str = "homeboy/extension-api-catalog-request/v1";
pub const EXTENSION_API_CATALOG_RESPONSE_SCHEMA: &str = "homeboy/extension-api-catalog-response/v1";
pub const EXTENSION_API_RESOLVE_REQUEST_SCHEMA: &str = "homeboy/extension-api-resolve-request/v1";
pub const EXTENSION_API_RESOLVE_RESPONSE_SCHEMA: &str = "homeboy/extension-api-resolve-response/v1";

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
