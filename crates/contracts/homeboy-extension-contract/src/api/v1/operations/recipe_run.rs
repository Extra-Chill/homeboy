use serde::{Deserialize, Serialize};

use super::{ExtensionApiOperationFailure, ExtensionApiVersion};

pub const RECIPE_RUN_PROVIDER_CAPABILITY_PREFIX: &str = "recipe-run-provider.";
pub const EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-recipe-run-provider-inventory-request/v1";
pub const EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-recipe-run-provider-inventory-response/v1";
pub const EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-recipe-run-plan-request/v1";
pub const EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-recipe-run-plan-response/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRecipeRunProviderInventoryRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiRecipeRunProviderValidation {
    Valid,
    Invalid,
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRecipeRunProviderInventoryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub owning_extension: String,
    pub resolvable: bool,
    pub validation: ExtensionApiRecipeRunProviderValidation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRecipeRunProviderInventoryResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ExtensionApiRecipeRunProviderInventoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRecipeRunPlanRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub provider_id: String,
    pub recipe_path: String,
    pub artifact_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRecipeRunPlan {
    pub provider_id: String,
    pub provider_version: String,
    pub owning_extension: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiRecipeRunSelectionFailureCode {
    NotFound,
    Invalid,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRecipeRunSelectionFailure {
    pub code: ExtensionApiRecipeRunSelectionFailureCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiRecipeRunPlanResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ExtensionApiRecipeRunPlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_provider_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_failure: Option<ExtensionApiRecipeRunSelectionFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}
