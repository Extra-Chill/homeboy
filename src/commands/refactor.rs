mod across_targets;
mod add;
mod audit;
mod move;
mod propagate_add_missing;
mod refactor;
mod refactor_target_args;
mod single;
mod targets;
mod transform;
mod types;

pub use across_targets::*;
pub use add::*;
pub use audit::*;
pub use move::*;
pub use propagate_add_missing::*;
pub use refactor::*;
pub use refactor_target_args::*;
pub use single::*;
pub use targets::*;
pub use transform::*;
pub use types::*;

use clap::{Args, Subcommand};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use serde::Serialize;
use std::collections::HashSet;

use super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use crate::commands::CmdResult;

impl RefactorTargetArgs {
    fn resolve_targets(&self) -> homeboy::Result<Vec<RefactorTarget>> {
        let component_ids = collect_component_ids(&self.component_ids, &self.components);
        if self.path.is_some() && !component_ids.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "component",
                "--path cannot be combined with multiple component IDs",
                None,
                Some(vec![
                    "Use --path for one target only".to_string(),
                    "Use --component/--components for multi-component refactors".to_string(),
                ]),
            ));
        }

        if let Some(path) = &self.path {
            return Ok(vec![RefactorTarget {
                component_id: None,
                path: Some(path.clone()),
                label: path.clone(),
            }]);
        }

        if component_ids.is_empty() {
            return Err(homeboy::Error::validation_missing_argument(vec![
                "component".to_string(),
            ]));
        }

        Ok(component_ids
            .into_iter()
            .map(|id| RefactorTarget {
                label: id.clone(),
                component_id: Some(id),
                path: None,
            })
            .collect())
    }
}

// ============================================================================
// Propagate (add missing fields to struct instantiations)
// ============================================================================

// ============================================================================
// Transform
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_component_ids_dedupes_and_trims() {
        let ids = collect_component_ids(
            &["alpha".to_string(), " beta ".to_string()],
            &["beta".to_string(), "gamma".to_string(), "".to_string()],
        );

        assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn target_args_reject_path_with_multiple_components() {
        let args = RefactorTargetArgs {
            component_ids: vec!["alpha".to_string(), "beta".to_string()],
            components: vec![],
            path: Some("/tmp/example".to_string()),
        };

        let error = args.resolve_targets().unwrap_err();
        assert!(
            error.to_string().contains("--path cannot be combined"),
            "unexpected error: {}",
            error
        );
    }

    #[test]
    fn target_args_build_multi_component_targets() {
        let args = RefactorTargetArgs {
            component_ids: vec!["alpha".to_string()],
            components: vec!["beta".to_string(), "alpha".to_string()],
            path: None,
        };

        let targets = args.resolve_targets().unwrap();
        let labels: Vec<_> = targets.into_iter().map(|target| target.label).collect();
        assert_eq!(labels, vec!["alpha", "beta"]);
    }
}
