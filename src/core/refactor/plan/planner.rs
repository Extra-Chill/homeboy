mod collect;
mod fix_accumulator;
mod stage;
mod summarize_audit_fix;
mod types;

pub use collect::{collect_fix_proposals, collect_stage_changed_files};
pub use fix_accumulator::{FixAccumulator, FixAccumulator};
pub use stage::{run_lint_stage, run_test_stage};
pub use summarize_audit_fix::{
    collect_audit_changed_files,
    format_changed_files,
    plan_audit_stage,
    summarize_audit_fix_result_entries,
};
pub use types::{
    FixProposal,
    LintSourceOptions,
    OUTPUT_DIR_ENV,
    PlanOverlap,
    PlanStageSummary,
    PlanTotals,
    PlannedStage,
    RefactorPlan,
    RefactorPlanRequest,
    TestSourceOptions,
};

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

pub const KNOWN_PLAN_SOURCES: &[&str] = &["audit", "lint", "test"];

pub fn lint_refactor_request(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> RefactorPlanRequest {
    RefactorPlanRequest {
        component,
        root,
        sources: vec!["lint".to_string()],
        changed_since: None,
        only: Vec::new(),
        exclude: Vec::new(),
        settings,
        lint: options,
        test: TestSourceOptions::default(),
        write,
        force: false,
    }
}

pub fn test_refactor_request(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: TestSourceOptions,
    write: bool,
) -> RefactorPlanRequest {
    RefactorPlanRequest {
        component,
        root,
        sources: vec!["test".to_string()],
        changed_since: None,
        only: Vec::new(),
        exclude: Vec::new(),
        settings,
        lint: LintSourceOptions::default(),
        test: options,
        write,
        force: false,
    }
}

pub fn run_lint_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> crate::Result<RefactorPlan> {
    build_refactor_plan(lint_refactor_request(
        component, root, settings, options, write,
    ))
}

pub fn run_test_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: TestSourceOptions,
    write: bool,
) -> crate::Result<RefactorPlan> {
    build_refactor_plan(test_refactor_request(
        component, root, settings, options, write,
    ))
}

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

