mod collect;
mod constants;
mod fix_accumulator;
mod helpers;
mod lint_refactor_request;
mod plan;
mod stage;
mod test_refactor_request;
mod try_load_cached;
mod types;

pub use collect::*;
pub use constants::*;
pub use fix_accumulator::*;
pub use helpers::*;
pub use lint_refactor_request::*;
pub use plan::*;
pub use stage::*;
pub use test_refactor_request::*;
pub use try_load_cached::*;
pub use types::*;

use crate::code_audit::CodeAuditResult;
use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::extension;
use crate::extension::test::compute_changed_test_files;
use crate::git;
use crate::refactor::auto as fixer;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use crate::Error;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use super::verify::AuditConvergenceScoring;

impl FixAccumulator {
    fn extend(&mut self, items: Vec<FixApplied>) {
        self.fixes.extend(items);
    }

    fn summary(&self) -> Option<FixResultsSummary> {
        if self.fixes.is_empty() {
            None
        } else {
            Some(auto::summarize_fix_results(&self.fixes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-refactor-planner-{name}-{nanos}"))
    }

    fn test_component(root: &Path) -> Component {
        Component {
            id: "component".to_string(),
            local_path: root.to_string_lossy().to_string(),
            remote_path: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn analyze_stage_overlaps_reports_later_stage_precedence() {
        let stages = vec![
            PlanStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 2,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "test".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: None,
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let overlaps = analyze_stage_overlaps(&stages);

        assert_eq!(
            overlaps,
            vec![
                PlanOverlap {
                    file: "src/lib.rs".to_string(),
                    earlier_stage: "audit".to_string(),
                    later_stage: "lint".to_string(),
                    resolution: "lint pass ran after audit in pipeline sequence".to_string(),
                },
                PlanOverlap {
                    file: "src/main.rs".to_string(),
                    earlier_stage: "lint".to_string(),
                    later_stage: "test".to_string(),
                    resolution: "test pass ran after lint in pipeline sequence".to_string(),
                },
            ]
        );
    }

    #[test]
    fn analyze_stage_overlaps_ignores_disjoint_files() {
        let stages = vec![
            PlanStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        assert!(analyze_stage_overlaps(&stages).is_empty());
    }

    #[test]
    fn summarize_plan_totals_counts_stage_and_fix_totals() {
        let stages = vec![
            PlanStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 2,
                files_modified: 1,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 0,
                files_modified: 0,
                detected_findings: Some(1),
                changed_files: Vec::new(),
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "test".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 3,
                files_modified: 2,
                detected_findings: None,
                changed_files: vec!["tests/foo.rs".to_string(), "tests/bar.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let totals = summarize_plan_totals(&stages, 3);

        assert_eq!(totals.stages_with_proposals, 2);
        assert_eq!(totals.total_fixes_proposed, 5);
        assert_eq!(totals.total_files_selected, 3);
    }

    #[test]
    fn build_refactor_plan_audit_write_uses_audit_refactor_engine() {
        let root = tmp_dir("audit-write");
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        let component = test_component(&root);
        let plan = build_refactor_plan(RefactorPlanRequest {
            component,
            root: root.clone(),
            sources: vec!["audit".to_string()],
            changed_since: None,
            only: vec![crate::code_audit::AuditFinding::DuplicateFunction],
            exclude: vec![],
            settings: vec![],
            lint: LintSourceOptions::default(),
            test: TestSourceOptions::default(),
            write: true,
            force: false,
        })
        .unwrap();

        let audit_stage = plan
            .stages
            .iter()
            .find(|stage| stage.stage == "audit")
            .expect("audit stage present");

        assert!(audit_stage.applied);
        assert!(audit_stage.files_modified > 0);
        assert!(!audit_stage.changed_files.is_empty());
        assert!(plan
            .proposals
            .iter()
            .any(|proposal| proposal.source == "audit"));
        assert!(audit_stage
            .warnings
            .iter()
            .any(|warning| warning.starts_with("audit iteration ")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn try_load_cached_audit_reads_output_dir() {
        let dir = tmp_dir("cached-audit");
        fs::create_dir_all(&dir).unwrap();
        let audit_result = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: "/tmp/test".to_string(),
            summary: crate::code_audit::AuditSummary {
                files_scanned: 10,
                conventions_detected: 2,
                outliers_found: 1,
                alignment_score: None,
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![],
            duplicate_groups: vec![],
        };

        // Write a CliResponse envelope
        let envelope = serde_json::json!({
            "success": true,
            "data": audit_result,
        });
        fs::write(
            dir.join("audit.json"),
            serde_json::to_string_pretty(&envelope).unwrap(),
        )
        .unwrap();

        // Set the env var and load
        std::env::set_var(OUTPUT_DIR_ENV, dir.to_string_lossy().as_ref());
        let loaded = try_load_cached_audit();
        std::env::remove_var(OUTPUT_DIR_ENV);

        let loaded = loaded.expect("should load cached audit");
        assert_eq!(loaded.component_id, "test");
        assert_eq!(loaded.summary.files_scanned, 10);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn try_load_cached_audit_skips_failed_envelope() {
        let dir = tmp_dir("cached-audit-fail");
        fs::create_dir_all(&dir).unwrap();
        let envelope = serde_json::json!({
            "success": false,
            "error": {
                "code": "internal.io_error",
                "message": "something broke",
                "details": {},
            },
        });
        fs::write(
            dir.join("audit.json"),
            serde_json::to_string_pretty(&envelope).unwrap(),
        )
        .unwrap();

        std::env::set_var(OUTPUT_DIR_ENV, dir.to_string_lossy().as_ref());
        let loaded = try_load_cached_audit();
        std::env::remove_var(OUTPUT_DIR_ENV);

        assert!(loaded.is_none(), "should not use failed audit result");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn try_load_cached_audit_returns_none_when_unset() {
        std::env::remove_var(OUTPUT_DIR_ENV);
        assert!(try_load_cached_audit().is_none());
    }

    #[test]
    fn normalize_sources_orders_known_sources() {
        let normalized =
            normalize_sources(&["test".to_string(), "audit".to_string(), "lint".to_string()])
                .expect("sources should normalize");

        assert_eq!(normalized, vec!["audit", "lint", "test"]);
    }

    #[test]
    fn normalize_sources_rejects_unknown_sources() {
        let err =
            normalize_sources(&["weird".to_string()]).expect_err("unknown source should fail");
        assert!(err.to_string().contains("Unknown refactor source"));
    }
}
