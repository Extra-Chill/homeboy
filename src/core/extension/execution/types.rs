//! types — extracted from execution.rs.

use crate::engine::command::CapturedOutput;
use crate::project::{self, Project};
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use serde::Serialize;
use std::collections::HashMap;
use super::super::runner_contract::RunnerStepFilter;
use crate::component::{self, Component};
use crate::error::{Error, Result};
use crate::server::http::ApiClient;
use std::path::Path;
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::super::scope::ExtensionScope;


/// Result of executing a extension.
pub struct ExtensionRunResult {
    pub exit_code: i32,
    pub project_id: Option<String>,
    pub output: Option<CapturedOutput>,
}

pub struct ExtensionExecutionResult {
    pub output: CapturedOutput,
    pub exit_code: i32,
    pub success: bool,
}

pub struct ExtensionExecutionOutcome {
    pub project_id: Option<String>,
    pub result: ExtensionExecutionResult,
}

pub enum ExtensionExecutionMode {
    Interactive,
    Captured,
}

/// Result of running extension setup.
pub struct ExtensionSetupResult {
    pub exit_code: i32,
}

pub(crate) struct ExtensionExecutionContext {
    extension_id: String,
    project_id: Option<String>,
    component_id: Option<String>,
    project: Option<Project>,
    settings: HashMap<String, serde_json::Value>,
}

/// Backward-compatible alias for existing command API usage.
pub type ExtensionStepFilter = RunnerStepFilter;

pub struct PreparedCapabilityRun {
    pub execution: super::ExtensionExecutionContext,
    pub settings_json: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionReadyStatus {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
