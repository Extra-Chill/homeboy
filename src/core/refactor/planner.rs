use crate::code_audit::fixer;
use crate::code_audit::is_test_path;
use crate::component::Component;
use crate::extension;
use crate::git;
use crate::lint_baseline;
use crate::test_drift::{self, DriftOptions};
use crate::undo::UndoSnapshot;
use crate::utils::autofix::{self, FixApplied, FixResultsSummary};
use crate::Error;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use super::sandbox::{
    clone_tree, copy_changed_files, diff_tree_snapshots, snapshot_tree, SandboxDir,
};

pub const KNOWN_PLAN_SOURCES: &[&str] = &["audit", "lint", "test"];

#[derive(Debug, Clone)]
pub struct RefactorPlanRequest {
    pub component: Component,
    pub root: PathBuf,
    pub sources: Vec<String>,
    pub changed_since: Option<String>,
    pub only: Vec<crate::code_audit::AuditFinding>,
    pub exclude: Vec<crate::code_audit::AuditFinding>,
    pub settings: Vec<(String, String)>,
    pub write: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefactorPlan {
    pub component_id: String,
    pub source_path: String,
    pub sources: Vec<String>,
    pub dry_run: bool,
    pub applied: bool,
    pub merge_strategy: String,
    pub proposals: Vec<FixProposal>,
    pub stages: Vec<PlanStageSummary>,
    pub plan_totals: PlanTotals,
    pub overlaps: Vec<PlanOverlap>,
    pub files_modified: usize,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    pub warnings: Vec<String>,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanStageSummary {
    pub stage: String,
    pub planned: bool,
    pub applied: bool,
    pub fixes_proposed: usize,
    pub files_modified: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_findings: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanOverlap {
    pub file: String,
    pub earlier_stage: String,
    pub later_stage: String,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanTotals {
    pub stages_with_proposals: usize,
    pub total_fixes_proposed: usize,
    pub total_files_selected: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FixProposal {
    pub source: String,
    pub file: String,
    pub rule_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Default)]
struct FixAccumulator {
    fixes: Vec<FixApplied>,
}

impl FixAccumulator {
    fn extend(&mut self, items: Vec<FixApplied>) {
        self.fixes.extend(items);
    }

    fn summary(&self) -> Option<FixResultsSummary> {
        if self.fixes.is_empty() {
            None
        } else {
            Some(autofix::summarize_fix_results(&self.fixes))
        }
    }
}

struct PlannedStage {
    source: String,
    summary: PlanStageSummary,
    fix_results: Vec<FixApplied>,
}

pub fn build_refactor_plan(request: RefactorPlanRequest) -> crate::Result<RefactorPlan> {
    let sources = normalize_sources(&request.sources)?;
    let root_str = request.root.to_string_lossy().to_string();
    let original_changes = git::get_uncommitted_changes(&root_str).ok();
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

    let working_root = clone_tree(&request.root)?;

    for source in &sources {
        let stage = match source.as_str() {
            "audit" => plan_audit_stage(
                &request.component.id,
                &request.root,
                scoped_changed_files.as_deref(),
                &request.only,
                &request.exclude,
                Some(working_root.path()),
                true,
            )?,
            "lint" => run_lint_stage(
                &request.component,
                &working_root,
                &request.settings,
                scoped_changed_files.as_deref(),
                true,
            )?,
            "test" => run_test_stage(
                &request.component,
                &working_root,
                &request.settings,
                scoped_test_files.as_deref(),
                true,
            )?,
            _ => unreachable!("sources are normalized before planning"),
        };

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

    if request.write && applied {
        let mut snapshot_files: HashSet<String> = changed_files.iter().cloned().collect();
        if let Some(changes) = &original_changes {
            snapshot_files.extend(changes.staged.iter().cloned());
            snapshot_files.extend(changes.unstaged.iter().cloned());
            snapshot_files.extend(changes.untracked.iter().cloned());
        }

        if !snapshot_files.is_empty() {
            let mut snap = UndoSnapshot::new(&request.root, "refactor ci");
            for file in &snapshot_files {
                snap.capture_file(file);
            }
            if let Err(e) = snap.save() {
                crate::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
            }
        }

        copy_changed_files(working_root.path(), &request.root, &changed_files)?;
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
        vec![
            "Plan only. Sandbox passes were used to accumulate fix proposals without touching the real tree. Re-run with --write to apply them.".to_string(),
        ]
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

fn normalize_sources(sources: &[String]) -> crate::Result<Vec<String>> {
    let lowered: Vec<String> = sources.iter().map(|source| source.to_lowercase()).collect();
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

fn collect_fix_proposals(stages: &[PlannedStage]) -> Vec<FixProposal> {
    let mut proposals = Vec::new();

    for stage in stages {
        for fix in &stage.fix_results {
            proposals.push(FixProposal {
                source: stage.source.clone(),
                file: fix.file.clone(),
                rule_id: fix.rule.clone(),
                action: fix.action.clone(),
            });
        }
    }

    proposals.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.file.cmp(&b.file))
            .then(a.rule_id.cmp(&b.rule_id))
    });

    proposals
}

fn compute_changed_test_files(component: &Component, git_ref: &str) -> crate::Result<Vec<String>> {
    let source_path = std::path::PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());
    let changed_files = git::get_files_changed_since(&source_path.to_string_lossy(), git_ref)?;

    let opts = if source_path.join("Cargo.toml").exists() {
        DriftOptions::rust(&source_path, git_ref)
    } else {
        DriftOptions::php(&source_path, git_ref)
    };

    let report = test_drift::detect_drift(&component.id, &opts)?;
    let mut selected: BTreeSet<String> = BTreeSet::new();

    for file in &changed_files {
        if is_test_path(file) {
            selected.insert(file.clone());
        }
    }

    for drifted in &report.drifted_tests {
        selected.insert(drifted.test_file.clone());
    }

    Ok(selected.into_iter().collect())
}

fn collect_stage_changed_files(stages: &[PlanStageSummary]) -> Vec<String> {
    let mut final_changed_files = BTreeSet::new();
    for stage in stages {
        for file in &stage.changed_files {
            final_changed_files.insert(file.clone());
        }
    }
    final_changed_files.into_iter().collect()
}

fn plan_audit_stage(
    component_id: &str,
    root: &Path,
    changed_files: Option<&[String]>,
    only: &[crate::code_audit::AuditFinding],
    exclude: &[crate::code_audit::AuditFinding],
    apply_root: Option<&Path>,
    write: bool,
) -> crate::Result<PlannedStage> {
    let result = if let Some(changed) = changed_files {
        if changed.is_empty() {
            crate::code_audit::CodeAuditResult {
                component_id: component_id.to_string(),
                source_path: root.to_string_lossy().to_string(),
                summary: crate::code_audit::AuditSummary {
                    files_scanned: 0,
                    conventions_detected: 0,
                    outliers_found: 0,
                    alignment_score: None,
                    files_skipped: 0,
                    warnings: vec![],
                },
                conventions: vec![],
                directory_conventions: vec![],
                findings: vec![],
                duplicate_groups: vec![],
            }
        } else {
            crate::code_audit::audit_path_scoped(
                component_id,
                &root.to_string_lossy(),
                changed,
                None,
            )?
        }
    } else {
        crate::code_audit::audit_path_with_id(component_id, &root.to_string_lossy())?
    };

    let mut fix_result = fixer::generate_fixes(&result, root);
    let policy = fixer::FixPolicy {
        only: (!only.is_empty()).then_some(only.to_vec()),
        exclude: exclude.to_vec(),
    };
    let preflight_context = fixer::PreflightContext { root };
    let policy_summary =
        fixer::apply_fix_policy(&mut fix_result, write, &policy, &preflight_context);

    let changed_files = collect_audit_changed_files(&fix_result);
    let fix_results = summarize_audit_fix_result_entries(&fix_result);
    let fixes_proposed = fix_results.len();

    if write {
        if let Some(apply_root) = apply_root {
            if !fix_result.fixes.is_empty() {
                fixer::apply_fixes(&mut fix_result.fixes, apply_root);
            }
            if !fix_result.new_files.is_empty() {
                fixer::apply_new_files(&mut fix_result.new_files, apply_root);
            }
        }
    }

    Ok(PlannedStage {
        source: "audit".to_string(),
        summary: PlanStageSummary {
            stage: "audit".to_string(),
            planned: true,
            applied: write && !changed_files.is_empty(),
            fixes_proposed,
            files_modified: changed_files.len(),
            detected_findings: Some(result.findings.len()),
            changed_files,
            fix_summary: if policy_summary.visible_insertions + policy_summary.visible_new_files > 0
            {
                Some(autofix::summarize_audit_fix_result(&fix_result))
            } else {
                None
            },
            warnings: Vec::new(),
        },
        fix_results,
    })
}

fn run_lint_stage(
    component: &Component,
    sandbox: &SandboxDir,
    settings: &[(String, String)],
    changed_files: Option<&[String]>,
    plan_mode: bool,
) -> crate::Result<PlannedStage> {
    let mut sandbox_component = component.clone();
    sandbox_component.local_path = sandbox.path().to_string_lossy().to_string();
    let resolved = super::runner::resolve_lint_command(&sandbox_component)?;
    let findings_file = std::env::temp_dir().join(format!(
        "homeboy-lint-findings-{}-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let fix_results_file = autofix::fix_results_temp_path();
    let fix_plan_file = autofix::fix_plan_temp_path();
    let before_fix = if plan_mode {
        Some(snapshot_tree(&sandbox_component.local_path)?)
    } else {
        None
    };

    let effective_glob = if let Some(changed_files) = changed_files {
        if changed_files.is_empty() {
            None
        } else {
            let abs_files: Vec<String> = changed_files
                .iter()
                .map(|f| format!("{}/{}", sandbox_component.local_path, f))
                .collect();
            if abs_files.len() == 1 {
                Some(abs_files[0].clone())
            } else {
                Some(format!("{{{}}}", abs_files.join(",")))
            }
        }
    } else {
        None
    };

    extension::ExtensionRunner::new(&sandbox_component.id, &resolved.script_path)
        .extension_id(resolved.extension_id.clone())
        .component(sandbox_component.clone())
        .settings(settings)
        .env_if(plan_mode, "HOMEBOY_AUTO_FIX", "1")
        .env_opt("HOMEBOY_LINT_GLOB", &effective_glob)
        .env(
            "HOMEBOY_LINT_FINDINGS_FILE",
            &findings_file.to_string_lossy(),
        )
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_PLAN_FILE",
            &fix_plan_file.to_string_lossy(),
        )
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_RESULTS_FILE",
            &fix_results_file.to_string_lossy(),
        )
        .run()?;

    let changed_files = if plan_mode {
        let after_fix = snapshot_tree(&sandbox_component.local_path)?;
        before_fix
            .as_ref()
            .map(|before| diff_tree_snapshots(before, &after_fix))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let planned_fix_results = autofix::parse_fix_plan_file(&fix_plan_file);
    let fix_results = if planned_fix_results.is_empty() {
        autofix::parse_fix_results_file(&fix_results_file)
    } else {
        planned_fix_results
    };
    let fixes_proposed = fix_results.len();
    let lint_findings = lint_baseline::parse_findings_file(&findings_file).unwrap_or_default();
    let _ = std::fs::remove_file(&fix_results_file);
    let _ = std::fs::remove_file(&fix_plan_file);
    let _ = std::fs::remove_file(&findings_file);

    Ok(PlannedStage {
        source: "lint".to_string(),
        summary: PlanStageSummary {
            stage: "lint".to_string(),
            planned: true,
            applied: plan_mode && !changed_files.is_empty(),
            fixes_proposed,
            files_modified: changed_files.len(),
            detected_findings: Some(lint_findings.len()),
            changed_files,
            fix_summary: if fix_results.is_empty() {
                None
            } else {
                Some(autofix::summarize_fix_results(&fix_results))
            },
            warnings: Vec::new(),
        },
        fix_results,
    })
}

fn run_test_stage(
    component: &Component,
    sandbox: &SandboxDir,
    settings: &[(String, String)],
    changed_test_files: Option<&[String]>,
    plan_mode: bool,
) -> crate::Result<PlannedStage> {
    let mut sandbox_component = component.clone();
    sandbox_component.local_path = sandbox.path().to_string_lossy().to_string();
    let resolved = super::runner::resolve_test_command(&sandbox_component)?;
    let results_file = std::env::temp_dir().join(format!(
        "homeboy-test-results-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let fix_results_file = autofix::fix_results_temp_path();
    let fix_plan_file = autofix::fix_plan_temp_path();
    let before_fix = if plan_mode {
        Some(snapshot_tree(&sandbox_component.local_path)?)
    } else {
        None
    };

    let mut runner = extension::ExtensionRunner::new(&sandbox_component.id, &resolved.script_path)
        .extension_id(resolved.extension_id.clone())
        .component(sandbox_component.clone())
        .settings(settings)
        .env("HOMEBOY_TEST_RESULTS_FILE", &results_file.to_string_lossy())
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_PLAN_FILE",
            &fix_plan_file.to_string_lossy(),
        )
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_RESULTS_FILE",
            &fix_results_file.to_string_lossy(),
        )
        .env_if(plan_mode, "HOMEBOY_AUTO_FIX", "1");

    if let Some(changed_test_files) = changed_test_files {
        if !changed_test_files.is_empty() {
            runner = runner.env("HOMEBOY_CHANGED_TEST_FILES", &changed_test_files.join("\n"));
        }
    }

    runner.run()?;

    let changed_files = if plan_mode {
        let after_fix = snapshot_tree(&sandbox_component.local_path)?;
        before_fix
            .as_ref()
            .map(|before| diff_tree_snapshots(before, &after_fix))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let planned_fix_results = autofix::parse_fix_plan_file(&fix_plan_file);
    let fix_results = if planned_fix_results.is_empty() {
        autofix::parse_fix_results_file(&fix_results_file)
    } else {
        planned_fix_results
    };
    let fixes_proposed = fix_results.len();
    let _ = std::fs::remove_file(&fix_results_file);
    let _ = std::fs::remove_file(&fix_plan_file);
    let _ = std::fs::remove_file(&results_file);

    Ok(PlannedStage {
        source: "test".to_string(),
        summary: PlanStageSummary {
            stage: "test".to_string(),
            planned: true,
            applied: plan_mode && !changed_files.is_empty(),
            fixes_proposed,
            files_modified: changed_files.len(),
            detected_findings: None,
            changed_files,
            fix_summary: if fix_results.is_empty() {
                None
            } else {
                Some(autofix::summarize_fix_results(&fix_results))
            },
            warnings: Vec::new(),
        },
        fix_results,
    })
}

fn collect_audit_changed_files(fix_result: &fixer::FixResult) -> Vec<String> {
    let mut files = BTreeSet::new();
    for fix in &fix_result.fixes {
        if !fix.insertions.is_empty() {
            files.insert(fix.file.clone());
        }
    }
    for file in &fix_result.new_files {
        files.insert(file.file.clone());
    }
    files.into_iter().collect()
}

fn summarize_audit_fix_result_entries(fix_result: &fixer::FixResult) -> Vec<FixApplied> {
    let mut entries = Vec::new();

    for fix in &fix_result.fixes {
        for insertion in &fix.insertions {
            if insertion.auto_apply {
                entries.push(FixApplied {
                    file: fix.file.clone(),
                    rule: format!("{:?}", insertion.finding).to_lowercase(),
                    action: Some("insert".to_string()),
                });
            }
        }
    }

    for new_file in &fix_result.new_files {
        entries.push(FixApplied {
            file: new_file.file.clone(),
            rule: format!("{:?}", new_file.finding).to_lowercase(),
            action: Some("create".to_string()),
        });
    }

    entries
}

fn analyze_stage_overlaps(stages: &[PlanStageSummary]) -> Vec<PlanOverlap> {
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
                            "{} pass ran after {} in sandbox sequence",
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

fn summarize_plan_totals(stages: &[PlanStageSummary], total_files_selected: usize) -> PlanTotals {
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
                    resolution: "lint pass ran after audit in sandbox sequence".to_string(),
                },
                PlanOverlap {
                    file: "src/main.rs".to_string(),
                    earlier_stage: "lint".to_string(),
                    later_stage: "test".to_string(),
                    resolution: "test pass ran after lint in sandbox sequence".to_string(),
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
}
