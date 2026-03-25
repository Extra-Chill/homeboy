//! resolved_build_command — extracted from mod.rs.

use crate::extension::{self, exec_context, ExtensionCapability, ExtensionExecutionContext};
use serde::Serialize;
use std::path::PathBuf;
use crate::component::{self, Component};
use crate::engine::command::CapturedOutput;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};


#[derive(Debug, Clone)]
pub enum ResolvedBuildCommand {
    ExtensionProvided {
        context: ExtensionExecutionContext,
        command: String,
        source: String,
    },
    LocalScript {
        command: String,
        script_name: String,
    },
}
