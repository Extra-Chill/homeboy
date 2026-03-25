mod extension_manifest;
mod sub_structs_unchanged;
mod types;

pub use extension_manifest::*;
pub use sub_structs_unchanged::*;
pub use types::*;

use crate::config::ConfigEntity;
use crate::error::{Error, Result};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ============================================================================
// Capability Groups
// ============================================================================

// ============================================================================
// ExtensionManifest
// ============================================================================

impl ExtensionManifest {
    pub fn has_cli(&self) -> bool {
        self.cli.is_some()
    }

    pub fn has_build(&self) -> bool {
        self.build.is_some()
    }

    pub fn has_lint(&self) -> bool {
        self.lint
            .as_ref()
            .and_then(|c| c.extension_script.as_ref())
            .is_some()
    }

    pub fn has_test(&self) -> bool {
        self.test
            .as_ref()
            .and_then(|c| c.extension_script.as_ref())
            .is_some()
    }

    pub fn lint_script(&self) -> Option<&str> {
        self.lint
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn test_script(&self) -> Option<&str> {
        self.test
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn build_script(&self) -> Option<&str> {
        self.build
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    /// Convenience: get deploy verifications (empty if no deploy capability).
    pub fn deploy_verifications(&self) -> &[DeployVerification] {
        self.deploy
            .as_ref()
            .map(|d| d.verifications.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get deploy overrides (empty if no deploy capability).
    pub fn deploy_overrides(&self) -> &[DeployOverride] {
        self.deploy
            .as_ref()
            .map(|d| d.overrides.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get version patterns (empty if no deploy capability).
    pub fn version_patterns(&self) -> &[VersionPatternConfig] {
        self.deploy
            .as_ref()
            .map(|d| d.version_patterns.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get since_tag config.
    pub fn since_tag(&self) -> Option<&SinceTagConfig> {
        self.deploy.as_ref().and_then(|d| d.since_tag.as_ref())
    }

    /// Convenience: get runtime config.
    pub fn runtime(&self) -> Option<&RuntimeConfig> {
        self.executable.as_ref().map(|e| &e.runtime)
    }

    /// Convenience: get inputs (empty if no executable capability).
    pub fn inputs(&self) -> &[InputConfig] {
        self.executable
            .as_ref()
            .map(|e| e.inputs.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get audit reference setup script path (relative to extension dir).
    pub fn audit_setup_references(&self) -> Option<&str> {
        self.audit
            .as_ref()
            .and_then(|a| a.setup_references.as_deref())
    }

    /// Convenience: get audit ignore claim patterns (empty if no audit capability).
    pub fn audit_ignore_claim_patterns(&self) -> &[String] {
        self.audit
            .as_ref()
            .map(|a| a.ignore_claim_patterns.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get audit feature patterns (empty if no audit capability).
    pub fn audit_feature_patterns(&self) -> &[String] {
        self.audit
            .as_ref()
            .map(|a| a.feature_patterns.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get feature labels map (empty if no audit capability).
    pub fn audit_feature_labels(&self) -> &HashMap<String, String> {
        static EMPTY: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        self.audit
            .as_ref()
            .map(|a| &a.feature_labels)
            .unwrap_or(&EMPTY)
    }

    /// Convenience: get doc targets map (empty if no audit capability).
    pub fn audit_doc_targets(&self) -> &HashMap<String, DocTarget> {
        static EMPTY: std::sync::LazyLock<HashMap<String, DocTarget>> =
            std::sync::LazyLock::new(HashMap::new);
        self.audit
            .as_ref()
            .map(|a| &a.doc_targets)
            .unwrap_or(&EMPTY)
    }

    /// Convenience: get feature context rules (empty if no audit capability).
    pub fn audit_feature_context(&self) -> &HashMap<String, FeatureContextRule> {
        static EMPTY: std::sync::LazyLock<HashMap<String, FeatureContextRule>> =
            std::sync::LazyLock::new(HashMap::new);
        self.audit
            .as_ref()
            .map(|a| &a.feature_context)
            .unwrap_or(&EMPTY)
    }

    /// Convenience: get test mapping config from audit capability.
    pub fn test_mapping(&self) -> Option<&TestMappingConfig> {
        self.audit.as_ref().and_then(|a| a.test_mapping.as_ref())
    }

    /// Convenience: get database config from platform capability.
    pub fn database(&self) -> Option<&DatabaseConfig> {
        self.platform.as_ref().and_then(|p| p.database.as_ref())
    }

    /// Parse the version string as semver.
    pub fn semver(&self) -> crate::error::Result<semver::Version> {
        super::version::parse_extension_version(&self.version, &self.id)
    }

    /// Get file extensions this extension provides (empty if not specified).
    pub fn provided_file_extensions(&self) -> &[String] {
        self.provides
            .as_ref()
            .map(|p| p.file_extensions.as_slice())
            .unwrap_or(&[])
    }

    /// Get capabilities this extension provides (empty if not specified).
    pub fn provided_capabilities(&self) -> &[String] {
        self.provides
            .as_ref()
            .map(|p| p.capabilities.as_slice())
            .unwrap_or(&[])
    }

    /// Check if this extension handles a given file extension.
    pub fn handles_file_extension(&self, ext: &str) -> bool {
        self.provided_file_extensions().iter().any(|e| e == ext)
    }

    /// Get the fingerprint script path (relative to extension dir), if configured.
    pub fn fingerprint_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.fingerprint.as_deref())
    }

    /// Get the refactor script path (relative to extension dir), if configured.
    pub fn refactor_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.refactor.as_deref())
    }

    /// Get the topology script path (relative to extension dir), if configured.
    pub fn topology_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.topology.as_deref())
    }

    /// Get the validate script path (relative to extension dir), if configured.
    pub fn validate_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.validate.as_deref())
    }

    /// Get the format script path (relative to extension dir), if configured.
    pub fn format_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.format.as_deref())
    }

    /// Get the contract script path (relative to extension dir), if configured.
    pub fn contract_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.contract.as_deref())
    }
}

// ============================================================================
// Sub-structs (unchanged from original)
// ============================================================================
