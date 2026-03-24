mod collect;
mod fix_accumulator;
mod helpers;
mod lint_refactor_request;
mod refactor;
mod stage;
mod summarize_audit_fix;
mod types;

pub use collect::*;
pub use fix_accumulator::*;
pub use helpers::*;
pub use lint_refactor_request::*;
pub use refactor::*;
pub use stage::*;
pub use summarize_audit_fix::*;
pub use types::*;

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

    #[test]
    fn test_lint_refactor_request_default_path() {
        let component = Default::default();
        let root = PathBuf::new();
        let settings = Vec::new();
        let options = Default::default();
        let write = false;
        let _result = lint_refactor_request(component, root, settings, options, write);
    }

    #[test]
    fn test_run_lint_refactor_default_path() {
        let component = Default::default();
        let root = PathBuf::new();
        let settings = Vec::new();
        let options = Default::default();
        let write = false;
        let _result = run_lint_refactor(component, root, settings, options, write);
    }

    #[test]
    fn test_run_test_refactor_default_path() {
        let component = Default::default();
        let root = PathBuf::new();
        let settings = Vec::new();
        let options = Default::default();
        let write = false;
        let _result = run_test_refactor(component, root, settings, options, write);
    }

    #[test]
    fn test_build_refactor_plan_default_path() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_request_write_request_force() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_let_scoped_changed_files_if_let_some_git_ref_request_changed() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_some_git_get_files_changed_since_root_str_git_ref() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_3() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_4() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_request_write() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_snapshot_files_is_empty() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        assert!(
            result.is_err(),
            "expected Err for: !snapshot_files.is_empty()"
        );
    }

    #[test]
    fn test_build_refactor_plan_default_path_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_match_crate_engine_format_write_format_after_write_request_r() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        let inner = result.unwrap();
        // Branch returns Ok(fmt) when: match crate::engine::format_write::format_after_write(&request.root, &abs_changed)
        assert_eq!(inner.component_id, String::new());
        assert_eq!(inner.source_path, String::new());
        assert_eq!(inner.sources, Vec::new());
        assert_eq!(inner.dry_run, false);
        assert_eq!(inner.applied, false);
        assert_eq!(inner.merge_strategy, String::new());
        assert_eq!(inner.proposals, Vec::new());
        assert_eq!(inner.stages, Vec::new());
        assert_eq!(inner.plan_totals, Default::default());
        assert_eq!(inner.overlaps, Vec::new());
        assert_eq!(inner.files_modified, 0);
        assert_eq!(inner.changed_files, Vec::new());
        assert_eq!(inner.fix_summary, None);
        assert_eq!(inner.warnings, Vec::new());
        assert_eq!(inner.hints, Vec::new());
    }

    #[test]
    fn test_build_refactor_plan_match_crate_engine_format_write_format_after_write_request_r_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_fmt_success() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        assert!(result.is_err(), "expected Err for: !fmt.success");
    }

    #[test]
    fn test_build_refactor_plan_has_expected_effects() {
        // Expected effects: mutation, logging
        let request = Default::default();
        let _ = build_refactor_plan(request);
    }

    #[test]
    fn test_normalize_sources_ordered_is_empty() {
        let sources = Vec::new();
        let result = normalize_sources(&sources);
        assert!(
            !result.is_empty(),
            "expected non-empty collection for: ordered.is_empty()"
        );
    }

    #[test]
    fn test_normalize_sources_ordered_is_empty_2() {
        let sources = Vec::new();
        let result = normalize_sources(&sources);
        assert!(
            !result.is_empty(),
            "expected non-empty collection for: ordered.is_empty()"
        );
    }

    #[test]
    fn test_normalize_sources_has_expected_effects() {
        // Expected effects: mutation
        let sources = Vec::new();
        let _ = normalize_sources(&sources);
    }

    #[test]
    fn test_analyze_stage_overlaps_default_path() {
        let stages = Vec::new();
        let result = analyze_stage_overlaps(&stages);
        assert!(
            !result.is_empty(),
            "expected non-empty collection for: default path"
        );
    }

    #[test]
    fn test_analyze_stage_overlaps_has_expected_effects() {
        // Expected effects: mutation
        let stages = Vec::new();
        let _ = analyze_stage_overlaps(&stages);
    }

    #[test]
    fn test_summarize_plan_totals_default_path() {
        let stages = Vec::new();
        let total_files_selected = 0;
        let _result = summarize_plan_totals(&stages, total_files_selected);
    }

    #[test]
    fn test_lint_refactor_request_default_path() {
        let component = Default::default();
        let root = PathBuf::new();
        let settings = Vec::new();
        let options = Default::default();
        let write = false;
        let _result = lint_refactor_request(component, root, settings, options, write);
    }

    #[test]
    fn test_run_lint_refactor_default_path() {

        let _result = run_lint_refactor();
    }

    #[test]
    fn test_run_test_refactor_default_path() {

        let _result = run_test_refactor();
    }

    #[test]
    fn test_build_refactor_plan_default_path() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_request_write_request_force() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_let_scoped_changed_files_if_let_some_git_ref_request_changed() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_some_git_get_files_changed_since_root_str_git_ref() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_3() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_4() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_request_write() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_snapshot_files_is_empty() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        assert!(result.is_err(), "expected Err for: !snapshot_files.is_empty()");
    }

    #[test]
    fn test_build_refactor_plan_default_path_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_match_crate_engine_format_write_format_after_write_request_r() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        let inner = result.unwrap();
        // Branch returns Ok(fmt) when: match crate::engine::format_write::format_after_write(&request.root, &abs_changed)
        assert_eq!(inner.component_id, String::new());
        assert_eq!(inner.source_path, String::new());
        assert_eq!(inner.sources, Vec::new());
        assert_eq!(inner.dry_run, false);
        assert_eq!(inner.applied, false);
        assert_eq!(inner.merge_strategy, String::new());
        assert_eq!(inner.proposals, Vec::new());
        assert_eq!(inner.stages, Vec::new());
        assert_eq!(inner.plan_totals, Default::default());
        assert_eq!(inner.overlaps, Vec::new());
        assert_eq!(inner.files_modified, 0);
        assert_eq!(inner.changed_files, Vec::new());
        assert_eq!(inner.fix_summary, None);
        assert_eq!(inner.warnings, Vec::new());
        assert_eq!(inner.hints, Vec::new());
    }

    #[test]
    fn test_build_refactor_plan_match_crate_engine_format_write_format_after_write_request_r_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_fmt_success() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        assert!(result.is_err(), "expected Err for: !fmt.success");
    }

    #[test]
    fn test_build_refactor_plan_has_expected_effects() {
        // Expected effects: mutation, logging
        let request = Default::default();
        let _ = build_refactor_plan(request);
    }

    #[test]
    fn test_normalize_sources_ordered_is_empty() {

        let result = normalize_sources();
        assert!(!result.is_empty(), "expected non-empty collection for: ordered.is_empty()");
    }

    #[test]
    fn test_normalize_sources_ordered_is_empty_2() {

        let result = normalize_sources();
        assert!(!result.is_empty(), "expected non-empty collection for: ordered.is_empty()");
    }

    #[test]
    fn test_normalize_sources_has_expected_effects() {
        // Expected effects: mutation

        let _ = normalize_sources();
    }

    #[test]
    fn test_analyze_stage_overlaps_default_path() {

        let result = analyze_stage_overlaps();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_analyze_stage_overlaps_has_expected_effects() {
        // Expected effects: mutation

        let _ = analyze_stage_overlaps();
    }

    #[test]
    fn test_summarize_plan_totals_default_path() {

        let _result = summarize_plan_totals();
    }

}
