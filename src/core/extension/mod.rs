mod aggregate_query_functions;
mod capability;
mod extension;
mod extensions_error;
mod find_extension;
mod helpers;
mod refactor_script_protocol;
mod resolve;
mod types;

pub use aggregate_query_functions::*;
pub use capability::*;
pub use extension::*;
pub use extensions_error::*;
pub use find_extension::*;
pub use helpers::*;
pub use refactor_script_protocol::*;
pub use resolve::*;
pub use types::*;

pub mod build;
mod execution;
pub mod grammar;
pub mod grammar_items;
mod lifecycle;
pub mod lint;
mod manifest;
mod runner;
mod runner_contract;
mod runtime_helper;
mod scope;
pub mod test;
pub mod update_check;
pub mod version;

pub mod exec_context;

// Re-export runner types
pub use runner::{ExtensionRunner, RunnerOutput};
pub use runner_contract::RunnerStepFilter;
pub use runtime_helper::RUNNER_STEPS_ENV;

// Re-export manifest types
pub use manifest::{
    ActionConfig, ActionType, AuditCapability, BuildConfig, CliConfig, DatabaseCliConfig,
    DatabaseConfig, DeployCapability, DeployOverride, DeployVerification, DiscoveryConfig,
    DocTarget, ExecutableCapability, ExtensionManifest, FeatureContextRule, HttpMethod,
    InputConfig, LintConfig, OutputConfig, OutputSchema, PlatformCapability, ProvidesConfig,
    RequirementsConfig, RuntimeConfig, ScriptsConfig, SelectOption, SettingConfig, SinceTagConfig,
    TestConfig, TestMappingConfig, VersionPatternConfig,
};

// Re-export version types
pub use version::{parse_extension_version, VersionConstraint};

// Re-export execution types and functions
pub(crate) use execution::execute_action;
pub use execution::{
    extension_ready_status, is_extension_compatible, run_action, run_extension, run_setup,
    ExtensionExecutionMode, ExtensionReadyStatus, ExtensionRunResult, ExtensionSetupResult,
    ExtensionStepFilter,
};

// Re-export scope types
pub use scope::ExtensionScope;

// Re-export lifecycle types and functions
pub use lifecycle::{
    check_update_available, derive_id_from_url, install, is_git_url, read_source_revision,
    slugify_id, uninstall, update, InstallResult, UpdateAvailable, UpdateResult,
};

// Re-export aggregate query types
// (ActionSummary, ExtensionSummary, UpdateAllResult, UpdateEntry defined below in this file)

// Extension loader functions

use crate::component::Component;
use crate::config;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;
use crate::paths;
use std::collections::HashMap;
use std::path::PathBuf;

// ============================================================================
// Refactor Script Protocol
// ============================================================================

// ============================================================================
// Aggregate query functions
// ============================================================================

use serde::Serialize;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, ScopedExtensionConfig};
    use std::collections::HashMap;

    #[test]
    fn validate_required_extensions_passes_with_no_modules() {
        let comp = Component {
            id: "test-component".to_string(),
            ..Default::default()
        };
        assert!(validate_required_extensions(&comp).is_ok());
    }

    #[test]
    fn validate_required_extensions_passes_with_empty_modules() {
        let comp = Component {
            id: "test-component".to_string(),
            extensions: Some(HashMap::new()),
            ..Default::default()
        };
        assert!(validate_required_extensions(&comp).is_ok());
    }

    #[test]
    fn validate_required_extensions_fails_with_missing_module() {
        let mut extensions = HashMap::new();
        extensions.insert(
            "nonexistent-extension-abc123".to_string(),
            ScopedExtensionConfig::default(),
        );
        let comp = Component {
            id: "test-component".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        };
        let err = validate_required_extensions(&comp).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ExtensionNotFound);
        assert!(err.message.contains("nonexistent-extension-abc123"));
        assert!(err.message.contains("test-component"));
        // Should have install hint + browse hint
        assert!(err.hints.len() >= 2);
        assert!(err
            .hints
            .iter()
            .any(|h| h.message.contains("homeboy extension install")));
        assert!(err
            .hints
            .iter()
            .any(|h| h.message.contains("homeboy-extensions")));
    }

    #[test]
    fn validate_required_extensions_reports_all_missing() {
        let mut extensions = HashMap::new();
        extensions.insert(
            "missing-mod-a".to_string(),
            ScopedExtensionConfig::default(),
        );
        extensions.insert(
            "missing-mod-b".to_string(),
            ScopedExtensionConfig::default(),
        );
        let comp = Component {
            id: "multi-dep".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        };
        let err = validate_required_extensions(&comp).unwrap_err();
        // Error should mention both missing extensions
        assert!(err.message.contains("missing-mod-a"));
        assert!(err.message.contains("missing-mod-b"));
        // Should have install hint for each + browse hint
        assert!(err.hints.len() >= 3);
    }

    #[test]
    fn test_should_run() {
        let filter = RunnerStepFilter {
            step: Some("lint,test".to_string()),
            skip: Some("test".to_string()),
        };
        assert!(filter.should_run("lint"));
        assert!(!filter.should_run("test"));
        assert!(!filter.should_run("deploy"));
    }

    #[test]
    fn test_to_env_pairs() {
        let filter = RunnerStepFilter {
            step: Some("a".to_string()),
            skip: Some("b".to_string()),
        };
        let env = filter.to_env_pairs();
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_STEP" && v == "a"));
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_SKIP" && v == "b"));
    }
}
