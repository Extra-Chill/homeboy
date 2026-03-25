//! types — extracted from mod.rs.

use serde::Serialize;
use crate::engine::command::CapturedOutput;
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use std::path::PathBuf;
use crate::component::{self, Component};
use crate::error::{Error, Result};
use crate::extension::{self, exec_context, ExtensionCapability, ExtensionExecutionContext};


#[derive(Debug, Clone, Serialize)]
pub struct BuildOutput {
    pub command: String,
    pub component_id: String,
    pub build_command: String,
    #[serde(flatten)]
    pub output: CapturedOutput,
    pub success: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum BuildResult {
    Single(BuildOutput),
    Bulk(BulkResult<BuildOutput>),
}
