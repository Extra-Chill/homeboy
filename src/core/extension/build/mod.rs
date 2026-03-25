mod internal_implementation;
mod public_api;
mod resolved_build_command;
mod types;

pub use internal_implementation::*;
pub use public_api::*;
pub use resolved_build_command::*;
pub use types::*;

use serde::Serialize;
use std::path::PathBuf;

use crate::component::{self, Component};
use crate::config::{is_json_input, parse_bulk_ids};
use crate::deploy::permissions;
use crate::engine::command::CapturedOutput;
use crate::engine::shell;
use crate::error::{Error, Result};
use crate::extension::{self, exec_context, ExtensionCapability, ExtensionExecutionContext};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::paths;
use crate::server::execute_local_command_in_dir;

mod artifact;

pub use artifact::{resolve_artifact_path, resolve_artifact_path_from_root};

// === Build Command Resolution ===

impl ResolvedBuildCommand {
    pub fn command(&self) -> &str {
        match self {
            ResolvedBuildCommand::ExtensionProvided { command, .. } => command,
            ResolvedBuildCommand::LocalScript { command, .. } => command,
        }
    }
}

// === Public API ===

// === Internal implementation ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_json_input_detects_json() {
        assert!(is_json_input(r#"{"componentIds": ["a"]}"#));
        assert!(is_json_input(r#"  {"componentIds": ["a"]}"#));
        assert!(!is_json_input("extrachill-api"));
        assert!(!is_json_input("some-component-id"));
    }
}