pub fn build_refactor_plan(request: RefactorPlanRequest) -> crate::Result<RefactorPlan> {
    let sources = normalize_sources(&request.sources)?;
    let root_str = request.root.to_string_lossy().to_string();
    let original_changes = git::get_uncommitted_changes(&root_str).ok();

    // Refuse to write to a dirty working tree unless --force is set.
    // Refactoring operates directly on the working tree, so mixing auto-generated
    // fixes with uncommitted manual changes makes rollback difficult.
    // Dry runs (no --write) are always safe — they don't modify files.
    if request.write && !request.force {
        if let Some(ref changes) = original_changes {
            if changes.has_changes {
                return Err(crate::Error::validation_invalid_argument(
                    "write",
                    "Working tree has uncommitted changes",
                    None,
                    Some(vec![
                        "Commit or stash your changes first".to_string(),
                        "Or use --force to proceed anyway".to_string(),
                    ]),
                ));
            }
        }
    }

    let scoped_changed_files = if let Some(git_ref) = request.changed_since.as_deref() {
        Some(git::get_files_changed_since(&root_str, git_ref)?)
    } else {
        None
    };
    let scoped_test_files = if let Some(git_ref) = request.changed_since.as_deref() {
        Some(compute_changed_test_files(&request.component, git_ref)?)
    } else {
        None
    };

    let mut planned_stages = Vec::new();
    let merge_order = sources.join(" → ");
    let mut warnings = vec![format!("Deterministic merge order: {}", merge_order)];
    let mut accumulator = FixAccumulator::default();

    // Save undo snapshot before any modifications so we can roll back.
    if request.write {
        let mut snapshot_files: HashSet<String> = HashSet::new();
        if let Some(changes) = &original_changes {
            snapshot_files.extend(changes.staged.iter().cloned());
            snapshot_files.extend(changes.unstaged.iter().cloned());
            snapshot_files.extend(changes.untracked.iter().cloned());
        }
        if !snapshot_files.is_empty() {
            let mut snap = UndoSnapshot::new(&request.root, "refactor sources (pre)");
            for file in &snapshot_files {
                snap.capture_file(file);
            }
            if let Err(e) = snap.save() {
                crate::log_status!(
                    "undo",
                    "Warning: failed to save pre-refactor undo snapshot: {}",
                    e
                );
            }
        }
    }

    let run_dir = RunDir::create()?;

    for source in &sources {
        let stage = match source.as_str() {
            "audit" => plan_audit_stage(
                &request.component.id,
                &request.root,
                scoped_changed_files.as_deref(),
                &request.only,
                &request.exclude,
                request.write,
            )?,
            "lint" => run_lint_stage(
                &request.component,
                &request.root,
                &request.settings,
                &request.lint,
                scoped_changed_files.as_deref(),
                request.write,
                &run_dir,
            )?,
            "test" => run_test_stage(
                &request.component,
                &request.root,
                &request.settings,
                &request.test,
                scoped_test_files.as_deref(),
                request.write,
                &run_dir,
            )?,
            _ => unreachable!("sources are normalized before planning"),
        };

        // Format generated/modified files so subsequent stages (especially lint)
        // see properly formatted code.
        if stage.summary.files_modified > 0 {
            format_changed_files(&request.root, &stage.summary.changed_files, &mut warnings);
        }

        accumulator.extend(stage.fix_results.clone());
        planned_stages.push(stage);
    }

    let proposals = collect_fix_proposals(&planned_stages);
    let mut stage_summaries: Vec<PlanStageSummary> = planned_stages
        .into_iter()
        .map(|stage| stage.summary)
        .collect();
    let changed_files = collect_stage_changed_files(&stage_summaries);
    let overlaps = analyze_stage_overlaps(&stage_summaries);
    if !overlaps.is_empty() {
        warnings.push(format!(
            "{} staged file overlap(s) resolved by precedence order {}",
            overlaps.len(),
            merge_order
        ));
    }

    let plan_totals = summarize_plan_totals(&stage_summaries, changed_files.len());
    let files_modified = changed_files.len();
    let applied = request.write && files_modified > 0;

    if applied {
        let abs_changed: Vec<PathBuf> =
            changed_files.iter().map(|f| request.root.join(f)).collect();
        match crate::engine::format_write::format_after_write(&request.root, &abs_changed) {
            Ok(fmt) => {
                if let Some(cmd) = &fmt.command {
                    if !fmt.success {
                        warnings.push(format!("Formatter ({}) exited non-zero", cmd));
                    }
                }
            }
            Err(e) => {
                crate::log_status!("format", "Warning: post-write format failed: {}", e);
            }
        }
    }

    for stage in &mut stage_summaries {
        stage.applied = request.write && stage.files_modified > 0;
    }

    if files_modified == 0 {
        warnings.push("No automated fixes accumulated across audit/lint/test".to_string());
    }

    let hints = if applied {
        sources
            .iter()
            .map(|source| format!("Re-run checks: homeboy {} {}", source, request.component.id))
            .collect()
    } else if files_modified > 0 {
        vec!["Dry run. Re-run with --write to apply fixes to the working tree.".to_string()]
    } else {
        Vec::new()
    };

    Ok(RefactorPlan {
        component_id: request.component.id,
        source_path: root_str,
        sources,
        dry_run: !request.write,
        applied,
        merge_strategy: "sequential_source_merge".to_string(),
        proposals,
        stages: stage_summaries,
        plan_totals,
        overlaps,
        files_modified,
        changed_files,
        fix_summary: accumulator.summary(),
        warnings,
        hints,
    })
}

