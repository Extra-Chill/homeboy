//! Code audit system for convention detection, drift analysis, and structural complexity.
//!
//! Scans source code to discover structural conventions, detect outliers,
//! report architectural drift, and flag structural issues (god files, high item counts).
//! Works by:
//!
//! 1. Fingerprinting source files (extract methods, registrations, types)
//! 2. Grouping files by directory and language
//! 3. Discovering conventions (patterns most files follow)
//! 4. Checking all files against discovered conventions
//! 5. Producing actionable findings for outliers
//! 6. Analyzing structural complexity (god files, high item counts)

mod documentation_drift_detection;
mod public_api;
mod reference_dependency_fingerprinting;
mod types;

pub use documentation_drift_detection::*;
pub use public_api::*;
pub use reference_dependency_fingerprinting::*;
pub use types::*;


pub mod baseline;
mod checks;
pub mod codebase_map;
mod comment_hygiene;
pub mod compare;
mod compiler_warnings;
pub(crate) mod conventions;
pub(crate) mod core_fingerprint;
mod dead_code;
mod discovery;
pub mod docs;
pub mod docs_audit;
mod duplication;
mod field_patterns;
mod findings;
pub mod fingerprint;
pub(crate) mod impact;
pub(crate) mod import_matching;
mod layer_ownership;
pub(crate) mod naming;
pub mod report;
pub mod run;
mod shadow_modules;
mod signatures;
mod structural;
mod test_coverage;
pub(crate) mod test_mapping;
mod test_topology;
pub(crate) mod walker;
mod wrapper_inference;

#[cfg(test)]
pub(crate) mod test_helpers;

use std::path::Path;

use self::layer_ownership::run as run_layer_ownership;

pub use checks::{CheckResult, CheckStatus};
pub use compare::{
    finding_fingerprint, score_delta, weighted_finding_score_with, AuditConvergenceScoring,
};
pub use conventions::{AuditFinding, Convention, Deviation, Language, Outlier};
pub use duplication::DuplicateGroup;
pub use findings::{Finding, Severity};
pub use fingerprint::FileFingerprint;
pub use report::AuditCommandOutput;
pub use run::{run_main_audit_workflow, AuditRunWorkflowArgs, AuditRunWorkflowResult};
pub use walker::is_test_path;

use crate::{component, is_zero, Result};

// ============================================================================
// Public API
// ============================================================================

// ============================================================================
// Documentation drift detection
// ============================================================================

// ============================================================================
// Reference dependency fingerprinting
// ============================================================================

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn audit_nonexistent_path_returns_error() {
        let result = audit_path("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }

    #[test]
    fn audit_empty_directory_returns_clean() {
        let dir = std::env::temp_dir().join("homeboy_audit_test_empty");
        let _ = fs::create_dir_all(&dir);

        let result = audit_path(dir.to_str().unwrap()).unwrap();
        assert_eq!(result.summary.files_scanned, 0);
        assert!(result.summary.alignment_score.is_none());
        assert!(result.conventions.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_analyze_layer_ownership() {
        let dir = std::env::temp_dir().join("homeboy_audit_layer_test");
        let _ = fs::create_dir_all(dir.join("inc/Core/Steps"));

        fs::write(
            dir.join("homeboy.json"),
            r#"{
              "audit_rules": {
                "layer_rules": [
                  {
                    "name": "engine-owns-terminal-status",
                    "forbid": {
                      "glob": "inc/Core/Steps/**/*.php",
                      "patterns": ["JobStatus::"]
                    },
                    "allow": {"glob": "inc/Abilities/Engine/**/*.php"}
                  }
                ]
              }
            }"#,
        )
        .unwrap();

        fs::write(
            dir.join("inc/Core/Steps/agent_ping.php"),
            "<?php\n$status = JobStatus::FAILED;\n",
        )
        .unwrap();

        let findings = layer_ownership::run(&dir);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].convention, "layer_ownership");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "Requires PHP extension with fingerprint script installed"]
    fn audit_directory_with_convention() {
        let dir = std::env::temp_dir().join("homeboy_audit_test_conv");
        let steps = dir.join("steps");
        let _ = fs::create_dir_all(&steps);

        // Create 3 files: 2 follow pattern, 1 is an outlier
        fs::write(
            steps.join("step_a.php"),
            r#"<?php
class StepA {
    public function register() {}
    public function validate($input) {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        fs::write(
            steps.join("step_b.php"),
            r#"<?php
class StepB {
    public function register() {}
    public function validate($input) {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        fs::write(
            steps.join("step_c.php"),
            r#"<?php
class StepC {
    public function register() {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        let result = audit_path(dir.to_str().unwrap()).unwrap();

        assert_eq!(result.summary.files_scanned, 3);
        assert!(result.summary.conventions_detected >= 1);
        assert!(result.summary.outliers_found >= 1);
        assert!(result.summary.alignment_score.unwrap() < 1.0);

        // Find the steps convention
        let steps_conv = result
            .conventions
            .iter()
            .find(|c| c.name == "Steps")
            .expect("Should find Steps convention");

        assert_eq!(steps_conv.total_files, 3);
        assert!(steps_conv
            .expected_methods
            .contains(&"register".to_string()));
        assert!(steps_conv.expected_methods.contains(&"execute".to_string()));
        assert_eq!(steps_conv.outliers.len(), 1);
        assert!(steps_conv.outliers[0].file.contains("step_c"));

        // Should have findings for the outlier
        assert!(!result.findings.is_empty());
        assert!(result
            .findings
            .iter()
            .any(|f| f.file.contains("step_c") && f.description.contains("validate")));

        let _ = fs::remove_dir_all(&dir);
    }
}
