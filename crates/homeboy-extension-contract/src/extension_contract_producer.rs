//! Extension contract-producer and materialization-source contract types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const EXTENSION_MATERIALIZATION_SOURCE_SCHEMA: &str =
    "homeboy/extension-materialization-source/v1";

pub const EXTENSION_CONTRACT_PRODUCER_SCHEMA: &str = "homeboy/extension-contract-producer/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionMaterializationSourceKind {
    Git,
    Archive,
    LocalPath,
    Generated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionMaterializationHelperManifestRef {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionMaterializationSourceContract {
    #[serde(default = "default_extension_materialization_source_schema")]
    pub schema: String,
    pub source_kind: ExtensionMaterializationSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_archive_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helper_manifest_refs: Vec<ExtensionMaterializationHelperManifestRef>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_extension_materialization_source_schema() -> String {
    EXTENSION_MATERIALIZATION_SOURCE_SCHEMA.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionContractProducerPhase {
    Discovery,
    Planning,
    Handoff,
    Result,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionContractProducerOutputKind {
    Capability,
    ExecutionPlan,
    MaterializationPlan,
    SecretPlan,
    RunnerEnvelopeAddition,
    Artifact,
    Status,
    Evidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionContractProducerOutput {
    pub kind: ExtensionContractProducerOutputKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionContractProducerInvocation {
    pub script: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionContractProducer {
    #[serde(default = "default_extension_contract_producer_schema")]
    pub schema: String,
    pub id: String,
    pub phase: ExtensionContractProducerPhase,
    pub invocation: ExtensionContractProducerInvocation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub produces: Vec<ExtensionContractProducerOutput>,
}

fn default_extension_contract_producer_schema() -> String {
    EXTENSION_CONTRACT_PRODUCER_SCHEMA.to_string()
}