pub fn normalize_sources(sources: &[String]) -> crate::Result<Vec<String>> {
    let lowered: Vec<String> = sources.iter().map(|source| source.to_lowercase()).collect();

    if lowered.iter().any(|source| source == "all") {
        return Ok(KNOWN_PLAN_SOURCES
            .iter()
            .map(|source| source.to_string())
            .collect());
    }

    let unknown: Vec<String> = lowered
        .iter()
        .filter(|source| !KNOWN_PLAN_SOURCES.contains(&source.as_str()))
        .cloned()
        .collect();

    if !unknown.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from",
            format!("Unknown refactor source(s): {}", unknown.join(", ")),
            None,
            Some(vec![format!(
                "Known sources: {}",
                KNOWN_PLAN_SOURCES.join(", ")
            )]),
        ));
    }

    let mut ordered = Vec::new();
    for known in KNOWN_PLAN_SOURCES {
        if lowered.iter().any(|source| source == known) {
            ordered.push((*known).to_string());
        }
    }

    if ordered.is_empty() {
        return Err(Error::validation_missing_argument(vec!["from".to_string()]));
    }

    Ok(ordered)
}

/// Try to load a cached audit result from a previous `homeboy audit` run.
///
/// Checks `HOMEBOY_OUTPUT_DIR/audit.json` for a `CliResponse<CodeAuditResult>`
/// envelope. If found and parseable, returns the `CodeAuditResult` without
/// re-running the audit. This avoids redundant full-codebase scans when the
/// refactor step runs after an audit gate that already produced the results.
///
/// Returns `None` if:
/// - `HOMEBOY_OUTPUT_DIR` is not set
/// - The file doesn't exist
/// - The file can't be parsed (e.g. the audit failed and wrote an error envelope)
fn try_load_cached_audit() -> Option<CodeAuditResult> {
    let output_dir = std::env::var(OUTPUT_DIR_ENV).ok()?;
    let audit_file = PathBuf::from(&output_dir).join("audit.json");

    let content = std::fs::read_to_string(&audit_file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Only use cached results from successful runs
    if json.get("success")?.as_bool()? != true {
        return None;
    }

    // The `--output` envelope wraps the audit in a `data` field
    let data = json.get("data")?;
    let result: CodeAuditResult = serde_json::from_value(data.clone()).ok()?;

    crate::log_status!(
        "refactor",
        "Using cached audit result ({} findings from {})",
        result.findings.len(),
        audit_file.display()
    );

    Some(result)
}

pub fn analyze_stage_overlaps(stages: &[PlanStageSummary]) -> Vec<PlanOverlap> {
    let mut overlaps = Vec::new();

    for (later_index, later_stage) in stages.iter().enumerate() {
        if later_stage.changed_files.is_empty() {
            continue;
        }

        let later_files: BTreeSet<&str> = later_stage
            .changed_files
            .iter()
            .map(String::as_str)
            .collect();

        for earlier_stage in stages.iter().take(later_index) {
            if earlier_stage.changed_files.is_empty() {
                continue;
            }

            for file in earlier_stage.changed_files.iter().map(String::as_str) {
                if later_files.contains(file) {
                    overlaps.push(PlanOverlap {
                        file: file.to_string(),
                        earlier_stage: earlier_stage.stage.clone(),
                        later_stage: later_stage.stage.clone(),
                        resolution: format!(
                            "{} pass ran after {} in pipeline sequence",
                            later_stage.stage, earlier_stage.stage
                        ),
                    });
                }
            }
        }
    }

    overlaps.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.earlier_stage.cmp(&b.earlier_stage))
            .then(a.later_stage.cmp(&b.later_stage))
    });

    overlaps
}

pub fn summarize_plan_totals(
    stages: &[PlanStageSummary],
    total_files_selected: usize,
) -> PlanTotals {
    PlanTotals {
        stages_with_proposals: stages
            .iter()
            .filter(|stage| stage.fixes_proposed > 0)
            .count(),
        total_fixes_proposed: stages.iter().map(|stage| stage.fixes_proposed).sum(),
        total_files_selected,
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
